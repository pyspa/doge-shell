//! Parent-owned admission, approval and lifetime for one background worker.
//! The socket carries requests, never authority. Only this host resolves and
//! admits native operations; owned command jobs remain here if the worker dies.

use super::{
    WorkerProcess,
    protocol::{Channel, ParentMessage, WorkerMessage},
};
use crate::{
    environment::{Environment, child_snapshot::ChildShellSnapshot},
    shell::Shell,
};
use anyhow::{Result, anyhow, bail};
use dsh_builtin::{
    BackgroundToolRunner,
    agent::jobs::AgentJobs,
    shell_capabilities::{ApprovalDecision, BackgroundTaskContext},
};
use dsh_types::background_task::{
    ApprovalRequest, Operation, PermissionDecision, Profile, TaskScope, TaskState, TaskStatus,
};
use parking_lot::{Condvar, Mutex};
use serde_json::{Value, json};
use std::{
    io::ErrorKind,
    net::Shutdown,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 256 * 1024;
mod approval;
mod lifecycle;
mod output;
mod owned_job;
use output::{drain, safe_output};
use owned_job::owned_job;

struct Admitted {
    name: String,
    arguments: String,
    operation: Operation,
    epoch: u64,
    approved: bool,
}
struct Pending {
    request: ApprovalRequest,
    guidance: bool,
    decision: Option<bool>,
}
struct Inner {
    task: TaskState,
    pending: Option<Pending>,
    admitted: Option<Admitted>,
    completed_ok: Option<bool>,
    tokens_reported: bool,
}
struct Shared {
    inner: Mutex<Inner>,
    changed: Condvar,
    canceled: AtomicBool,
    capture_done: AtomicBool,
    started: Instant,
    shutdown: std::os::unix::net::UnixStream,
    process: Mutex<Option<nix::unistd::Pid>>,
    jobs: Arc<Mutex<AgentJobs>>,
    output: Mutex<Vec<u8>>,
    secrets: Vec<String>,
}

fn active(status: TaskStatus) -> bool {
    matches!(status, TaskStatus::Running | TaskStatus::InputRequired)
}
impl Shared {
    fn new(
        task: TaskState,
        shutdown: std::os::unix::net::UnixStream,
        pid: Option<nix::unistd::Pid>,
        secrets: Vec<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                task,
                pending: None,
                admitted: None,
                completed_ok: None,
                tokens_reported: false,
            }),
            changed: Condvar::new(),
            canceled: AtomicBool::new(false),
            capture_done: AtomicBool::new(false),
            started: Instant::now(),
            shutdown,
            process: Mutex::new(pid),
            jobs: Arc::new(Mutex::new(AgentJobs::default())),
            output: Mutex::new(Vec::new()),
            secrets,
        })
    }
    fn elapsed(&self) -> u64 {
        self.started.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
    fn time_left(&self) -> Duration {
        let mut inner = self.inner.lock();
        inner.task.check_deadline(self.elapsed());
        Duration::from_millis(
            inner
                .task
                .scope()
                .budget
                .time_ms
                .saturating_sub(self.elapsed()),
        )
    }
    fn interrupt(&self) {
        self.canceled.store(true, Ordering::Release);
        self.changed.notify_all();
        let _ = self.shutdown.shutdown(Shutdown::Both);
        if let Some(pid) = *self.process.lock() {
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGINT);
        }
    }
    fn charge(&self, calls: u64, bytes: u64) {
        let stopped = {
            let mut inner = self.inner.lock();
            inner.task.record_usage(calls, 0, bytes, self.elapsed());
            !active(inner.task.status())
        };
        if stopped {
            self.interrupt();
        }
    }
    fn append_output(&self, bytes: &[u8]) {
        {
            let mut output = self.output.lock();
            if bytes.len() >= OUTPUT_LIMIT {
                output.clear();
                output.extend_from_slice(&bytes[bytes.len() - OUTPUT_LIMIT..]);
            } else {
                let discard = output
                    .len()
                    .saturating_add(bytes.len())
                    .saturating_sub(OUTPUT_LIMIT);
                output.drain(..discard);
                output.extend_from_slice(bytes);
            }
        }
    }
    fn redact(&self, text: &str) -> dsh_types::safety_policy::RedactedText {
        dsh_types::safety_policy::redact_sensitive_text_with_known(text, &self.secrets)
    }
    fn mask(&self, text: &str, limit: usize) -> String {
        safe_output(&self.redact(text).text, limit)
    }
    fn reap(&self) {
        if let Some(pid) = self.process.lock().take() {
            reap_worker(pid);
        }
    }
}

/// Local tool host authority. Never forwards a request back into the worker.
pub(super) struct LocalContext {
    shared: Arc<Shared>,
}
impl BackgroundTaskContext for LocalContext {
    fn remote_tool(&self, _name: &str, _args: &str) -> Option<Result<String, String>> {
        None
    }
    fn permits_tool(&self, name: &str, args: &str) -> bool {
        let mut inner = self.shared.inner.lock();
        inner.task.check_deadline(self.shared.elapsed());
        !self.shared.canceled.load(Ordering::Acquire)
            && !self.shared.worker_disconnected()
            && active(inner.task.status())
            && inner.admitted.as_ref().is_some_and(|admitted| {
                admitted.name == name
                    && serde_json::from_str::<Value>(args).ok()
                        == serde_json::from_str::<Value>(&admitted.arguments).ok()
                    && admitted.epoch == inner.task.generation()
            })
    }
    fn confirm(&self, _message: &str) -> Result<ApprovalDecision> {
        let (operation, approved) = {
            let inner = self.shared.inner.lock();
            let admitted = inner
                .admitted
                .as_ref()
                .ok_or_else(|| anyhow!("no admitted background operation"))?;
            if self.shared.canceled.load(Ordering::Acquire)
                || self.shared.worker_disconnected()
                || admitted.epoch != inner.task.generation()
                || !active(inner.task.status())
            {
                bail!("background operation expired");
            }
            (admitted.operation.clone(), admitted.approved)
        };
        if approved || await_approval(&self.shared, operation, false)? {
            Ok(ApprovalDecision::Allow)
        } else {
            Ok(ApprovalDecision::Deny)
        }
    }
    fn is_canceled(&self) -> bool {
        let mut inner = self.shared.inner.lock();
        inner.task.check_deadline(self.shared.elapsed());
        self.shared.canceled.load(Ordering::Acquire)
            || self.shared.worker_disconnected()
            || !active(inner.task.status())
            || inner
                .admitted
                .as_ref()
                .is_none_or(|admitted| admitted.epoch != inner.task.generation())
    }
    fn cwd(&self) -> PathBuf {
        self.shared.inner.lock().task.scope().cwd.clone()
    }
    fn jobs(&self) -> Option<Arc<Mutex<AgentJobs>>> {
        Some(self.shared.jobs.clone())
    }
    fn write_authorized(&self, path: &Path) -> bool {
        let (operation, epoch) = {
            let mut inner = self.shared.inner.lock();
            inner.task.check_deadline(self.shared.elapsed());
            if self.shared.canceled.load(Ordering::Acquire) || !active(inner.task.status()) {
                return false;
            }
            let Some(admitted) = inner.admitted.as_ref().filter(|admitted| {
                admitted.epoch == inner.task.generation()
                    && matches!(admitted.name.as_str(), "edit" | "str_replace")
                    && admitted
                        .operation
                        .canonical_targets
                        .iter()
                        .any(|target| target == path)
            }) else {
                return false;
            };
            (admitted.operation.clone(), admitted.epoch)
        };
        // No state lock across filesystem access. Existing task I/O refuses
        // symlinks and special files; creation requires continued absence.
        let unchanged = match &operation.preimage_digest {
            Some(expected) => {
                dsh_builtin::agent::files::read(path).is_ok_and(|current| &current == expected)
            }
            None => std::fs::symlink_metadata(path)
                .is_err_and(|error| error.kind() == ErrorKind::NotFound),
        };
        if !unchanged {
            return false;
        }
        let mut inner = self.shared.inner.lock();
        inner.task.check_deadline(self.shared.elapsed());
        !self.shared.canceled.load(Ordering::Acquire)
            && !self.shared.worker_disconnected()
            && active(inner.task.status())
            && inner.task.generation() == epoch
            && inner
                .admitted
                .as_ref()
                .is_some_and(|admitted| admitted.epoch == epoch && admitted.operation == operation)
    }
    fn read_result_authorized(&self, path: &Path, contents: &str) -> bool {
        let mut inner = self.shared.inner.lock();
        inner.task.check_deadline(self.shared.elapsed());
        !self.shared.canceled.load(Ordering::Acquire)
            && !self.shared.worker_disconnected()
            && active(inner.task.status())
            && inner.admitted.as_ref().is_some_and(|admitted| {
                admitted.epoch == inner.task.generation()
                    && matches!(admitted.name.as_str(), "read_file" | "edit" | "str_replace")
                    && admitted
                        .operation
                        .canonical_targets
                        .iter()
                        .any(|target| target == path)
                    && admitted.operation.preimage_digest.as_deref() == Some(contents)
            })
    }
    fn write_contents_authorized(&self, path: &Path, contents: &str) -> bool {
        if !self.write_authorized(path) {
            return false;
        }
        let mut inner = self.shared.inner.lock();
        inner.task.check_deadline(self.shared.elapsed());
        !self.shared.canceled.load(Ordering::Acquire)
            && !self.shared.worker_disconnected()
            && active(inner.task.status())
            && inner.admitted.as_ref().is_some_and(|admitted| {
                admitted.epoch == inner.task.generation()
                    && matches!(admitted.name.as_str(), "edit" | "str_replace")
                    && admitted
                        .operation
                        .canonical_targets
                        .iter()
                        .any(|target| target == path)
                    && admitted.operation.content_digest.as_deref() == Some(contents)
            })
    }
    fn read_authorized(&self, path: &Path) -> bool {
        let mut inner = self.shared.inner.lock();
        inner.task.check_deadline(self.shared.elapsed());
        !self.shared.canceled.load(Ordering::Acquire)
            && !self.shared.worker_disconnected()
            && active(inner.task.status())
            && inner.admitted.as_ref().is_some_and(|admitted| {
                admitted.epoch == inner.task.generation()
                    && matches!(admitted.name.as_str(), "read_file" | "ls")
                    && admitted
                        .operation
                        .canonical_targets
                        .iter()
                        .any(|target| target == path)
            })
    }
}

/// One in-memory handle, owned by the main shell. Dropping it reaps its child.
pub(crate) struct TaskHandle {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
impl TaskHandle {
    pub(super) fn launch(
        task_id: String,
        snapshot: ChildShellSnapshot,
        scope: TaskScope,
        worker: WorkerProcess,
    ) -> Result<Self> {
        let WorkerProcess {
            pid,
            channel,
            shutdown,
            stdout,
            stderr,
        } = worker;
        let task = match TaskState::new(task_id, scope) {
            Ok(task) => task,
            Err(reason) => {
                reap_worker(pid);
                return Err(anyhow!(reason));
            }
        };
        let mut secrets: Vec<_> = snapshot
            .variables
            .iter()
            .filter(|(key, value)| {
                dsh_types::safety_policy::is_sensitive_key(key)
                    && !value.is_empty()
                    && !matches!(
                        key.as_str(),
                        "AI_CHAT_SESSION_TTL_SECS"
                            | "AI_CHAT_CONTEXT_TOKEN_BUDGET"
                            | "AI_CHAT_TURN_TOKEN_BUDGET"
                    )
            })
            .map(|(_, value)| value.clone())
            .collect();
        secrets.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        secrets.dedup();
        let shared = Shared::new(task, shutdown, Some(pid), secrets);
        // The guard is captured by the closure, so failed thread creation also
        // drops it and reaps the already-spawned worker.
        let guard = WorkerGuard(shared.clone());
        let run_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("background-task".into())
            .spawn(move || {
                let _guard = guard;
                let readers: Vec<_> = [stdout, stderr]
                    .into_iter()
                    .map(|file| {
                        let shared = run_shared.clone();
                        std::thread::spawn(move || drain(file, &shared))
                    })
                    .collect();
                let outcome = run(channel, snapshot, &run_shared);
                {
                    let mut inner = run_shared.inner.lock();
                    inner.admitted = None;
                    inner.pending = None;
                    inner.completed_ok = Some(outcome.unwrap_or(false));
                    inner.task.complete(run_shared.elapsed());
                }
                run_shared.changed.notify_all();
                run_shared.jobs.lock().cancel_all();
                run_shared.reap();
                run_shared.capture_done.store(true, Ordering::Release);
                for reader in readers {
                    let _ = reader.join();
                }
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub(super) fn status(&self) -> Value {
        let mut inner = self.shared.inner.lock();
        inner.task.check_deadline(self.shared.elapsed());
        if inner
            .pending
            .as_ref()
            .is_some_and(|pending| inner.task.pending() != Some(&pending.request))
        {
            inner.pending = None;
        }
        let pending = inner
            .pending
            .as_ref()
            .map(|pending| approval::display(&self.shared, &inner, pending).0);
        json!({"task_id": inner.task.task_id(), "state": inner.task.status(), "epoch": inner.task.generation(),
            "profile": inner.task.scope().profile, "usage": inner.task.usage(), "tokens_reported": inner.tokens_reported,
            "approval": pending, "ok": inner.completed_ok})
    }
    pub(super) fn output(&self) -> String {
        self.shared.mask(
            &String::from_utf8_lossy(&self.shared.output.lock()),
            OUTPUT_LIMIT,
        )
    }
    pub(super) fn restrict_read_only(&self) -> Result<()> {
        {
            let mut inner = self.shared.inner.lock();
            let mut scope = inner.task.scope().clone();
            scope.profile = Profile::ReadOnly;
            scope.write_roots.clear();
            scope.sandbox_verified = false;
            inner
                .task
                .restrict_scope(scope, self.shared.elapsed())
                .map_err(|reason| anyhow!(reason))?;
            inner.pending = None;
            inner.admitted = None;
        }
        self.shared.changed.notify_all();
        self.shared.jobs.lock().cancel_all();
        Ok(())
    }
    pub(super) fn cancel(&mut self) {
        {
            let mut inner = self.shared.inner.lock();
            inner.task.cancel();
            inner.pending = None;
        }
        self.shared.jobs.lock().cancel_all();
        self.shared.interrupt();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
    pub(super) fn is_active(&self) -> bool {
        let mut inner = self.shared.inner.lock();
        inner.task.check_deadline(self.shared.elapsed());
        active(inner.task.status())
    }
}
impl Drop for TaskHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn await_approval(shared: &Shared, operation: Operation, guidance: bool) -> Result<bool> {
    let mut inner = shared.inner.lock();
    let request = inner
        .task
        .request_manual_approval(operation.clone(), shared.elapsed())
        .map_err(|reason| anyhow!(reason))?;
    inner.pending = Some(Pending {
        request: request.clone(),
        guidance,
        decision: None,
    });
    shared.changed.notify_all();
    loop {
        // Check the private transport even while no protocol receive is running.
        // Release authority before job cleanup to preserve the jobs/inner lock order.
        drop(inner);
        shared.cancel_disconnected();
        inner = shared.inner.lock();
        inner.task.check_deadline(shared.elapsed());
        if shared.canceled.load(Ordering::Acquire)
            || !active(inner.task.status())
            || inner.task.pending() != Some(&request)
        {
            inner.pending = None;
            bail!("background approval expired");
        }
        if let Some(allow) = inner
            .pending
            .as_mut()
            .and_then(|pending| pending.decision.take())
        {
            if !allow {
                inner.task.deny_approval();
                inner.pending = None;
                return Ok(false);
            }
            let scope = inner.task.scope().clone();
            drop(inner);
            let current = if guidance {
                Ok(operation.clone())
            } else {
                super::revalidate_operation(&operation, &scope)
            };
            inner = shared.inner.lock();
            let current = match current {
                Ok(current) => current,
                Err(reason) => {
                    inner.task.deny_approval();
                    inner.pending = None;
                    return Err(anyhow!(reason));
                }
            };
            let accepted = inner
                .task
                .consume_approval(&request, &current, shared.elapsed())
                .map_err(|reason| anyhow!(reason));
            inner.pending = None;
            accepted?;
            return Ok(true);
        }
        let remaining = inner
            .task
            .scope()
            .budget
            .time_ms
            .saturating_sub(shared.elapsed());
        shared
            .changed
            .wait_for(&mut inner, Duration::from_millis(remaining.min(50)));
    }
}

fn run(mut channel: Channel, snapshot: ChildShellSnapshot, shared: &Arc<Shared>) -> Result<bool> {
    let environment = Environment::new_isolated();
    snapshot.apply_to(&mut environment.write());
    let mut shell = Shell::new(environment);
    shell.background_context = Some(Arc::new(LocalContext {
        shared: shared.clone(),
    }));
    let runner = BackgroundToolRunner::new(&mut shell).map_err(|reason| anyhow!(reason))?;
    let mut previous_serial = 0;
    loop {
        let message = channel.receive::<WorkerMessage>(shared.time_left())?;
        match message {
            WorkerMessage::Complete { ok, tokens_used } => {
                let mut inner = shared.inner.lock();
                inner.tokens_reported = true;
                inner.task.record_usage(0, tokens_used, 0, shared.elapsed());
                return Ok(ok && active(inner.task.status()));
            }
            WorkerMessage::Tool {
                serial,
                name,
                arguments,
            } => {
                if serial <= previous_serial {
                    bail!("duplicate background request serial");
                }
                previous_serial = serial;
                shared.charge(1, 0);
                let result = tool(&name, &arguments, &runner, &mut shell, shared);
                let bytes = match &result {
                    Ok(value) | Err(value) => value.len() as u64,
                };
                shared.charge(0, bytes);
                channel.send(&ParentMessage::Reply { serial, result }, shared.time_left())?;
            }
            WorkerMessage::Confirm { serial, message } => {
                if serial <= previous_serial {
                    bail!("duplicate background request serial");
                }
                previous_serial = serial;
                // Trust in guidance never grants execution. Bind the exact
                // prompt to this epoch, but do not interpret its text as code.
                let cwd = shared.inner.lock().task.scope().cwd.clone();
                let operation = Operation {
                    tool: "read_file".into(),
                    args: json!({"guidance_trust": message, "serial": serial}),
                    canonical_targets: vec![cwd],
                    preimage_digest: None,
                    content_digest: None,
                };
                let allow = await_approval(shared, operation, true).unwrap_or(false);
                channel.send(
                    &ParentMessage::Approval { serial, allow },
                    shared.time_left(),
                )?;
            }
        }
    }
}

fn tool(
    name: &str,
    arguments: &str,
    runner: &BackgroundToolRunner,
    shell: &mut Shell,
    shared: &Arc<Shared>,
) -> Result<String, String> {
    let (scope, epoch) = {
        let mut inner = shared.inner.lock();
        inner.task.check_deadline(shared.elapsed());
        if !active(inner.task.status()) || shared.canceled.load(Ordering::Acquire) {
            return Err("background task stopped".into());
        }
        (inner.task.scope().clone(), inner.task.generation())
    };
    if matches!(name, "job_status" | "job_output" | "job_cancel") {
        return owned_job(name, arguments, shared);
    }
    let (operation, manual) = super::normalize_operation(name, arguments, &scope)?;
    let decision = shared
        .inner
        .lock()
        .task
        .evaluate(&operation, shared.elapsed());
    if (manual || matches!(decision, PermissionDecision::InputRequired))
        && operation
            .content_digest
            .as_ref()
            .is_some_and(|text| text.len() > 2048)
    {
        return Err("manual background edit exceeds the complete approval preview limit".into());
    }
    let approved = match decision {
        PermissionDecision::Denied(reason) => return Err(reason.into()),
        PermissionDecision::InputRequired => {
            await_approval(shared, operation.clone(), false).map_err(|error| error.to_string())?
        }
        PermissionDecision::AutoAllow if manual => {
            await_approval(shared, operation.clone(), false).map_err(|error| error.to_string())?
        }
        PermissionDecision::AutoAllow => false,
    };
    if (matches!(decision, PermissionDecision::InputRequired) || manual) && !approved {
        return Err("background operation denied".into());
    }
    let current_scope = shared.inner.lock().task.scope().clone();
    let current = super::revalidate_operation(&operation, &current_scope)?;
    if current != operation {
        return Err("background operation changed after admission".into());
    }
    let normalized_arguments =
        serde_json::to_string(&operation.args).map_err(|_| "invalid normalized operation")?;
    {
        let mut inner = shared.inner.lock();
        if !active(inner.task.status()) || shared.canceled.load(Ordering::Acquire) {
            return Err("background task stopped".into());
        }
        // Scope restrictions can race normalization; never carry a grant from
        // an old epoch into the actual dispatcher.
        if &current_scope != inner.task.scope() || inner.task.generation() != epoch {
            return Err("background scope changed".into());
        }
        inner.admitted = Some(Admitted {
            name: name.into(),
            arguments: normalized_arguments.clone(),
            operation,
            epoch: inner.task.generation(),
            approved,
        });
    }
    let result = runner.execute(name, &normalized_arguments, shell);
    shared.inner.lock().admitted = None;
    result
}

struct WorkerGuard(Arc<Shared>);
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        {
            let mut inner = self.0.inner.lock();
            inner.task.cancel();
            inner.pending = None;
            inner.admitted = None;
        }
        self.0.jobs.lock().cancel_all();
        self.0.interrupt();
        self.0.capture_done.store(true, Ordering::Release);
        self.0.changed.notify_all();
        self.0.reap();
    }
}
fn reap_worker(pid: nix::unistd::Pid) {
    use crate::process::{
        state::ProcessState,
        wait::{WaitPidObservation, observe_pid},
    };
    use nix::{
        errno::Errno,
        sys::{
            signal::{Signal, kill},
            wait::WaitPidFlag,
        },
    };
    let _ = kill(pid, Signal::SIGINT);
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        match observe_pid(pid, WaitPidFlag::WNOHANG) {
            Ok(
                WaitPidObservation::State(_, ProcessState::Completed(..))
                | WaitPidObservation::NoChild,
            ) => return,
            Ok(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(_) => break,
            Err(Errno::EINTR) => continue,
            _ => return,
        }
    }
    let _ = kill(pid, Signal::SIGKILL);
    loop {
        match observe_pid(pid, WaitPidFlag::empty()) {
            Ok(
                WaitPidObservation::State(_, ProcessState::Completed(..))
                | WaitPidObservation::NoChild,
            ) => break,
            Err(Errno::EINTR) => continue,
            Ok(_) => {
                let _ = kill(pid, Signal::SIGKILL);
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests;

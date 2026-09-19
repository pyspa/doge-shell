//! Deferred substitution bodies: authorize-then-run for `$(...)` and `<(...)`.
//!
//! Every body executes in a re-exec helper (a fresh `dogesh` process running
//! the structured plan through the shared gate → materialize → authorize →
//! launch evaluator), never in a `fork()` child and never in-process. The
//! parent only moves pipes: it hands the helper its stdout target, reads the
//! capture pipe, or retains the producer's read end as `/dev/fd/N`.
//!
//! A process-substitution descriptor is user-visible execution state.
//! Internal re-exec protocol descriptors must never overwrite it: protocol
//! targets are reserved from the kernel at spawn time (see
//! `crate::process::reexec`), never fixed numbers.
//!
//! Skipped branches (`false && $(...)`) spawn nothing: gating is checked in
//! the parent before any pipe or helper exists, and the helper re-checks
//! inner `&&`/`||` itself.

use super::authorize::{AuthorizationCancelled, ConfirmFn};
use super::plan::ExecutionPlan;
use crate::process::reexec::{ChildStdio, PlanExecMode, spawn_plan_helper};
use crate::process::{ProcessState, WaitPidObservation};
use crate::shell::Shell;
use anyhow::{Context as _, Result};
use dsh_types::Context;
use nix::unistd::Pid;
use std::future::Future;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::pin::Pin;

/// One live `<(...)` producer: the `/dev/fd/N` argument plus the resources
/// the parent owns until the consumer is spawned.
#[derive(Debug)]
pub struct ProcessSubstitution {
    /// The argv fragment handed to the consumer (`/dev/fd/N`).
    pub argument: String,
    /// Parent copy of the data-pipe read end. The consumer inherited its own
    /// copy at spawn; this one closes as soon as the consumer is launched.
    pub read_fd: OwnedFd,
    /// Producer helper pid plus its status-pipe read end, reaped (with
    /// TERM/KILL escalation) once the consumer no longer needs it.
    pub producer: ProducerHandle,
}

/// A helper producer the parent must reap: pid plus the status-pipe read end
/// carrying its one-byte completion verdict.
///
/// The `registry` handle deregisters the pid once reaped, so a `Shell` drop
/// only ever group-kills producers that shell itself still owns — never a
/// concurrent shell's producers sharing the same process (unit tests run many
/// shells on one fd/process table).
#[derive(Debug)]
pub struct ProducerHandle {
    pub pid: Pid,
    pub status_fd: OwnedFd,
    registry: ProducerRegistry,
}

/// File descriptors and auxiliary children one materialized job owns.
///
/// Dropped when the job has launched every stage: by then each consumer
/// holds its own copies, so the parent copies close here instead of leaking
/// one fd per `<(...)` for the rest of the session. Producer pids are handed
/// to detached reapers on drop.
#[derive(Debug, Default)]
pub struct ExecutionResources {
    pub inherited_fds: Vec<OwnedFd>,
    pub producers: Vec<ProducerHandle>,
}

impl ExecutionResources {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.inherited_fds.is_empty() && self.producers.is_empty()
    }

    pub fn add_process_substitution(&mut self, substitution: ProcessSubstitution) -> String {
        let argument = substitution.argument.clone();
        self.inherited_fds.push(substitution.read_fd);
        self.producers.push(substitution.producer);
        argument
    }
}

impl Drop for ExecutionResources {
    fn drop(&mut self) {
        // `inherited_fds` close here via `OwnedFd`; producers are reaped
        // without blocking the shell.
        for producer in std::mem::take(&mut self.producers) {
            spawn_producer_reaper(producer);
        }
    }
}

/// Reap every producer synchronously (prompt group-kill on lingering ones).
/// Used when the consumer already completed: the shell waits rather than
/// exiting with detached reapers that would die with it and orphan
/// grandchildren holding the session's pipes. No idle grace: a completed
/// consumer proves no producer output is still needed.
pub fn reap_producers_blocking(producers: Vec<ProducerHandle>) {
    for producer in producers {
        reap_producer_sync_after_consumer(producer);
    }
}

/// Live producer groups for one shell session.
///
/// A detached reaper dies with its process: if the shell exits first, the
/// group would linger holding session pipes (and hang test harnesses waiting
/// on EOF). Registration here lets shell shutdown group-kill every
/// still-tracked producer; reapers deregister on success.
///
/// This is per-`Shell` (shared by `Arc`), never process-global: a process may
/// host many shells at once (unit tests do), and one shell's shutdown must
/// not group-kill another shell's running producers. The previous global
/// registry did exactly that — whichever shell dropped first SIGTERMed every
/// still-registered producer in the process, emptying concurrent producers'
/// pipes before they wrote.
///
/// Invariant: cleanup only ever touches producers this shell still owns.
/// A reaped producer is deregistered by its reaper, so shutdown kill only
/// reaches genuinely lingering groups.
#[derive(Debug, Clone, Default)]
pub struct ProducerRegistry {
    inner: std::sync::Arc<parking_lot::Mutex<Vec<Pid>>>,
}

impl ProducerRegistry {
    pub fn register(&self, pid: Pid) {
        self.inner.lock().push(pid);
    }

    pub fn deregister(&self, pid: Pid) {
        self.inner.lock().retain(|known| *known != pid);
    }

    /// Best-effort group-kill of every still-registered producer. Called on
    /// shell shutdown so no producer outlives the session that spawned it.
    pub(crate) fn cleanup_producer_groups(&self) {
        let pids = std::mem::take(&mut *self.inner.lock());
        for pid in &pids {
            nix::sys::signal::killpg(*pid, nix::sys::signal::Signal::SIGTERM).ok();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        for pid in &pids {
            nix::sys::signal::killpg(*pid, nix::sys::signal::Signal::SIGKILL).ok();
        }
    }
}
/// Whether a producer wait observation means no further direct-child wait
/// is needed: the producer completed, or it is no longer waitable by this
/// caller (`NoChild`, i.e. this owner already consumed its status or it was
/// never ours). A `Stopped` producer is live state, never "reaped".
fn producer_wait_is_done(observation: &WaitPidObservation) -> bool {
    match observation {
        WaitPidObservation::State(_, ProcessState::Completed(_, _)) => true,
        WaitPidObservation::State(_, _) => false,
        WaitPidObservation::StillAlive => false,
        WaitPidObservation::NoChild => true,
    }
}

/// Whether `pid` needs no further direct-child wait. EINTR and unexpected
/// wait errors keep polling; only a terminal observation ends the wait.
fn child_no_longer_needs_wait(pid: Pid) -> bool {
    match crate::process::wait_pid_job(pid, true) {
        Ok(observation) => producer_wait_is_done(&observation),
        Err(nix::errno::Errno::EINTR) => false,
        Err(_) => false,
    }
}

/// Reap one producer helper: bounded grace, then `SIGTERM`, then `SIGKILL`,
/// then a blocking wait. Runs on a detached thread so the shell never blocks
/// on a producer that outlives its consumer (`cat <(sleep 30)`); reuses the
/// existing signal/wait primitives instead of growing a new framework.
///
/// After the wait, one status byte is read: `b'D'` means the producer denied
/// a nested body before running it. The consumer already launched (a verdict
/// is inherently asynchronous across a process boundary), so this only
/// reports — the security property (the denied command never executes)
/// holds regardless.
fn spawn_producer_reaper(producer: ProducerHandle) {
    let pid = producer.pid;
    if std::thread::Builder::new()
        .name("dsh-ps-reaper".to_string())
        .spawn(move || reap_producer_sync(producer))
        .is_err()
    {
        // Thread spawn failed: leave the group registered so shell-shutdown
        // cleanup still kills it; poll once so an already-dead child is not
        // left a zombie until then.
        let _ = crate::process::wait_pid_job(pid, true);
    }
}

/// Bounded synchronous reap of one producer: grace, group `SIGTERM`, group
/// `SIGKILL`, blocking wait, then the verdict byte. Detached-reaper path
/// only: the consumer may still be reading, so a lingering producer gets a
/// grace period before escalation.
fn reap_producer_sync(producer: ProducerHandle) {
    use std::time::{Duration, Instant};
    let pid = producer.pid;
    let reaped = |pid: Pid| child_no_longer_needs_wait(pid);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if reaped(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    escalate_if_alive_and_report(producer);
}

/// Reap one producer after its consumer completed: no idle grace. A finished
/// consumer proves the stream is unneeded — a lingering producer (`yes`
/// behind an exited `head`) is prompt `SIGTERM` material, and an
/// already-exited finite producer reaps on the first poll. Waiting the full
/// detached grace here wedged every `head <(yes)`-shaped line for ~2s.
fn reap_producer_sync_after_consumer(producer: ProducerHandle) {
    escalate_if_alive_and_report(producer);
}

/// Escalate a possibly-lingering producer (group `SIGTERM`, bounded wait,
/// group `SIGKILL`, blocking wait), then read its verdict byte and
/// deregister it. Shared by both reap paths; only the pre-escalation grace
/// differs.
fn escalate_if_alive_and_report(producer: ProducerHandle) {
    use nix::sys::signal::Signal;
    use std::time::{Duration, Instant};
    let pid = producer.pid;
    // The producer leads its own process group, so signal the GROUP: this
    // also reaches grandchildren that outlived the helper itself (a lingering
    // `sleep`, an infinite `yes` that missed `SIGPIPE`), releasing every
    // pipe copy they hold. Group membership survives reparenting, so the
    // kill works even after the helper already exited.
    let kill_group = |signal: Signal| {
        nix::sys::signal::killpg(pid, signal).ok();
    };
    let reaped = |pid: Pid| child_no_longer_needs_wait(pid);
    // Still alive: escalate before the blocking wait.
    if !reaped(pid) {
        kill_group(Signal::SIGTERM);
        let term_deadline = Instant::now() + Duration::from_secs(1);
        let mut done = false;
        while Instant::now() < term_deadline {
            if reaped(pid) {
                done = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !done {
            kill_group(Signal::SIGKILL);
            let _ = crate::process::wait_pid_job(pid, false);
        }
    }
    // Verdict byte (`b'A'`/`b'D'`/`b'E'`) or EOF when the helper died first.
    // `ProducerHandle.status_fd` closes here, releasing the last parent copy.
    let mut byte = [0u8; 1];
    let n = unsafe {
        libc::read(
            producer.status_fd.as_raw_fd(),
            byte.as_mut_ptr() as *mut libc::c_void,
            byte.len(),
        )
    };
    if n == 1 && byte[0] == b'D' {
        eprintln!("dogesh: process substitution producer denied by safety policy");
    }
    producer.registry.deregister(pid);
}

/// Snapshot the shell state one helper needs. The snapshot is authoritative;
/// per-job gating of the *outer* line already happened before this call.
fn helper_snapshot(shell: &Shell) -> crate::environment::child_snapshot::ChildShellSnapshot {
    crate::environment::child_snapshot::ChildShellSnapshot::capture(&shell.environment.read())
}

/// Run a command-substitution body in a helper and capture its stdout.
///
/// Returns the raw output; the caller trims trailing newlines per word
/// context. Parent shell state (cwd, variables, aliases, ...) is never
/// touched — isolation comes from the process boundary, not save/restore.
///
/// A nested denial inside the helper aborts the whole chain with
/// `AuthorizationCancelled` (reported on the status fd), exactly as if the
/// body had run in-process: the outer command never runs on empty output.
pub fn capture_subshell_plan_stdout<'a>(
    shell: &'a mut Shell,
    parent_ctx: &'a Context,
    plan: &'a ExecutionPlan,
    mode: PlanExecMode,
    _confirm: ConfirmFn,
) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>> {
    Box::pin(async move {
        // The helper may interact with the terminal (`helper_confirm` reads
        // `/dev/tty`, nested bodies can run interactive commands), so run it
        // with raw mode paused, as the pre-re-exec engine did around its
        // substitution reads. No-op when raw mode is off (non-interactive
        // runs, unit tests, helpers running nested substitutions); nothing
        // in this function prompts, so pausing cannot swallow a dialog.
        let _raw_pause = crate::repl::terminal_state::RawModePause::new();
        let (read_end, write_end) =
            crate::process::io::cloexec_pipe().context("failed to create substitution pipe")?;
        let (status_read, status_write) =
            crate::process::io::cloexec_pipe().context("failed to create status pipe")?;

        let snapshot = helper_snapshot(shell);
        // The substitution helper joins the shell's group so terminal
        // signals reach it together with the parent.
        let producer = spawn_plan_helper(
            &snapshot,
            plan,
            mode,
            ChildStdio {
                stdin: parent_ctx.infile,
                stdout: write_end.as_raw_fd(),
                stderr: parent_ctx.errfile,
            },
            shell.pgid,
            Some(status_write.as_raw_fd()),
        )?;
        // The helper owns the write ends now (dup'd to its stdout/status
        // fd); closing ours lets the readers see EOF when the helper exits.
        drop(write_end);
        drop(status_write);

        let output = tokio::task::spawn_blocking(move || {
            use std::io::Read as _;
            let mut file = std::fs::File::from(read_end);
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).map(|_| buf)
        })
        .await
        .context("command substitution reader failed")?
        .context("failed to read command substitution output")?;

        // Reap the (already-exited) helper synchronously: a finite producer
        // is gone by EOF, so this does not block; signal deaths surface as
        // 128+signal through the shared wait mapping.
        let _ = crate::process::wait_pid_job(producer, false);

        // The one-byte verdict distinguishes denial from empty output: the
        // helper wrote it just before exiting, so it is always available by
        // now (EOF when the helper died first means "ran", not "denied").
        let mut verdict = [0u8; 1];
        let verdict_len = {
            use std::io::Read as _;
            let mut file = std::fs::File::from(status_read);
            file.read(&mut verdict).unwrap_or(0)
        };
        if verdict_len == 1 && verdict[0] == b'D' {
            return Err(anyhow::anyhow!(AuthorizationCancelled));
        }
        if verdict_len == 1 && verdict[0] == b'E' {
            anyhow::bail!("isolated substitution helper failed");
        }

        Ok(String::from_utf8_lossy(&output).to_string())
    })
}

/// Authorize-then-start a `<(...)` producer helper and hand back ownership.
///
/// Gating of the outer line already happened; the helper re-applies inner
/// `&&`/`||` itself, so `cat <(false && echo bad; echo good)` prints only
/// `good`.
pub fn start_process_substitution<'a>(
    shell: &'a mut Shell,
    parent_ctx: &'a Context,
    plan: &'a ExecutionPlan,
    _confirm: ConfirmFn,
) -> Pin<Box<dyn Future<Output = Result<ProcessSubstitution>> + 'a>> {
    Box::pin(async move {
        // The data pipe is deliberately *not* CLOEXEC: the consumer (a
        // `fork`+`execve` external or a `posix_spawn` helper) inherits the
        // read end by forking after the pipe exists, the way every shell
        // hands `/dev/fd/N` down. The parent copy closes when the job's
        // `ExecutionResources` drop after the consumer spawned, so no fd
        // leaks for the rest of the session; extra read-end copies in
        // pipeline siblings are harmless (read ends never block EOF) and die
        // with those processes.
        let mut pipe_fds = [0 as RawFd; 2];
        if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
            anyhow::bail!("failed pipe: {}", std::io::Error::last_os_error());
        }
        // SAFETY: `pipe` succeeded, so both fds are owned by us exactly once.
        let read_end = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
        let write_end = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };
        let (status_read, status_write) =
            crate::process::io::cloexec_pipe().context("failed to create status pipe")?;

        let snapshot = helper_snapshot(shell);
        // The producer runs detached in its own process group (it is the
        // leader): terminal signals stay with the interactive session, and
        // the reaper's group-kill cleans grandchildren that outlive the
        // consumer instead of leaking harness pipes or lingering `sleep`s.
        // State isolation still comes from the process boundary.
        let producer_pid = spawn_plan_helper(
            &snapshot,
            plan,
            PlanExecMode::ProcessSubstitution,
            ChildStdio {
                stdin: parent_ctx.infile,
                stdout: write_end.as_raw_fd(),
                stderr: parent_ctx.errfile,
            },
            Pid::from_raw(0),
            Some(status_write.as_raw_fd()),
        )?;
        // The helper owns the write ends now (dup'd to its stdout/status
        // fd); dropping ours lets the consumer see EOF when the producer
        // exits, and hands status verdicts to the reaper.
        drop(write_end);
        drop(status_write);

        shell.producer_registry.register(producer_pid);
        let argument = format!("/dev/fd/{}", read_end.as_raw_fd());
        Ok(ProcessSubstitution {
            argument,
            read_fd: read_end,
            producer: ProducerHandle {
                pid: producer_pid,
                status_fd: status_read,
                registry: shell.producer_registry.clone(),
            },
        })
    })
}

use std::os::fd::AsRawFd as _;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repl::confirmation::ConfirmationAction;

    fn allow_all(_: &str) -> Result<ConfirmationAction> {
        Ok(ConfirmationAction::Yes)
    }

    /// Producer reap policy: only terminal observations end the wait.
    /// `Stopped` is live state and must never count as reaped.
    #[test]
    fn producer_reap_policy_treats_only_terminal_observations_as_done() {
        use nix::sys::signal::Signal;

        let pid = Pid::from_raw(424281);
        assert!(producer_wait_is_done(&WaitPidObservation::State(
            pid,
            ProcessState::Completed(0, None)
        )));
        assert!(producer_wait_is_done(&WaitPidObservation::State(
            pid,
            ProcessState::Completed(1, None)
        )));
        assert!(producer_wait_is_done(&WaitPidObservation::NoChild));
        assert!(!producer_wait_is_done(&WaitPidObservation::StillAlive));
        assert!(!producer_wait_is_done(&WaitPidObservation::State(
            pid,
            ProcessState::Stopped(pid, Signal::SIGTSTP)
        )));
        assert!(!producer_wait_is_done(&WaitPidObservation::State(
            pid,
            ProcessState::Running
        )));
    }

    /// Test A (producer-only): the re-exec producer delivers its bytes to the
    /// data pipe with no `/dev/fd` consumer involved.
    ///
    /// Green here + red end-to-end isolates the failure to consumer
    /// inheritance / resource lifetime, not the producer path. Red here
    /// isolates it to producer stdout wiring / helper evaluation / producer
    /// lifetime.
    #[tokio::test]
    async fn producer_only_delivers_marker_without_consumer() {
        let env = crate::environment::Environment::new();
        let mut shell = Shell::new(env.clone());
        let plan = crate::shell::parse::parse_execution_plan(
            "printf PRODUCER-MARKER",
            std::sync::Arc::clone(&env),
        )
        .expect("parse producer plan");
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);

        let substitution =
            match start_process_substitution(&mut shell, &ctx, &plan, allow_all).await {
                Ok(substitution) => substitution,
                Err(err) => panic!("dogesh helper binary missing for producer test: {err:#}"),
            };
        // The read end must survive a consumer `execve`: non-CLOEXEC.
        let read_number = substitution.read_fd.as_raw_fd();
        let cloexec = unsafe {
            let borrowed = std::os::fd::BorrowedFd::borrow_raw(substitution.read_fd.as_raw_fd());
            nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_GETFD).expect("F_GETFD")
        };
        assert!(
            !nix::fcntl::FdFlag::from_bits_retain(cloexec).contains(nix::fcntl::FdFlag::FD_CLOEXEC),
            "process-substitution read fd {read_number} must be non-CLOEXEC to survive consumer exec"
        );
        let producer_pid = substitution.producer.pid;
        tracing::debug!(
            producer_pid = %producer_pid,
            read_fd = read_number,
            argument = %substitution.argument,
            "producer-only test spawned",
        );

        // Read the data pipe directly: no `/dev/fd/N` consumer anywhere.
        let read_fd = substitution.read_fd;
        let producer = substitution.producer;
        let output = tokio::task::spawn_blocking(move || {
            use std::io::Read as _;
            let mut file = std::fs::File::from(read_fd);
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).map(|_| buf)
        })
        .await
        .expect("reader task")
        .expect("read producer pipe");
        let text = String::from_utf8_lossy(&output).to_string();
        assert_eq!(
            text, "PRODUCER-MARKER",
            "producer helper wrote {text:?}, expected PRODUCER-MARKER (producer pid {producer_pid})"
        );

        // A finite producer is gone by EOF: bounded reap must not escalate to
        // group-kill for the healthy case.
        reap_producers_blocking(vec![producer]);
    }

    /// Two substitutions retain two distinct read ends and two producers:
    /// guards against `ExecutionResources` overwrite collapsing the first.
    #[tokio::test]
    async fn two_substitutions_retain_distinct_resources() {
        let env = crate::environment::Environment::new();
        let mut shell = Shell::new(env.clone());
        let ctx = Context::new_safe(shell.pid, shell.pgid, false);
        let plan_one =
            crate::shell::parse::parse_execution_plan("printf one", std::sync::Arc::clone(&env))
                .expect("parse");
        let plan_two =
            crate::shell::parse::parse_execution_plan("printf two", std::sync::Arc::clone(&env))
                .expect("parse");

        let first = start_process_substitution(&mut shell, &ctx, &plan_one, allow_all)
            .await
            .expect("start first producer");
        let second = start_process_substitution(&mut shell, &ctx, &plan_two, allow_all)
            .await
            .expect("start second producer");

        assert_ne!(
            first.read_fd.as_raw_fd(),
            second.read_fd.as_raw_fd(),
            "two producers must hold distinct read fds"
        );
        assert_ne!(
            first.producer.pid, second.producer.pid,
            "distinct producers"
        );

        let mut resources = ExecutionResources::new();
        let arg_one = resources.add_process_substitution(first);
        let arg_two = resources.add_process_substitution(second);
        assert_eq!(resources.inherited_fds.len(), 2, "both read ends retained");
        assert_eq!(resources.producers.len(), 2, "both producers retained");
        assert_ne!(arg_one, arg_two, "distinct /dev/fd arguments");
        // `resources` drops here: fds close, producers go to detached reapers.
    }

    /// Background jobs keep their producers until the job itself is done.
    ///
    /// Launching used to hand background producers to detached reapers
    /// immediately, whose 2s grace group-killed them while the background
    /// consumer still needed their pipes (`cat <(sleep 5; echo done) &`
    /// read early EOF). Ownership now stays in the job on `wait_jobs`;
    /// detached reapers only take over when the job drops.
    ///
    /// The consumer below is the `dirs` builtin on purpose, so the whole
    /// launch stays fork-free headless (an external consumer would exercise
    /// interactive `setpgid`/PTY job control instead, which is unrelated to
    /// producer ownership). Resource retention in `Job::launch` is
    /// consumer-type-agnostic.
    #[tokio::test]
    async fn background_launch_keeps_producer_until_job_done() {
        use dsh_types::terminal::{ShellMode, TerminalState};
        use std::time::Duration;

        // The consumer is `dirs` (a background-safe builtin that ignores
        // stdin): the producer `sleep 30` stays alive for the whole test,
        // so liveness past the old 2s reaper grace proves it was not
        // group-killed early. No timing flake: kill (before fix) fires at
        // wall-clock ~2.0s, the check runs at ~3.0s.
        let input = "dirs < <(sleep 30)".to_string();
        let env = crate::environment::Environment::new();
        let mut shell = Shell::new(env.clone());
        let plan = crate::shell::parse::parse_execution_plan(&input, std::sync::Arc::clone(&env))
            .expect("parse background plan");
        // Headless interactive context: no tty, but background is genuinely
        // not waited (unlike piped `-c` runs, which wait even for `&`).
        let null = std::fs::File::open("/dev/null").expect("open /dev/null");
        use std::os::fd::AsRawFd as _;
        let null_fd = null.as_raw_fd();
        let mut ctx = Context {
            shell_pid: shell.pid,
            shell_pgid: shell.pgid,
            shell_tmode: None,
            terminal_state: TerminalState::non_terminal(),
            shell_mode: ShellMode::Script,
            foreground: false,
            interactive: true,
            infile: null_fd,
            outfile: null_fd,
            errfile: null_fd,
            captured_out: None,
            output_observer: None,
            save_history: false,
            pid: None,
            pgid: Some(shell.pgid),
            process_count: 0,
        };
        let materialized =
            crate::shell::materialize::materialize_job(&mut shell, &ctx, &plan.jobs[0], allow_all)
                .await
                .expect("materialize")
                .expect("job");
        let mut job = materialized.job;
        job.resources = materialized.resources;
        job.foreground = false;
        job.disable_pty = true;

        let state = job.launch(&mut ctx, &mut shell).await.expect("launch");
        assert_eq!(
            state,
            crate::process::state::ProcessState::Running,
            "background launch must return without waiting"
        );
        // Structural gate, no timing involved: ownership moved to the job,
        // not to an immediate detached reaper.
        assert_eq!(
            job.resources.producers.len(),
            1,
            "background job must retain its producer"
        );
        assert_eq!(
            job.resources.inherited_fds.len(),
            1,
            "background job must retain its read end"
        );

        // Functional proof: the producer is still alive past the old 2s
        // reaper grace, i.e. no detached reaper group-killed it while the
        // background consumer lives. `WNOHANG` on a running child reports
        // `None` without reaping, so the check is non-destructive.
        tokio::time::sleep(Duration::from_millis(3100)).await;
        let producer_pid = job
            .resources
            .producers
            .first()
            .expect("retained producer")
            .pid;
        assert_eq!(
            crate::process::wait_pid_job(producer_pid, true),
            Ok(crate::process::WaitPidObservation::StillAlive),
            "background producer was killed early (still needed by its consumer)"
        );
        // `job` drops here: the lingering producer goes to a detached
        // reaper (grace, then group-kill), and shell-shutdown cleanup covers
        // anything left. No zombie: the reaper owns the final wait.
    }
}

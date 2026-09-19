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
#[derive(Debug)]
pub struct ProducerHandle {
    pub pid: Pid,
    pub status_fd: OwnedFd,
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

/// Reap every producer synchronously (bounded grace, then group-kill).
/// Used when the consumer already completed: the shell waits rather than
/// exiting with detached reapers that would die with it and orphan
/// grandchildren holding the session's pipes.
pub fn reap_producers_blocking(producers: Vec<ProducerHandle>) {
    for producer in producers {
        reap_producer_sync(producer);
    }
}

/// Live producer groups, process-wide.
///
/// A detached reaper dies with its process: if the shell exits first, the
/// group would linger holding session pipes (and hang test harnesses waiting
/// on EOF). Registration here lets shell shutdown group-kill every
/// still-tracked producer; reapers deregister on success.
static PRODUCER_GROUPS: parking_lot::Mutex<Vec<Pid>> = parking_lot::Mutex::new(Vec::new());

fn register_producer(pid: Pid) {
    PRODUCER_GROUPS.lock().push(pid);
}

fn deregister_producer(pid: Pid) {
    PRODUCER_GROUPS.lock().retain(|known| *known != pid);
}

/// Best-effort group-kill of every still-registered producer. Called on
/// shell shutdown so no producer outlives the session that spawned it.
pub(crate) fn cleanup_producer_groups() {
    let pids = std::mem::take(&mut *PRODUCER_GROUPS.lock());
    for pid in &pids {
        nix::sys::signal::killpg(*pid, nix::sys::signal::Signal::SIGTERM).ok();
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    for pid in &pids {
        nix::sys::signal::killpg(*pid, nix::sys::signal::Signal::SIGKILL).ok();
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
/// `SIGKILL`, blocking wait, then the verdict byte. Shared by the detached
/// reaper thread and the foreground path (`reap_producers_blocking`).
fn reap_producer_sync(producer: ProducerHandle) {
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
    let reaped = |pid: Pid| crate::process::wait_pid_job(pid, true).is_some();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if reaped(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Still alive after grace: escalate before the blocking wait.
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
    deregister_producer(pid);
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

        register_producer(producer_pid);
        let argument = format!("/dev/fd/{}", read_end.as_raw_fd());
        Ok(ProcessSubstitution {
            argument,
            read_fd: read_end,
            producer: ProducerHandle {
                pid: producer_pid,
                status_fd: status_read,
            },
        })
    })
}

use std::os::fd::AsRawFd as _;

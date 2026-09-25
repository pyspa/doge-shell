//! Direction-aware `<(...)` / `>(...)` process substitution.
//!
//! Every body executes in a re-exec helper (a fresh `dogesh` process running
//! the structured plan through the shared gate → materialize → authorize →
//! launch evaluator), never in a `fork()` child and never in-process. The
//! parent only moves pipes: it retains one endpoint as `/dev/fd/N` and hands
//! the other to the helper.
//!
//! ```text
//! <(list): helper stdout → pipe → /dev/fd/N → outer consumer (Read)
//! >(list): outer producer → /dev/fd/N → pipe → helper stdin (Write)
//! ```
//!
//! Lifecycle is direction-aware (see `ExecutionResources::finish_foreground`):
//! a `Read` producer is useless once its outer consumer completed, so it is
//! reaped with TERM/KILL escalation; a `Write` consumer must drain to EOF
//! first, so it is never signalled on the normal path and is reaped with a
//! blocking wait on a detached thread. Abnormal `Shell` drop group-kills
//! whatever this shell still owns in both directions; normal exit explicitly
//! releases `Write` ownership without signal or wait.
//!
//! A process-substitution descriptor is user-visible execution state.
//! Internal re-exec protocol descriptors must never overwrite it: protocol
//! targets are reserved from the kernel at spawn time (see
//! `crate::process::reexec`), never fixed numbers.
//!
//! Skipped branches (`false && $(...)`) spawn nothing: gating is checked in
//! the parent before any pipe or helper exists, and the helper re-checks
//! inner `&&`/`||` itself.

use super::authorize::ConfirmFn;
use super::plan::{ExecutionPlan, ProcessSubstitutionDirection};
use crate::process::reexec::{ChildStdio, PlanExecMode, PlanSignalPolicy, spawn_plan_helper};
use crate::process::{ProcessState, WaitPidObservation};
use crate::shell::Shell;
use anyhow::{Context as _, Result};
use dsh_types::Context;
use nix::unistd::Pid;
use std::future::Future;
use std::os::fd::{AsRawFd as _, FromRawFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::pin::Pin;

/// One live process substitution: the `/dev/fd/N` argument plus the resources
/// the parent owns until the outer command is spawned.
#[derive(Debug)]
pub struct ProcessSubstitution {
    /// The argv fragment handed to the outer command (`/dev/fd/N`).
    pub argument: String,
    /// Which way bytes flow through the pipe.
    pub direction: ProcessSubstitutionDirection,
    /// Parent copy of the data-pipe endpoint handed to the outer command.
    /// Read → read end; Write → write end. Non-CLOEXEC so an `execve`d
    /// outer command keeps it as `/dev/fd/N`. Closes as soon as the outer
    /// command is launched (foreground) or the background job drops.
    pub inherited_fd: OwnedFd,
    /// Helper pid plus its status-pipe read end.
    pub helper: ProcessSubstitutionHelper,
}

/// A helper the parent must reap: pid plus the status-pipe read end carrying
/// its one-byte completion verdict.
///
/// The `registry` handle deregisters the pid once reaped, so a `Shell` drop
/// only ever group-kills helpers that shell itself still owns — never a
/// concurrent shell's helpers sharing the same process (unit tests run many
/// shells on one fd/process table).
#[derive(Debug)]
pub struct ProcessSubstitutionHelper {
    pub pid: Pid,
    pub status_fd: OwnedFd,
    registry: ProcessSubstitutionRegistry,
}

/// Backwards-compatible alias: the old producer-only name.
pub type ProducerHandle = ProcessSubstitutionHelper;
/// Backwards-compatible alias for the registry.
pub type ProducerRegistry = ProcessSubstitutionRegistry;

/// File descriptors and auxiliary children one materialized job owns.
///
/// Dropped when the job no longer needs them: by then each outer command
/// holds its own copies, so the parent copies close here instead of leaking
/// one fd per substitution for the rest of the session. Helpers are handed
/// to direction-aware detached reapers on drop.
#[derive(Debug, Default)]
pub struct ExecutionResources {
    process_substitutions: Vec<ProcessSubstitution>,
}

impl ExecutionResources {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.process_substitutions.is_empty()
    }

    pub fn add_process_substitution(&mut self, substitution: ProcessSubstitution) -> String {
        let argument = substitution.argument.clone();
        self.process_substitutions.push(substitution);
        argument
    }

    /// Foreground finalization: close-then-reap in that order.
    ///
    /// Contract (behaviorally pinned by the large-output drain and FIFO-gate
    /// tests):
    /// 1. close every parent endpoint copy first (Write helpers need EOF),
    /// 2. then reap per direction (Read: synchronous escalation; Write:
    ///    detached natural reaper, never blocking the prompt).
    pub fn finish_foreground(self) {
        // `ManuallyDrop` (not a field move) because `Self` implements
        // `Drop`: take ownership of every helper without running the
        // detached policy twice.
        let mut this = std::mem::ManuallyDrop::new(self);
        let mut substitutions = std::mem::take(&mut this.process_substitutions);
        let mut read_helpers = Vec::new();
        let mut write_helpers = Vec::new();
        for substitution in substitutions.drain(..) {
            // Close the parent endpoint copy before reaping: the Write
            // consumer cannot see EOF otherwise.
            drop(substitution.inherited_fd);
            match substitution.direction {
                ProcessSubstitutionDirection::Read => read_helpers.push(substitution.helper),
                ProcessSubstitutionDirection::Write => write_helpers.push(substitution.helper),
            }
        }
        // Read producers are useless once the outer consumer completed.
        reap_helpers_blocking(read_helpers);
        // Write consumers drain to EOF naturally on a detached thread; the
        // prompt never waits for them.
        for helper in write_helpers {
            spawn_output_consumer_reaper(helper);
        }
    }

    /// Detached finalization for background-job drop: close parent copies,
    /// then hand every helper to its detached reaper without blocking.
    ///
    /// Explicit equivalent of the `Drop` policy below; both delegate to
    /// [`detach_all`] so the two paths cannot drift apart. Background `Job`s
    /// finalize implicitly via `Drop`.
    pub fn finish_detached(self) {
        let mut this = std::mem::ManuallyDrop::new(self);
        let substitutions = std::mem::take(&mut this.process_substitutions);
        detach_all(substitutions);
    }
}

/// Shared detached policy: close parent endpoint copies first (Write helpers
/// need EOF), then hand every helper to its direction-aware detached reaper
/// without blocking. Used by [`ExecutionResources::finish_detached`] and the
/// `Drop` impl.
fn detach_all(substitutions: Vec<ProcessSubstitution>) {
    for substitution in substitutions {
        drop(substitution.inherited_fd);
        match substitution.direction {
            ProcessSubstitutionDirection::Read => {
                spawn_producer_reaper(substitution.helper);
            }
            ProcessSubstitutionDirection::Write => {
                spawn_output_consumer_reaper(substitution.helper);
            }
        }
    }
}

impl Drop for ExecutionResources {
    fn drop(&mut self) {
        // Background-job ownership reaches here without an explicit finalize:
        // detached policy for both directions, never blocking the shell.
        // Shared with `finish_detached` via `detach_all`.
        let substitutions = std::mem::take(&mut self.process_substitutions);
        detach_all(substitutions);
    }
}

/// Reap every Read helper synchronously (prompt group-kill on lingering ones).
/// Used when the outer consumer already completed: the shell waits rather than
/// exiting with detached reapers that would die with it and orphan
/// grandchildren holding session pipes. No idle grace: a completed
/// consumer proves no producer output is still needed.
pub fn reap_producers_blocking(helpers: Vec<ProcessSubstitutionHelper>) {
    reap_helpers_blocking(helpers);
}

fn reap_helpers_blocking(helpers: Vec<ProcessSubstitutionHelper>) {
    for helper in helpers {
        reap_producer_sync_after_consumer(helper);
    }
}

/// Live process-substitution helper groups for one shell session.
///
/// A detached reaper dies with its process: if the shell exits first, the
/// group would linger holding session pipes (and hang test harnesses waiting
/// on EOF). Registration here lets shell shutdown group-kill every
/// still-tracked helper; reapers deregister on success.
///
/// This is per-`Shell` (shared by `Arc`), never process-global: a process may
/// host many shells at once (unit tests do), and one shell's shutdown must
/// not group-kill another shell's running helpers.
///
/// Invariant: cleanup only ever touches helpers this shell still owns.
/// A reaped helper is deregistered by its reaper, so shutdown kill only
/// reaches genuinely lingering groups.
///
/// Resource owner is exactly one of:
/// `ExecutionResources` → reaper / registry → released/reaped.
#[derive(Debug, Clone, Default)]
pub struct ProcessSubstitutionRegistry {
    inner: std::sync::Arc<parking_lot::Mutex<Vec<RegisteredProcessSubstitution>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RegisteredProcessSubstitution {
    pid: Pid,
    direction: ProcessSubstitutionDirection,
}

impl ProcessSubstitutionRegistry {
    pub fn register(&self, pid: Pid, direction: ProcessSubstitutionDirection) {
        self.inner
            .lock()
            .push(RegisteredProcessSubstitution { pid, direction });
    }

    pub fn deregister(&self, pid: Pid) {
        self.inner.lock().retain(|known| known.pid != pid);
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, pid: Pid) -> bool {
        self.inner.lock().iter().any(|known| known.pid == pid)
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// Best-effort group-kill of every still-registered helper. Called on
    /// abnormal shell shutdown so no helper outlives the session that
    /// spawned it. Both directions are cleanup targets here.
    pub(crate) fn cleanup_process_substitution_groups(&self) {
        let entries = std::mem::take(&mut *self.inner.lock());
        for entry in &entries {
            nix::sys::signal::killpg(entry.pid, nix::sys::signal::Signal::SIGTERM).ok();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        for entry in &entries {
            nix::sys::signal::killpg(entry.pid, nix::sys::signal::Signal::SIGKILL).ok();
        }
    }

    /// Normal-exit release for `Write` consumers only: drop registry
    /// ownership without signal and without wait, letting the helper drain
    /// to EOF and outlive the exiting shell under OS reparent semantics.
    /// `Read` producers stay registered for abnormal `Drop` cleanup.
    fn detach_consumers_for_normal_exit(&self) -> usize {
        let mut guard = self.inner.lock();
        let before = guard.len();
        guard.retain(|known| known.direction != ProcessSubstitutionDirection::Write);
        before - guard.len()
    }
}

impl Shell {
    /// Normal-exit ownership release for output consumers.
    ///
    /// Mirrors `detach_known_async_jobs_for_normal_exit`: explicit release
    /// only, never inferred inside `Drop`. Returns the number of released
    /// consumers.
    pub(crate) fn detach_process_substitution_consumers_for_normal_exit(&mut self) -> usize {
        self.process_substitution_registry
            .detach_consumers_for_normal_exit()
    }
}

/// Whether a helper wait observation means no further direct-child wait
/// is needed: the helper completed, or it is no longer waitable by this
/// caller (`NoChild`, i.e. this owner already consumed its status or it was
/// never ours). A `Stopped` helper is live state, never "reaped".
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

/// Reap one Read producer helper: bounded grace, then `SIGTERM`, then
/// `SIGKILL`, then a blocking wait. Runs on a detached thread so the shell
/// never blocks on a producer that outlives its consumer
/// (`cat <(sleep 30)`); reuses the existing signal/wait primitives instead
/// of growing a new framework.
///
/// After the wait, one status byte is read: `b'D'` means the producer denied
/// a nested body before running it. The consumer already launched (a verdict
/// is inherently asynchronous across a process boundary), so this only
/// reports — the security property (the denied command never executes)
/// holds regardless.
fn spawn_producer_reaper(helper: ProcessSubstitutionHelper) {
    let pid = helper.pid;
    if std::thread::Builder::new()
        .name("dsh-ps-reaper".to_string())
        .spawn(move || reap_producer_sync(helper))
        .is_err()
    {
        // Thread spawn failed: leave the group registered so shell-shutdown
        // cleanup still kills it; poll once so an already-dead child is not
        // left a zombie until then.
        let _ = crate::process::wait_pid_job(pid, true);
    }
}

/// Bounded synchronous reap of one Read producer: grace, group `SIGTERM`,
/// group `SIGKILL`, blocking wait, then the verdict byte. Detached-reaper
/// path only: the consumer may still be reading, so a lingering producer gets
/// a grace period before escalation.
fn reap_producer_sync(helper: ProcessSubstitutionHelper) {
    use std::time::{Duration, Instant};
    let pid = helper.pid;
    let reaped = |pid: Pid| child_no_longer_needs_wait(pid);
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if reaped(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    escalate_if_alive_and_report(helper);
}

/// Reap one Read producer after its consumer completed: no idle grace. A
/// finished consumer proves the stream is unneeded — a lingering producer
/// (`yes` behind an exited `head`) is prompt `SIGTERM` material, and an
/// already-exited finite producer reaps on the first poll. Waiting the full
/// detached grace here wedged every `head <(yes)`-shaped line for ~2s.
fn reap_producer_sync_after_consumer(helper: ProcessSubstitutionHelper) {
    escalate_if_alive_and_report(helper);
}

/// Escalate a possibly-lingering Read producer (group `SIGTERM`, bounded
/// wait, group `SIGKILL`, blocking wait), then read its verdict byte and
/// deregister it. Shared by both Read reap paths; only the pre-escalation
/// grace differs.
fn escalate_if_alive_and_report(helper: ProcessSubstitutionHelper) {
    use nix::sys::signal::Signal;
    use std::time::{Duration, Instant};
    let pid = helper.pid;
    // The helper leads its own process group, so signal the GROUP: this
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
    // `ProcessSubstitutionHelper.status_fd` closes here, releasing the last
    // parent copy.
    let mut byte = [0u8; 1];
    let n = unsafe {
        libc::read(
            helper.status_fd.as_raw_fd(),
            byte.as_mut_ptr() as *mut libc::c_void,
            byte.len(),
        )
    };
    if n == 1 && byte[0] == b'D' {
        eprintln!("dogesh: process substitution producer denied by safety policy");
    }
    helper.registry.deregister(pid);
}

/// Detached natural reaper for a `Write` consumer: blocking wait for the
/// helper's natural exit, never `SIGTERM`/`SIGKILL` on the normal path.
///
/// A healthy slow consumer (`produce > >(slow-but-valid-consumer)`) is
/// legal; killing it after an arbitrary timeout would lose tail bytes. Only
/// abnormal `Shell` drop group-kills still-owned helpers.
fn spawn_output_consumer_reaper(helper: ProcessSubstitutionHelper) {
    let pid = helper.pid;
    if std::thread::Builder::new()
        .name("dsh-ps-out-reaper".to_string())
        .spawn(move || reap_output_consumer_sync(helper))
        .is_err()
    {
        // Thread spawn failed: leave the group registered so shell-shutdown
        // cleanup still kills it; poll once so an already-dead child is not
        // left a zombie until then.
        let _ = crate::process::wait_pid_job(pid, true);
    }
}

/// Blocking natural reap of one `Write` consumer: wait until it exits on its
/// own after EOF, then read its verdict byte and deregister. No signals.
fn reap_output_consumer_sync(helper: ProcessSubstitutionHelper) {
    let pid = helper.pid;
    // Blocking wait: the parent write end is already closed (see
    // `ExecutionResources::finish_foreground`), so the consumer sees EOF
    // and drains. EINTR retries inside `wait_pid_job`'s contract are
    // handled by looping on non-terminal observations.
    loop {
        match crate::process::wait_pid_job(pid, false) {
            Ok(observation) if producer_wait_is_done(&observation) => break,
            Ok(_) => continue,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => {
                // Unexpected wait error: do not spin forever; fall through
                // to verdict read and deregistration. Abnormal-drop cleanup
                // remains the safety net for genuinely stuck groups.
                break;
            }
        }
    }
    let mut byte = [0u8; 1];
    let n = unsafe {
        libc::read(
            helper.status_fd.as_raw_fd(),
            byte.as_mut_ptr() as *mut libc::c_void,
            byte.len(),
        )
    };
    if n == 1 && byte[0] == b'D' {
        eprintln!("dogesh: process substitution consumer denied by safety policy");
    }
    helper.registry.deregister(pid);
}

/// Hide one endpoint from the helper across `posix_spawn`.
///
/// The retained outer endpoint must not leak into the helper: for `Write`
/// the helper would otherwise inherit a write-end copy and `cat` would never
/// see EOF. Failure is an error, never silent: failing open here reintroduces
/// exactly that hang.
fn set_cloexec(fd: RawFd) -> Result<()> {
    // SAFETY: `fcntl` only probes/mutates flags; ownership stays.
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
    let flags = nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_GETFD)
        .context("failed to query process-substitution fd flags")?;
    let mut bits = nix::fcntl::FdFlag::from_bits_retain(flags);
    bits.insert(nix::fcntl::FdFlag::FD_CLOEXEC);
    nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_SETFD(bits))
        .context("failed to hide process-substitution fd from helper")?;
    Ok(())
}

/// Restore outer inheritability after spawn so the outer command keeps
/// `/dev/fd/N` across its own `execve`.
fn clear_cloexec(fd: RawFd) -> Result<()> {
    // SAFETY: same as above.
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
    let flags = nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_GETFD)
        .context("failed to query process-substitution fd flags")?;
    let mut bits = nix::fcntl::FdFlag::from_bits_retain(flags);
    bits.remove(nix::fcntl::FdFlag::FD_CLOEXEC);
    nix::fcntl::fcntl(borrowed, nix::fcntl::FcntlArg::F_SETFD(bits))
        .context("failed to restore process-substitution fd inheritability")?;
    Ok(())
}

/// Snapshot the shell state one helper needs. The snapshot is authoritative;
/// per-job gating of the *outer* line already happened before this call.
fn helper_snapshot(shell: &Shell) -> crate::environment::child_snapshot::ChildShellSnapshot {
    crate::environment::child_snapshot::ChildShellSnapshot::capture(&shell.environment.read())
}

/// Authorize-then-start a `<(...)` / `>(...)` helper and hand back ownership.
///
/// Gating of the outer line already happened; the helper re-applies inner
/// `&&`/`||` itself, so `cat <(false && echo bad; echo good)` prints only
/// `good`.
pub fn start_process_substitution<'a>(
    shell: &'a mut Shell,
    parent_ctx: &'a Context,
    plan: &'a ExecutionPlan,
    direction: ProcessSubstitutionDirection,
    _confirm: ConfirmFn,
) -> Pin<Box<dyn Future<Output = Result<ProcessSubstitution>> + 'a>> {
    Box::pin(async move {
        // The data pipe is deliberately *not* CLOEXEC: the outer command (a
        // `fork`+`execve` external or a `posix_spawn` helper) inherits the
        // retained endpoint by forking after the pipe exists, the way every
        // shell hands `/dev/fd/N` down. The parent copy closes when the job's
        // `ExecutionResources` finalizes after the outer command spawned, so
        // no fd leaks for the rest of the session.
        let mut pipe_fds = [0 as RawFd; 2];
        if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
            anyhow::bail!("failed pipe: {}", std::io::Error::last_os_error());
        }
        // SAFETY: `pipe` succeeded, so both fds are owned by us exactly once.
        let mut read_opt = Some(unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) });
        let mut write_opt = Some(unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) });
        let (status_read, status_write) =
            crate::process::io::cloexec_pipe().context("failed to create status pipe")?;

        let snapshot = helper_snapshot(shell);
        // The helper runs detached in its own process group (it is the
        // leader): terminal signals stay with the interactive session, and
        // the reaper's group-kill cleans grandchildren that outlive the
        // outer command instead of leaking harness pipes or lingering
        // `sleep`s. State isolation still comes from the process boundary.
        // Both directions share this grouping so abnormal-drop cleanup
        // reaches grandchildren either way; `setsid()` is never added.
        //
        // The retained outer endpoint must not leak into the helper: for
        // `Write` the helper would otherwise inherit a write-end copy and
        // `cat` would never see EOF. Temporarily mark it CLOEXEC across
        // `posix_spawn` (file-action dups still work), then clear it so the
        // outer command inherits `/dev/fd/N` across its own `execve`.
        let read_raw = read_opt.as_ref().expect("pipe read").as_raw_fd();
        let write_raw = write_opt.as_ref().expect("pipe write").as_raw_fd();
        // Retained endpoint hides from the helper; helper endpoint stays
        // inheritable for the file-action dup. Hiding is fail-closed: if it
        // fails, bail before spawning rather than handing the helper a
        // write-end copy that would hide EOF forever.
        match direction {
            ProcessSubstitutionDirection::Read => set_cloexec(read_raw)?,
            ProcessSubstitutionDirection::Write => set_cloexec(write_raw)?,
        }
        let child_stdio = match direction {
            ProcessSubstitutionDirection::Read => ChildStdio {
                stdin: parent_ctx.infile,
                stdout: write_raw,
                stderr: parent_ctx.errfile,
            },
            ProcessSubstitutionDirection::Write => ChildStdio {
                stdin: read_raw,
                stdout: parent_ctx.outfile,
                stderr: parent_ctx.errfile,
            },
        };
        // Keep both endpoints alive until spawn returns: the child dups one,
        // and dropping early would recycle the number.
        let helper_pid = spawn_plan_helper(
            &snapshot,
            plan,
            PlanExecMode::ProcessSubstitution,
            PlanSignalPolicy::Normal,
            child_stdio,
            Pid::from_raw(0),
            Some(status_write.as_raw_fd()),
        )?;
        // Restore outer inheritability. If the restore fails, the helper is
        // already running: register it and hand it to a detached reaper so
        // nothing orphans (no zombie, no leaked group), then report the
        // failure instead of returning a `/dev/fd/N` the outer command could
        // not inherit.
        let clear_result = match direction {
            ProcessSubstitutionDirection::Read => clear_cloexec(read_raw),
            ProcessSubstitutionDirection::Write => clear_cloexec(write_raw),
        };
        if let Err(err) = clear_result {
            shell
                .process_substitution_registry
                .register(helper_pid, direction);
            let registry = shell.process_substitution_registry.clone();
            let helper = ProcessSubstitutionHelper {
                pid: helper_pid,
                status_fd: status_read,
                registry,
            };
            match direction {
                ProcessSubstitutionDirection::Read => spawn_producer_reaper(helper),
                ProcessSubstitutionDirection::Write => spawn_output_consumer_reaper(helper),
            }
            drop(read_opt);
            drop(write_opt);
            drop(status_write);
            return Err(err);
        }
        // The helper owns its endpoint now (dup'd to its stdin/stdout);
        // take the retained outer endpoint and drop the other parent copy
        // so the peer sees EOF when its counterpart exits. Status verdicts
        // go to the reaper.
        let retained_end = match direction {
            ProcessSubstitutionDirection::Read => read_opt.take().expect("pipe read"),
            ProcessSubstitutionDirection::Write => write_opt.take().expect("pipe write"),
        };
        drop(read_opt);
        drop(write_opt);
        drop(status_write);

        shell
            .process_substitution_registry
            .register(helper_pid, direction);
        let argument = format!("/dev/fd/{}", retained_end.as_raw_fd());
        Ok(ProcessSubstitution {
            argument,
            direction,
            inherited_fd: retained_end,
            helper: ProcessSubstitutionHelper {
                pid: helper_pid,
                status_fd: status_read,
                registry: shell.process_substitution_registry.clone(),
            },
        })
    })
}

#[cfg(test)]
mod tests;

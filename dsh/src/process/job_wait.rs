use super::job::Job;
use super::job_process::JobProcess;
use super::state::ProcessState;
use super::wait::{WaitPidObservation, observe_pid};
use crate::shell::SHELL_TERMINAL;
use anyhow::{Context, Result};
use nix::sys::signal::Signal;
use nix::sys::signal::killpg;
use nix::sys::wait::WaitPidFlag;
use nix::unistd::{Pid, getpid, isatty, tcsetpgrp};
use std::os::fd::BorrowedFd;
use std::time::Duration;
use tokio::time;
use tracing::{debug, error};

/// Whether this process may hand the real terminal to a job.
///
/// `isatty` alone is not enough: under `cargo test` the test binary inherits
/// the developer's terminal on fd 0, so job control would retarget *their*
/// terminal's foreground process group.
fn owns_terminal() -> bool {
    crate::terminal::terminal_control_enabled()
        && isatty(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }).unwrap_or(false)
}

/// Report a job that just stopped, in the same `[1]+  Stopped  cmd` format bash
/// uses. Without it Ctrl-Z looks like it did nothing.
fn print_stopped_notice(job: &Job) {
    use crate::repl::job_notify::{JobMarker, JobNotice, JobNoticeState, format_job_notice};
    use std::io::Write;

    let notice = format_job_notice(&JobNotice {
        job_id: job.job_id,
        cmd: job.cmd.clone(),
        state: JobNoticeState::Stopped,
        marker: JobMarker::Current,
    });
    // Leading \r: in raw mode the cursor is mid-line when the job stops.
    println!("\r{}", notice);
    let _ = std::io::stdout().flush();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KnownWaitResult {
    State(Pid, ProcessState),
    StillAlive,
    /// Known pids existed but none is currently waitable by this caller
    /// (every `waitpid` reported `ECHILD`). This says nothing about the
    /// process tree: claiming completion from it alone would invent status.
    NoWaitableChildren,
    /// The canonical tree holds no pid to wait at all (e.g. a builtin-only
    /// job). Kept distinct from [`KnownWaitResult::NoWaitableChildren`] so
    /// logs tell "nothing to wait" apart from "lost wait ownership".
    NoKnownPids,
}

/// Which termination semantics a wait loop enforces.
///
/// `fg` resumes a job into the terminal session: a `SIGINT` belongs to
/// the job and a fully-stopped tree ends the wait back at the prompt.
/// `wait PID` only wants termination: stops are not completions, and a
/// `SIGINT` interrupts the builtin instead of reaching the background job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobWaitPolicy {
    Foreground,
    TerminationOnly,
}

/// How a [`wait_loop`] finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobWaitOutcome {
    Completed,
    Stopped,
    /// `SIGINT` arrived during a [`JobWaitPolicy::TerminationOnly`] wait.
    /// The job stays active; the caller requeues it and reports 130.
    Interrupted,
}
///
/// The first `waitpid` of a loop runs immediately after `fork`, so it practically
/// always reports `StillAlive`. With a single fixed delay every command — even one
/// that exits in a millisecond — paid that delay before the shell noticed and
/// redrew the prompt. Starting small and backing off keeps short commands snappy
/// while long-running jobs settle on a cheap poll rate.
const WAIT_POLL_MIN: Duration = Duration::from_millis(1);
const WAIT_POLL_MAX: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy)]
struct WaitBackoff {
    delay: Duration,
}

impl WaitBackoff {
    fn new() -> Self {
        Self {
            delay: WAIT_POLL_MIN,
        }
    }

    /// Delay to sleep before the next `waitpid`, doubling up to [`WAIT_POLL_MAX`].
    fn next_delay(&mut self) -> Duration {
        let delay = self.delay;
        self.delay = (self.delay * 2).min(WAIT_POLL_MAX);
        delay
    }

    /// A process just changed state; its siblings are likely to follow shortly,
    /// so go back to polling tightly.
    fn reset(&mut self) {
        self.delay = WAIT_POLL_MIN;
    }
}

pub async fn put_in_foreground(job: &mut Job, no_hang: bool, cont: bool) -> Result<()> {
    debug!(
        "put_in_foreground: id: {} pgid {:?} no_hang: {} cont: {}",
        job.id, job.pgid, no_hang, cont
    );

    if !owns_terminal() {
        debug!("Not a terminal environment, skipping process group control");
        debug!("About to call wait_job with no_hang: {}", no_hang);
        wait_job(job, no_hang).await?;
        debug!("wait_job completed in non-terminal mode");
        return Ok(());
    }

    debug!("Terminal environment detected, proceeding with process group control");

    // Snapshot what the terminal handoff needs before any `&mut` wait borrow.
    let job_pgid = job.pgid;
    let uses_full_proxy = crate::process::job_pty::uses_full_pty_proxy(job);
    if !uses_full_proxy {
        if let Some(pgid) = job_pgid {
            debug!("Setting foreground process group to {}", pgid);
            if let Err(err) = tcsetpgrp(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }, pgid) {
                debug!(
                    "tcsetpgrp failed: {}, continuing without terminal control",
                    err
                );
            } else {
                debug!("Successfully set foreground process group to {}", pgid);
            }

            if cont {
                debug!("Sending SIGCONT to process group {}", pgid);
                let cont_result = crate::process::signal::send_signal(pgid, Signal::SIGCONT)
                    .context("failed send signal SIGCONT");
                if let Err(err) = cont_result {
                    debug!("SIGCONT failed, restoring shell foreground before returning");
                    restore_shell_foreground(job);
                    return Err(err);
                }
                debug!("SIGCONT sent successfully");
            }
        } else {
            debug!("No pgid available, skipping process group operations");
        }
    } else {
        debug!("Full-proxy PTY job active, skipping tcsetpgrp (shell proxies I/O)");
    }

    debug!("About to call wait_job with no_hang: {}", no_hang);
    let wait_result = wait_job(job, no_hang).await;
    debug!("wait_job completed (or failed), restoring shell foreground");
    // Terminal ownership must come back even when the wait itself fails;
    // a failed wait must not strand the real terminal on the job's pgid.
    // Restoration stays best-effort (debug log, continue), so a restoration
    // failure never masks the primary wait/SIGCONT error.
    restore_shell_foreground(job);

    match &wait_result {
        Ok(()) => debug!("put_in_foreground completed successfully"),
        Err(err) => debug!("put_in_foreground wait failed: {:?}", err),
    }
    wait_result
}

fn restore_shell_foreground(job: &Job) {
    let shell_pgid = job.shell_pgid;
    debug!("Restoring shell process group {}", shell_pgid);
    if let Err(err) = tcsetpgrp(
        unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) },
        shell_pgid,
    ) {
        debug!("tcsetpgrp shell_pgid failed: {}, continuing anyway", err);
    } else {
        debug!("Successfully restored shell process group {}", shell_pgid);
    }
}

pub async fn put_in_background(job: &mut Job) -> Result<()> {
    debug!("put_in_background pgid {:?}", job.pgid,);

    if !owns_terminal() {
        debug!("Not a terminal environment, skipping process group control");
        return Ok(());
    }

    if let Err(err) = tcsetpgrp(
        unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) },
        job.shell_pgid,
    ) {
        debug!("tcsetpgrp shell_pgid failed: {}, continuing anyway", err);
    } else {
        debug!(
            "Successfully set background process group to shell {}",
            job.shell_pgid
        );
    }
    Ok(())
}

pub async fn wait_job(job: &mut Job, no_hang: bool) -> Result<()> {
    debug!("wait_job called with no_hang: {}", no_hang);
    debug!("Calling wait_process_no_hang (forced for output capture)");
    wait_process_no_hang(job).await
}

pub async fn wait_process_no_hang(job: &mut Job) -> Result<()> {
    let _ = wait_loop(job, JobWaitPolicy::Foreground).await?;
    Ok(())
}

/// Wait until a background job's canonical tree terminates, for `wait PID`.
///
/// Unlike [`wait_process_no_hang`], a fully-stopped tree is never a result:
/// the loop keeps polling until termination (or interruption). Stops are
/// not archived as exit statuses.
pub async fn wait_for_termination(job: &mut Job) -> Result<JobWaitOutcome> {
    wait_loop(job, JobWaitPolicy::TerminationOnly).await
}

async fn wait_loop(job: &mut Job, policy: JobWaitPolicy) -> Result<JobWaitOutcome> {
    debug!("wait_loop started for job: {} ({policy:?})", job.id);
    let mut backoff = WaitBackoff::new();
    let outcome = loop {
        match policy {
            JobWaitPolicy::Foreground => {
                if crate::process::signal::check_and_clear_sigint() {
                    debug!("wait_loop: Detected SIGINT in parent shell, forwarding to job");
                    if let Some(pgid) = job.pgid {
                        debug!("Forwarding SIGINT to pgid: {}", pgid);
                        let _ = killpg(pgid, Signal::SIGINT);
                    } else if let Some(pid) = job.pid {
                        debug!("Forwarding SIGINT to pid: {}", pid);
                        let _ = nix::sys::signal::kill(pid, Signal::SIGINT);
                    }
                }
            }
            // A `wait PID` SIGINT interrupts the builtin instead of
            // reaching the background job: no forwarding here. The job
            // stays active and the caller requeues it.
            JobWaitPolicy::TerminationOnly => {
                if crate::process::signal::check_and_clear_sigint() {
                    debug!("wait_loop: SIGINT during termination wait, interrupting");
                    break JobWaitOutcome::Interrupted;
                }
            }
        }

        debug!("waitpid loop iteration...");

        check_background_all_output(job).await?;

        let wait_pids = job_wait_pids(job);
        let wait_pids_for_error = wait_pids.clone();
        let (pid, state) = match tokio::task::spawn_blocking(move || {
            wait_known_pids(&wait_pids, WaitPidFlag::WUNTRACED | WaitPidFlag::WNOHANG)
        })
        .await
        {
            Ok(Ok(KnownWaitResult::State(pid, state))) => (pid, state),
            Ok(Ok(KnownWaitResult::StillAlive)) => {
                time::sleep(backoff.next_delay()).await;
                continue;
            }
            Ok(Ok(
                no_waitable @ (KnownWaitResult::NoWaitableChildren | KnownWaitResult::NoKnownPids),
            )) => {
                // `ECHILD` (or an empty pid set) never completes a job on
                // its own: only the canonical tree decides. Drain available
                // (non-blocking) output first, then consult the tree.
                check_background_all_output(job).await?;
                if job.is_process_tree_completed() {
                    drain_completed_output(job).await?;
                    break JobWaitOutcome::Completed;
                }
                if job.is_fully_stopped() {
                    match policy {
                        JobWaitPolicy::Foreground => {
                            job.refresh_lifecycle_state();
                            print_stopped_notice(job);
                            break JobWaitOutcome::Stopped;
                        }
                        // A stopped tree with no waitable child left is an
                        // infrastructure error, never a wait result: stops
                        // are not archived as exit statuses.
                        JobWaitPolicy::TerminationOnly => {
                            anyhow::bail!(
                                "no waitable child remains ({:?}) for stopped job {} ('{}', state: {:?}, known pids: {:?})",
                                no_waitable,
                                job.job_id,
                                job.cmd,
                                job.state,
                                wait_pids_for_error,
                            );
                        }
                    }
                }
                anyhow::bail!(
                    "no waitable child remains ({:?}) for active job {} ('{}', state: {:?}, known pids: {:?})",
                    no_waitable,
                    job.job_id,
                    job.cmd,
                    job.state,
                    wait_pids_for_error,
                );
            }
            Ok(Err(nix::errno::Errno::EINTR)) => {
                debug!("⏳ WAIT: waitpid interrupted by signal (EINTR), continuing");
                continue;
            }
            status => {
                error!("unexpected waitpid event: {:?}", status);
                anyhow::bail!(
                    "unexpected waitpid event for job {}: {:?}",
                    job.job_id,
                    status
                );
            }
        };

        check_background_all_output(job).await?;
        job.set_process_state(pid, state);
        backoff.reset();

        debug!("fin wait: pid:{:?} state:{:?}", pid, state);

        // Strict completion first: every stage in the canonical process
        // tree must be `Completed`. Final-consumer success alone never
        // completes the job and never kills siblings (not even
        // `Stopped / Completed(0)`).
        if job.is_process_tree_completed() {
            debug!("Job completed, breaking from wait loop");
            drain_completed_output(job).await?;
            break JobWaitOutcome::Completed;
        }

        // Only a fully-stopped job (every live stage stopped) ends the
        // foreground wait. A single stopped stage in a still-running
        // pipeline must keep polling until its siblings stop as well.
        // A termination wait never ends on a stop: it keeps polling
        // until the tree completes or the wait is interrupted.
        if job.is_fully_stopped() {
            match policy {
                JobWaitPolicy::Foreground => {
                    job.refresh_lifecycle_state();
                    print_stopped_notice(job);
                    debug!("Job stopped, breaking from wait loop");
                    break JobWaitOutcome::Stopped;
                }
                JobWaitPolicy::TerminationOnly => {
                    job.refresh_lifecycle_state();
                }
            }
        }
    };
    debug!("wait_loop completed for job: {} ({outcome:?})", job.id);
    Ok(outcome)
}

fn wait_known_pids(pids: &[Pid], flags: WaitPidFlag) -> nix::Result<KnownWaitResult> {
    if pids.is_empty() {
        return Ok(KnownWaitResult::NoKnownPids);
    }

    // One shared decoder with `wait_pid_job`, so both wait layers agree on
    // what ECHILD, EINTR, and unexpected statuses mean.
    let mut saw_alive = false;
    for pid in pids {
        match observe_pid(*pid, flags | WaitPidFlag::WNOHANG) {
            Ok(WaitPidObservation::State(pid, state)) => {
                return Ok(KnownWaitResult::State(pid, state));
            }
            Ok(WaitPidObservation::StillAlive) => {
                saw_alive = true;
            }
            // Not waitable by this caller; keep scanning the remaining pids.
            Ok(WaitPidObservation::NoChild) => {}
            Err(nix::errno::Errno::EINTR) => return Err(nix::errno::Errno::EINTR),
            Err(err) => return Err(err),
        }
    }

    if saw_alive {
        Ok(KnownWaitResult::StillAlive)
    } else {
        Ok(KnownWaitResult::NoWaitableChildren)
    }
}

fn job_wait_pids(job: &Job) -> Vec<Pid> {
    let current_pid = getpid();
    let mut pids = Vec::new();
    if let Some(process) = &job.process {
        collect_process_wait_pids(process, current_pid, &mut pids);
    }
    if pids.is_empty()
        && let Some(pid) = job.pid
        && pid != current_pid
    {
        pids.push(pid);
    }
    pids.sort_unstable_by_key(|pid| pid.as_raw());
    pids.dedup();
    pids
}

fn collect_process_wait_pids(process: &JobProcess, current_pid: Pid, pids: &mut Vec<Pid>) {
    // Variant-independent: both re-exec builtin helpers and external
    // commands own real child pids. Foreground in-process builtins carry
    // `pid == getpid()` and are excluded here.
    if let Some(pid) = process.get_pid()
        && pid != current_pid
    {
        pids.push(pid);
    }

    if let Some(next) = process.next_process() {
        collect_process_wait_pids(next, current_pid, pids);
    }
}

/// Completed-job reconciliation drain: every monitor retires through
/// [`OutputMonitor::finalize_ready_now`](crate::process::io::OutputMonitor::finalize_ready_now).
///
/// Sync because retirement never waits: whatever the kernel already holds
/// (plus the monitors' pending fragments) is recovered, and a
/// descendant-held pipe only yields `WouldBlock`, never a stall.
pub fn finalize_ready_output(job: &mut Job) -> Result<()> {
    for monitor in &mut job.monitors {
        monitor.finalize_ready_now()?;
    }
    Ok(())
}

pub async fn check_background_all_output(job: &mut Job) -> Result<()> {
    debug!(
        "check_background_all_output: monitors.len() = {}",
        job.monitors.len()
    );
    let mut i = 0;
    while i < job.monitors.len() {
        debug!("Processing monitor {}", i);
        job.monitors[i].drain_available().await?;
        i += 1;
    }
    debug!("check_background_all_output completed");
    Ok(())
}

pub async fn drain_completed_output(job: &mut Job) -> Result<()> {
    debug!(
        "drain_completed_output: monitors.len() = {}",
        job.monitors.len()
    );
    let mut i = 0;
    while i < job.monitors.len() {
        debug!("Draining completed monitor {}", i);
        job.monitors[i].drain_to_eof().await?;
        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::process::Process;
    use nix::unistd::{getpgrp, setpgid};
    use std::os::unix::process::CommandExt;
    use std::process::{Command as StdCommand, Stdio};

    #[tokio::test]
    async fn job_wait_does_not_reap_unrelated_completion_child() {
        let unrelated = StdCommand::new("sh")
            .arg("-c")
            .arg("printf unrelated")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn unrelated child");
        let mut job_child = StdCommand::new("sh")
            .arg("-c")
            .arg("sleep 0.05")
            .spawn()
            .expect("spawn job child");

        let mut job = Job::new("test".to_string(), getpgrp());
        let job_pid = Pid::from_raw(job_child.id() as i32);
        let mut process = Process::new("sh".to_string(), vec![]);
        process.pid = Some(job_pid);
        job.pid = Some(job_pid);
        job.set_process(JobProcess::Command(process));

        wait_process_no_hang(&mut job).await.expect("wait job");

        let output = unrelated.wait_with_output().expect("wait unrelated child");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "unrelated");
        let _ = job_child.wait();
    }

    /// An active tree with no waitable child is an ownership inconsistency,
    /// never a silent success: the wait must fail loudly instead of
    /// completing the job with invented status.
    ///
    /// The own pid deterministically yields ECHILD (it is never our child
    /// and can never be recycled under us), so no timing is involved.
    #[tokio::test]
    async fn active_job_with_no_waitable_child_fails_loudly() {
        let pid = getpid();

        let mut job = Job::new("test".to_string(), getpgrp());
        let mut process = Process::new("sh".to_string(), vec![]);
        process.pid = Some(pid);
        job.pid = Some(pid);
        job.set_process(JobProcess::Command(process));

        let result = wait_process_no_hang(&mut job).await;
        assert!(
            result.is_err(),
            "active tree + ECHILD everywhere must fail loudly, not complete"
        );
        // The canonical tree must not gain invented status on the way out.
        let head = job.process.as_ref().expect("process tree");
        assert_eq!(head.get_state(), ProcessState::Running);
    }

    /// A pipeline stage completing non-zero does not terminate its siblings.
    ///
    /// Both stages share one process group (as interactive pipelines do), so
    /// this exercises the `job.pgid`-gated path that integration tests — with
    /// piped stdio and no controlling terminal — cannot reach. The old
    /// `send_killpg` logic SIGKILLed the whole group when the left stage
    /// failed, and this test fails against it: the right stage ends up
    /// `Completed(137, Some(SIGKILL))` instead of `Completed(0, None)`.
    #[tokio::test]
    async fn nonzero_stage_exit_does_not_kill_pipeline_siblings() {
        fn setpgid_self(pgid: Pid) -> std::io::Result<()> {
            setpgid(Pid::from_raw(0), pgid)
                .map_err(|errno| std::io::Error::from_raw_os_error(errno as i32))
        }

        // Left stage fails (exit 3) shortly after start.
        let mut left_cmd = StdCommand::new("sh");
        left_cmd
            .arg("-c")
            .arg("sleep 0.05; exit 3")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            left_cmd.pre_exec(|| setpgid_self(Pid::from_raw(0)));
        }
        let mut left_child = left_cmd.spawn().expect("spawn left stage");
        let pgid = Pid::from_raw(left_child.id() as i32);

        // Right stage outlives the left stage's failure.
        let mut right_cmd = StdCommand::new("sh");
        right_cmd
            .arg("-c")
            .arg("sleep 0.3")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            right_cmd.pre_exec(move || setpgid_self(pgid));
        }
        let mut right_child = right_cmd.spawn().expect("spawn right stage");
        let left_pid = Pid::from_raw(left_child.id() as i32);
        let right_pid = Pid::from_raw(right_child.id() as i32);

        let mut job = Job::new("test-pipeline".to_string(), getpgrp());
        job.pgid = Some(pgid);
        let mut left_proc = Process::new("sh".to_string(), vec![]);
        left_proc.pid = Some(left_pid);
        let mut right_proc = Process::new("sh".to_string(), vec![]);
        right_proc.pid = Some(right_pid);
        left_proc.next = Some(Box::new(JobProcess::Command(right_proc)));
        job.set_process(JobProcess::Command(left_proc));

        wait_process_no_hang(&mut job).await.expect("wait pipeline");

        let _ = left_child.wait();
        let _ = right_child.wait();

        let head = job.process.as_ref().expect("pipeline head");
        assert_eq!(head.get_state(), ProcessState::Completed(3, None));
        let tail = head.next().expect("pipeline tail");
        assert_eq!(
            tail.get_state(),
            ProcessState::Completed(0, None),
            "right stage must survive the left stage's non-zero exit"
        );
        assert_eq!(job.last_process_state(), ProcessState::Completed(0, None));
    }
}

//! Normal-exit ownership release for known async jobs.
//!
//! While the shell runs, an explicit `&` job stays shell-owned (`wait_jobs`
//! plus a `KnownAsyncLedger::Active` entry) so `$!`, `wait PID`, and `jobs`
//! keep working. Only the normal execution-environment exit — command-mode
//! return, helper-plan return — releases that ownership: the job metadata is
//! dropped without any signal, wait, or output drain, and the orphaned child
//! continues under ordinary OS reparent semantics (no double-fork, no
//! `setsid`, no process-group change).
//!
//! `Shell::Drop` stays the abnormal-path safety net: anything still in
//! `wait_jobs` there was never deliberately detached and is killed. Detach
//! validation is transactional (validate all candidates before releasing
//! any) and fail-closed: a job holding a parent-owned monitor, PTY task, or
//! execution resource is never silently released.

use crate::shell::Shell;
use anyhow::Result;

/// Release `Active` known-async ownership for a normal exit.
///
/// Three passes: refresh every candidate's observed tree state, validate
/// all of them, then commit ownership release (ledger entry removed, job
/// metadata dropped) with no fallible step in between, so a validation
/// failure leaves the whole table intact for `Drop` cleanup. The refresh
/// is non-blocking observation (`WNOHANG`, like the old exit drain's
/// status poll) — never a wait — so a stopped-but-unobserved child is
/// still caught by validation instead of orphaned on stale state.
/// Returns the number of detached jobs.
impl Shell {
    pub(crate) fn detach_known_async_jobs_for_normal_exit(&mut self) -> Result<usize> {
        let mut candidates = Vec::new();
        for (index, job) in self.wait_jobs.iter().enumerate() {
            if is_detachable_known_async(job, &self.known_async) {
                candidates.push(index);
            }
        }
        for &index in &candidates {
            self.wait_jobs[index].update_status();
        }
        for &index in &candidates {
            validate_detach_safe(&self.wait_jobs[index])?;
        }
        let mut detached = 0_usize;
        for &index in candidates.iter().rev() {
            let job = self.wait_jobs.remove(index);
            if let Some(pid) = job.pid {
                self.known_async.remove(pid);
            }
            detached += 1;
            drop(job);
        }
        Ok(detached)
    }
}

/// A job is detachable only when this shell holds `Active` ledger ownership
/// for its PID with a matching job id. `foreground == false` alone is never
/// enough: stopped foreground jobs, session-owned jobs, and any future
/// background provenance all stay shell-owned for `Drop` cleanup.
fn is_detachable_known_async(
    job: &crate::process::Job,
    ledger: &crate::shell::job_ledger::KnownAsyncLedger,
) -> bool {
    let Some(pid) = job.pid else {
        return false;
    };
    match ledger.active_entry(pid) {
        Some(entry) => entry.job_id == job.job_id,
        None => false,
    }
}

/// A detached child keeps running without its parent, so it must not depend
/// on any parent-owned resource that vanishes at exit: no output monitor
/// (closing the read end would `EPIPE`/`SIGPIPE` the helper), no PTY proxy
/// task, and no execution resources (whose `Drop` spawns a producer reaper).
/// A stopped child is likewise rejected: orphaning it would strand a process
/// nobody will ever `SIGCONT`, while `Drop` cleanup still releases it with
/// `SIGKILL`. Anything unexpected fails closed — the caller keeps ownership
/// and reports an infrastructure failure instead of orphaning live state.
fn validate_detach_safe(job: &crate::process::Job) -> Result<()> {
    if job.has_stopped_process() {
        anyhow::bail!(
            "cannot detach async job {} ('{}'): has a stopped process; leaving it shell-owned for shutdown cleanup",
            job.job_id,
            job.cmd,
        );
    }
    if !job.monitors.is_empty() {
        anyhow::bail!(
            "cannot detach async job {} ('{}'): still owns {} output monitor(s)",
            job.job_id,
            job.cmd,
            job.monitors.len(),
        );
    }
    if job.pty.is_some() {
        anyhow::bail!(
            "cannot detach async job {} ('{}'): still owns a PTY",
            job.job_id,
            job.cmd,
        );
    }
    if job.pty_output_task.is_some() || job.pty_input_task.is_some() {
        anyhow::bail!(
            "cannot detach async job {} ('{}'): still owns a PTY proxy task",
            job.job_id,
            job.cmd,
        );
    }
    if !job.resources.is_empty() {
        anyhow::bail!(
            "cannot detach async job {} ('{}'): still owns execution resources",
            job.job_id,
            job.cmd,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{Job, JobProcess, Process};
    use crate::process::{ProcessState, SubshellType};
    use nix::sys::signal::Signal;
    use nix::unistd::Pid;
    use std::time::Duration;

    fn live_child() -> Pid {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep for detach test");
        let pid = Pid::from_raw(child.id() as i32);
        std::mem::forget(child);
        pid
    }

    fn reap_child(pid: Pid) {
        let _ = nix::sys::signal::kill(pid, Signal::SIGKILL);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while let Ok(nix::sys::wait::WaitStatus::StillAlive) =
            nix::sys::wait::waitpid(pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "detach-test child never reaped"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn alive(pid: Pid) -> bool {
        nix::sys::signal::kill(pid, None).is_ok()
    }

    fn tracked_job(shell: &mut Shell, pid: Pid) -> usize {
        let mut job = Job::new("sleep 30 &".to_string(), shell.pgid);
        job.job_id = shell.get_job_id();
        job.foreground = false;
        job.pid = Some(pid);
        job.pgid = Some(pid);
        let mut process = Process::new("sleep".to_string(), vec!["sleep".to_string()]);
        process.pid = Some(pid);
        job.set_process(JobProcess::Command(process));
        job.state = ProcessState::Running;
        job.subshell = SubshellType::None;
        shell
            .track_async_job(job)
            .expect("register detach-test job");
        shell.wait_jobs.last().expect("job pushed").job_id
    }

    fn test_shell() -> Shell {
        Shell::new(crate::environment::Environment::new())
    }

    #[test]
    fn detach_releases_known_active_job_without_signalling() {
        let mut shell = test_shell();
        let pid = live_child();
        let job_id = tracked_job(&mut shell, pid);
        assert!(alive(pid));

        let detached = shell
            .detach_known_async_jobs_for_normal_exit()
            .expect("detach valid async job");
        assert_eq!(detached, 1);
        assert!(
            shell.wait_jobs.iter().all(|job| job.job_id != job_id),
            "detached job must leave the table"
        );
        assert!(
            shell.known_async.active_entry(pid).is_none(),
            "detached ledger ownership must be removed"
        );
        // No signal, no wait: the child outlives the release.
        assert!(alive(pid), "detach must not signal the child");
        reap_child(pid);
    }

    #[test]
    fn detach_leaves_unknown_job_for_drop_cleanup() {
        let mut shell = test_shell();
        let pid = live_child();
        let mut job = Job::new("stopped fg".to_string(), shell.pgid);
        job.job_id = shell.get_job_id();
        job.pid = Some(pid);
        shell.wait_jobs.push(job);

        let detached = shell
            .detach_known_async_jobs_for_normal_exit()
            .expect("detach with no candidates succeeds");
        assert_eq!(detached, 0);
        assert_eq!(shell.wait_jobs.len(), 1, "unknown job stays shell-owned");
        reap_child(pid);
    }

    #[tokio::test]
    async fn detach_rejects_monitor_bearing_job_and_keeps_ownership() {
        use crate::process::io::{OutputMonitor, cloexec_pipe};
        use dsh_types::observed_output::ObservedStream;

        let mut shell = test_shell();
        let pid = live_child();
        let job_id = tracked_job(&mut shell, pid);
        let (read, _write) = cloexec_pipe().expect("pipe for detach test");
        shell
            .wait_jobs
            .last_mut()
            .expect("job pushed")
            .monitors
            .push(OutputMonitor::new(read, None, ObservedStream::Stdout).expect("monitor"));

        let err = shell
            .detach_known_async_jobs_for_normal_exit()
            .expect_err("monitor-bearing job must fail detach");
        assert!(err.to_string().contains("output monitor"));
        assert_eq!(shell.wait_jobs.len(), 1, "failed detach keeps the job");
        assert!(
            shell.known_async.active_entry(pid).is_some(),
            "failed detach keeps the ledger entry"
        );
        assert_eq!(shell.wait_jobs[0].job_id, job_id);
        reap_child(pid);
    }

    #[test]
    fn detach_rejects_stopped_job_and_keeps_ownership() {
        let mut shell = test_shell();
        let pid = Pid::from_raw(424242);
        let mut job = Job::new("sleep 60 &".to_string(), shell.pgid);
        job.job_id = shell.get_job_id();
        job.foreground = false;
        job.pid = Some(pid);
        job.pgid = Some(pid);
        let mut process = Process::new("sleep".to_string(), vec!["sleep".to_string()]);
        process.pid = Some(pid);
        process.state = ProcessState::Stopped(pid, Signal::SIGTSTP);
        job.set_process(JobProcess::Command(process));
        job.state = ProcessState::Running;
        shell
            .track_async_job(job)
            .expect("register stopped-test job");

        let err = shell
            .detach_known_async_jobs_for_normal_exit()
            .expect_err("stopped job must fail detach");
        assert!(err.to_string().contains("stopped"));
        assert_eq!(shell.wait_jobs.len(), 1, "failed detach keeps the job");
        assert!(
            shell.known_async.active_entry(pid).is_some(),
            "failed detach keeps the ledger entry"
        );
    }

    #[tokio::test]
    async fn detach_is_transactional_across_candidates() {
        use crate::process::io::{OutputMonitor, cloexec_pipe};
        use dsh_types::observed_output::ObservedStream;

        let mut shell = test_shell();
        let pid_a = live_child();
        let pid_b = live_child();
        tracked_job(&mut shell, pid_a);
        tracked_job(&mut shell, pid_b);
        // Poison only the second candidate: the first must not be released.
        let (read, _write) = cloexec_pipe().expect("pipe for detach test");
        shell
            .wait_jobs
            .last_mut()
            .expect("job pushed")
            .monitors
            .push(OutputMonitor::new(read, None, ObservedStream::Stdout).expect("monitor"));

        let err = shell
            .detach_known_async_jobs_for_normal_exit()
            .expect_err("one invalid candidate fails the whole detach");
        assert!(err.to_string().contains("output monitor"));
        assert_eq!(shell.wait_jobs.len(), 2, "no partial detach allowed");
        assert!(shell.known_async.active_entry(pid_a).is_some());
        assert!(shell.known_async.active_entry(pid_b).is_some());
        reap_child(pid_a);
        reap_child(pid_b);
    }
}

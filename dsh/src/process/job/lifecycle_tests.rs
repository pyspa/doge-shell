//! Stop-state semantics: `has_stopped_process` vs `is_fully_stopped`.
//!
//! `has_stopped_process` (any `Stopped` stage) drives SIGCONT resume
//! decisions; `is_fully_stopped` (at least one `Stopped`, no `Running`)
//! drives foreground-wait termination and the `Job.state` summary.

use super::*;
use crate::shell::job::final_exit_status;
use nix::sys::signal::Signal;
use nix::unistd::{Pid, getpgrp};
use std::os::unix::process::{CommandExt, ExitStatusExt};

fn job_with_stage_states(states: &[ProcessState]) -> Job {
    let mut job = Job::new("test".to_string(), Pid::from_raw(1));
    for (index, state) in states.iter().enumerate() {
        let mut process = Process::new(format!("stage-{}", index + 1), vec![]);
        process.state = *state;
        job.set_process(JobProcess::Command(process));
    }
    job
}

#[test]
fn refresh_lifecycle_keeps_partial_stop_running() {
    let stopped = ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP);

    // Case A: Running / Stopped stays Running.
    let mut job = job_with_stage_states(&[ProcessState::Running, stopped]);
    job.refresh_lifecycle_state();
    assert_eq!(job.state, ProcessState::Running);

    // Case B: Stopped / Running stays Running (order-independent).
    let mut job = job_with_stage_states(&[stopped, ProcessState::Running]);
    job.refresh_lifecycle_state();
    assert_eq!(job.state, ProcessState::Running);

    // Case E: Completed / Running stays Running.
    let mut job = job_with_stage_states(&[ProcessState::Completed(0, None), ProcessState::Running]);
    job.refresh_lifecycle_state();
    assert_eq!(job.state, ProcessState::Running);
}

#[test]
fn refresh_lifecycle_marks_all_stopped_pipeline_stopped() {
    // Case C: Stopped / Stopped carries the actual observed stop state.
    let mut job = job_with_stage_states(&[
        ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP),
        ProcessState::Stopped(Pid::from_raw(12), Signal::SIGSTOP),
    ]);
    job.refresh_lifecycle_state();
    assert_eq!(
        job.state,
        ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP)
    );
}

#[test]
fn refresh_lifecycle_preserves_actual_stop_signal() {
    // Case D: Completed / Stopped is fully stopped with the real signal.
    let mut job = job_with_stage_states(&[
        ProcessState::Completed(0, None),
        ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTTIN),
    ]);
    job.refresh_lifecycle_state();
    assert_eq!(
        job.state,
        ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTTIN)
    );
}

#[test]
fn refresh_lifecycle_keeps_pipeline_completion_state() {
    // Case F: all completed keeps the existing final-stage state.
    let mut job = job_with_stage_states(&[
        ProcessState::Completed(0, None),
        ProcessState::Completed(7, None),
    ]);
    job.refresh_lifecycle_state();
    assert_eq!(job.state, ProcessState::Completed(7, None));
    assert!(!job.is_fully_stopped());
    assert!(!job.has_stopped_process());
}

#[test]
fn no_process_job_is_not_fully_stopped() {
    let job = Job::new("test".to_string(), Pid::from_raw(1));
    assert!(!job.is_fully_stopped());
    assert!(!job.has_stopped_process());
}

#[test]
fn update_status_derives_summary_from_full_process_tree() {
    let stopped = ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP);

    // Root Running + tail Stopped: summary stays Running.
    let mut job = job_with_stage_states(&[ProcessState::Running, stopped]);
    job.update_status();
    assert_eq!(job.state, ProcessState::Running);

    // Root Stopped + tail Running: summary is still Running.
    let mut job = job_with_stage_states(&[stopped, ProcessState::Running]);
    job.update_status();
    assert_eq!(job.state, ProcessState::Running);

    // All stopped: summary becomes the actual Stopped state.
    let mut job = job_with_stage_states(&[
        ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP),
        ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP),
    ]);
    job.update_status();
    assert_eq!(
        job.state,
        ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP)
    );
}

#[test]
fn fg_resume_checks_any_stopped_process() {
    // Partial stop with a stale Running summary still needs SIGCONT.
    let mut job = job_with_stage_states(&[
        ProcessState::Running,
        ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP),
    ]);
    job.state = ProcessState::Running;
    assert!(job.has_stopped_process());

    // Fully-running tree with a stale Stopped summary needs no SIGCONT:
    // the canonical tree wins over the cached summary.
    let mut job = job_with_stage_states(&[ProcessState::Running, ProcessState::Running]);
    job.state = ProcessState::Stopped(Pid::from_raw(424242), Signal::SIGTSTP);
    assert!(!job.has_stopped_process());
}

#[test]
fn resume_last_job_selects_only_fully_stopped_job() {
    // Mirrors the Ctrl-Z empty-line selection: most recent fully-stopped
    // job wins, partial stops are skipped.
    let mut partial = job_with_stage_states(&[
        ProcessState::Running,
        ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP),
    ]);
    partial.cmd = "partial".to_string();
    let mut full = job_with_stage_states(&[
        ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP),
        ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP),
    ]);
    full.cmd = "full".to_string();
    assert!(!partial.is_fully_stopped());
    assert!(full.is_fully_stopped());

    let jobs = [full, partial];
    let selected = jobs
        .iter()
        .rev()
        .find(|job| job.is_fully_stopped())
        .map(|job| job.cmd.clone());
    // `rev` visits `partial` first and skips it, selecting `full`.
    assert_eq!(selected.as_deref(), Some("full"));
}

/// A job pgid naming the shell's own group must never be signaled as a
/// group: `killpg(shell_group)` would kill the shell itself.
#[test]
fn safe_job_pgid_rejects_shell_group() {
    let shell_group = getpgrp();
    let mut job = Job::new("test".to_string(), shell_group);
    job.pgid = Some(shell_group);
    assert_eq!(job.safe_job_pgid(), None);

    let own = Pid::from_raw(shell_group.as_raw() + 100_000);
    job.pgid = Some(own);
    assert_eq!(job.safe_job_pgid(), Some(own));

    job.pgid = None;
    assert_eq!(job.safe_job_pgid(), None);
}

fn setpgid_self(pgid: Pid) -> std::io::Result<()> {
    setpgid(Pid::from_raw(0), pgid).map_err(|errno| std::io::Error::from_raw_os_error(errno as i32))
}

/// With a safe job pgid, `signal` terminates through the group: the
/// dedicated group leader (as `posix_spawn` creates for background
/// re-exec helpers) dies without touching the shell.
#[test]
fn signal_prefers_safe_process_group() {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg("sleep 30");
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        cmd.pre_exec(|| setpgid_self(Pid::from_raw(0)));
    }
    let child = cmd.spawn().expect("spawn grouped child");
    let pid = Pid::from_raw(child.id() as i32);

    let shell_group = getpgrp();
    assert_ne!(pid, shell_group, "group leader must own a fresh group");
    let mut job = Job::new("test".to_string(), shell_group);
    job.pid = Some(pid);
    job.pgid = Some(pid);
    let mut process = Process::new("sh".to_string(), vec![]);
    process.pid = Some(pid);
    job.set_process(JobProcess::Command(process));

    job.signal(Signal::SIGKILL).expect("group signal");
    let mut child = child;
    let status = child.wait().expect("reap grouped child");
    assert_eq!(status.signal(), Some(Signal::SIGKILL as i32));
    // Reaching here proves the shell's own group was never signaled.
}

/// With an unsafe pgid (the shell's own group), `signal` must fall back
/// to the canonical tree's owned child pids instead of `killpg`: the
/// child dies, the test process survives.
#[test]
fn signal_falls_back_to_tree_when_pgid_is_shell_group() {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 30")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn child in shell group");
    let pid = Pid::from_raw(child.id() as i32);

    let shell_group = getpgrp();
    let mut job = Job::new("test".to_string(), shell_group);
    job.pid = Some(pid);
    job.pgid = Some(shell_group);
    let mut process = Process::new("sh".to_string(), vec![]);
    process.pid = Some(pid);
    job.set_process(JobProcess::Command(process));

    job.signal(Signal::SIGKILL).expect("tree fallback signal");
    let status = child.wait().expect("reap child");
    assert_eq!(status.signal(), Some(Signal::SIGKILL as i32));
}

#[test]
fn final_exit_status_reads_single_process_exit() {
    let job = job_with_stage_states(&[ProcessState::Completed(7, None)]);
    assert_eq!(final_exit_status(&job), Some(7));
}

#[test]
fn final_exit_status_normalizes_signal_death() {
    let job = job_with_stage_states(&[ProcessState::Completed(0, Some(Signal::SIGTERM))]);
    assert_eq!(final_exit_status(&job), Some(143));
}

#[test]
fn final_exit_status_reads_pipeline_tail() {
    // `false | true`: head fails, tail decides.
    let job = job_with_stage_states(&[
        ProcessState::Completed(1, None),
        ProcessState::Completed(0, None),
    ]);
    assert_eq!(final_exit_status(&job), Some(0));

    // `true | false`: tail failure is the status.
    let job = job_with_stage_states(&[
        ProcessState::Completed(0, None),
        ProcessState::Completed(9, None),
    ]);
    assert_eq!(final_exit_status(&job), Some(9));
}

#[test]
fn final_exit_status_is_none_without_completed_tail() {
    let job = job_with_stage_states(&[ProcessState::Running]);
    assert_eq!(final_exit_status(&job), None);

    let job = Job::new("test".to_string(), Pid::from_raw(1));
    assert_eq!(final_exit_status(&job), None);
}

#[test]
fn pipefail_status_stays_out_of_lifecycle_state() {
    use crate::process::pipeline_status::PipelineStatusPolicy;
    use dsh_types::shell_options::{ShellOption, ShellOptions};
    // `false | true` with pipefail ON: lifecycle stays tail-success,
    // logical status is the upstream failure.
    let mut job = job_with_stage_states(&[
        ProcessState::Completed(1, None),
        ProcessState::Completed(0, None),
    ]);
    let mut options = ShellOptions::default();
    options.set(ShellOption::Pipefail, true);
    job.pipeline_status_policy = PipelineStatusPolicy::from_shell_options(options);
    job.refresh_lifecycle_state();
    assert_eq!(job.state, ProcessState::Completed(0, None));
    assert_eq!(job.final_exit_status(), Some(1));
}

#[test]
fn final_status_follows_job_policy_value_not_other_values() {
    use crate::process::pipeline_status::PipelineStatusPolicy;
    use dsh_types::shell_options::{ShellOption, ShellOptions};
    // The policy is a frozen value on the job: resolving with an ON value
    // reports ON semantics, resolving with OFF reports OFF semantics.
    // The launch-time wiring itself (snapshot in `Job::launch`, no live
    // reads in finalization) is pinned end-to-end by the
    // `pipeline.pipefail-background-snapshot-frozen` contract.
    let mut on = ShellOptions::default();
    on.set(ShellOption::Pipefail, true);
    let mut job = job_with_stage_states(&[
        ProcessState::Completed(1, None),
        ProcessState::Completed(0, None),
    ]);
    job.pipeline_status_policy = PipelineStatusPolicy::from_shell_options(on);
    assert_eq!(job.final_exit_status(), Some(1));

    // An OFF-valued policy keeps OFF semantics even when another value
    // elsewhere has pipefail ON.
    let off = ShellOptions::default();
    let mut job = job_with_stage_states(&[
        ProcessState::Completed(1, None),
        ProcessState::Completed(0, None),
    ]);
    job.pipeline_status_policy = PipelineStatusPolicy::from_shell_options(off);
    let mut other_on = ShellOptions::default();
    other_on.set(ShellOption::Pipefail, true);
    assert_eq!(job.final_exit_status(), Some(0));
}

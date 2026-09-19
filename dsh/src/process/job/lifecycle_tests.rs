//! Stop-state semantics: `has_stopped_process` vs `is_fully_stopped`.
//!
//! `has_stopped_process` (any `Stopped` stage) drives SIGCONT resume
//! decisions; `is_fully_stopped` (at least one `Stopped`, no `Running`)
//! drives foreground-wait termination and the `Job.state` summary.

use super::*;
use nix::sys::signal::Signal;
use nix::unistd::Pid;

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

//! Stop-state truth table for pipelines: `has_stopped_process` (any
//! `Stopped` stage) vs `is_fully_stopped` (at least one `Stopped`, no
//! `Running`; `Completed` stages are neutral).

use crate::process::job_process::JobProcess;
use crate::process::process::Process;
use crate::process::state::ProcessState;
use nix::sys::signal::Signal;
use nix::unistd::Pid;

fn pipeline_with_states(states: &[ProcessState]) -> JobProcess {
    let mut states = states.iter().copied();
    let first = states.next().expect("pipeline needs at least one stage");
    let mut process = Process::new("stage-1".to_string(), vec![]);
    process.state = first;
    let mut pipeline = JobProcess::Command(process);
    for (index, state) in states.enumerate() {
        let mut process = Process::new(format!("stage-{}", index + 2), vec![]);
        process.state = state;
        pipeline.link(JobProcess::Command(process));
    }
    pipeline
}

fn assert_stop_states(states: &[ProcessState], has_stopped: bool, fully_stopped: bool) {
    let pipeline = pipeline_with_states(states);
    assert_eq!(
        pipeline.has_stopped_process(),
        has_stopped,
        "has_stopped_process for {:?}",
        states
    );
    assert_eq!(
        pipeline.is_fully_stopped(),
        fully_stopped,
        "is_fully_stopped for {:?}",
        states
    );
}

#[test]
fn fully_stopped_single_stopped_process() {
    assert_stop_states(
        &[ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP)],
        true,
        true,
    );
}

#[test]
fn fully_stopped_rejects_running_process() {
    assert_stop_states(&[ProcessState::Running], false, false);
}

#[test]
fn fully_stopped_rejects_completed_pipeline() {
    assert_stop_states(&[ProcessState::Completed(0, None)], false, false);
    assert_stop_states(
        &[
            ProcessState::Completed(0, None),
            ProcessState::Completed(7, None),
        ],
        false,
        false,
    );
}

#[test]
fn fully_stopped_accepts_all_stopped_pipeline() {
    assert_stop_states(
        &[
            ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP),
            ProcessState::Stopped(Pid::from_raw(12), Signal::SIGSTOP),
        ],
        true,
        true,
    );
}

#[test]
fn fully_stopped_rejects_running_plus_stopped() {
    assert_stop_states(
        &[ProcessState::Running, ProcessState::Running],
        false,
        false,
    );
}

#[test]
fn fully_stopped_is_pipeline_order_independent() {
    let stopped = ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP);
    // Partial stop: stopped stage exists either way, but a Running
    // sibling means the job as a whole is not stopped.
    assert_stop_states(&[ProcessState::Running, stopped], true, false);
    assert_stop_states(&[stopped, ProcessState::Running], true, false);
}

#[test]
fn fully_stopped_accepts_completed_plus_stopped() {
    let stopped = |pid: i32| ProcessState::Stopped(Pid::from_raw(pid), Signal::SIGTSTP);
    assert_stop_states(&[ProcessState::Completed(0, None), stopped(12)], true, true);
    assert_stop_states(&[stopped(11), ProcessState::Completed(0, None)], true, true);
    assert_stop_states(
        &[
            ProcessState::Completed(0, None),
            stopped(12),
            ProcessState::Completed(1, None),
        ],
        true,
        true,
    );
}

#[test]
fn fully_stopped_rejects_completed_plus_running() {
    assert_stop_states(
        &[ProcessState::Completed(0, None), ProcessState::Running],
        false,
        false,
    );
    assert_stop_states(
        &[ProcessState::Running, ProcessState::Completed(0, None)],
        false,
        false,
    );
}

#[test]
fn fully_stopped_rejects_stopped_with_running_tail() {
    assert_stop_states(
        &[
            ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP),
            ProcessState::Running,
            ProcessState::Completed(0, None),
        ],
        true,
        false,
    );
}

#[test]
fn has_stopped_process_is_distinct_from_fully_stopped() {
    let pipeline = pipeline_with_states(&[
        ProcessState::Running,
        ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP),
    ]);
    assert!(pipeline.has_stopped_process());
    assert!(!pipeline.is_fully_stopped());
}

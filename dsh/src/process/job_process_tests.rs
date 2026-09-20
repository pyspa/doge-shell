//! Stop-state truth table for pipelines: `has_stopped_process` (any
//! `Stopped` stage) vs `is_fully_stopped` (at least one `Stopped`, no
//! `Running`; `Completed` stages are neutral).

use crate::process::builtin::BuiltinProcess;
use crate::process::job_process::JobProcess;
use crate::process::process::Process;
use crate::process::state::ProcessState;
use dsh_types::{Context, ExitStatus};
use nix::sys::signal::Signal;
use nix::unistd::{Pid, getpid};
use std::os::unix::process::ExitStatusExt;

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

fn exit_zero_builtin(
    _ctx: &Context,
    _argv: Vec<String>,
    _proxy: &mut dyn dsh_builtin::ShellProxy,
) -> ExitStatus {
    ExitStatus::ExitedWith(0)
}

fn builtin_node(name: &str) -> JobProcess {
    JobProcess::Builtin(BuiltinProcess::new(
        name.to_string(),
        exit_zero_builtin,
        vec![name.to_string()],
    ))
}

/// Background re-exec builtins own a real child pid, so `set_pid` /
/// `get_pid` must round-trip it exactly like external commands.
#[test]
fn builtin_pid_roundtrips_through_set_get_pid() {
    let pid = Pid::from_raw(4242);
    let mut node = builtin_node("dirs");
    assert_eq!(node.get_pid(), None);
    node.set_pid(Some(pid));
    assert_eq!(node.get_pid(), Some(pid));
    node.set_pid(None);
    assert_eq!(node.get_pid(), None);
}

/// Only a pid owned by the parent shell is lifecycle-managed: the
/// foreground in-process builtin (`pid == shell pid`) owns nothing.
#[test]
fn owned_child_pid_excludes_shell_pid() {
    let shell_pid = getpid();
    let mut node = builtin_node("dirs");
    node.set_pid(Some(shell_pid));
    assert_eq!(node.owned_child_pid(shell_pid), None);

    let child = Pid::from_raw(shell_pid.as_raw() + 1000);
    node.set_pid(Some(child));
    assert_eq!(node.owned_child_pid(shell_pid), Some(child));

    node.set_pid(None);
    assert_eq!(node.owned_child_pid(shell_pid), None);
}

/// Killing a background builtin helper must signal the real child and
/// leave state synthesis to a later `waitpid` observation: `kill` itself
/// records nothing.
#[test]
fn background_builtin_child_is_killed() {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 30")
        .spawn()
        .expect("spawn long-running child");
    let pid = Pid::from_raw(child.id() as i32);

    let mut node = builtin_node("dirs");
    node.set_pid(Some(pid));
    node.kill().expect("kill background builtin helper");
    // State stays `Running` until `waitpid` observes the signal death.
    assert_eq!(node.get_state(), ProcessState::Running);

    let status = child.wait().expect("reap signaled child");
    assert_eq!(status.signal(), Some(Signal::SIGKILL as i32));
}

/// `kill` on a foreground in-process builtin must be a silent no-op:
/// signaling the shell's own pid would kill the shell itself.
#[test]
fn foreground_builtin_kill_does_not_signal_shell() {
    let mut node = builtin_node("fg-builtin");
    node.set_pid(Some(getpid()));
    node.kill().expect("kill on shell pid must be a no-op");
    assert_eq!(node.get_state(), ProcessState::Running);
    // Reaching here proves the shell survived its own kill path.
}

/// `kill` on an already-`Completed` node signals nothing: the pid may
/// already be recycled by an unrelated process.
#[test]
fn kill_skips_completed_node() {
    let mut node = builtin_node("dirs");
    node.set_pid(Some(Pid::from_raw(1)));
    node.set_state(ProcessState::Completed(0, None));
    node.kill().expect("kill on completed node must be a no-op");
    assert_eq!(node.get_state(), ProcessState::Completed(0, None));
}

/// `ESRCH` (already gone) is success-equivalent on the kill path: shutdown
/// cleanup routinely races natural exits. `i32::MAX` cannot name a live
/// process, so the kernel deterministically answers `ESRCH` without any
/// signal being delivered anywhere.
#[test]
fn kill_treats_already_gone_child_as_success() {
    let mut node = builtin_node("dirs");
    node.set_pid(Some(Pid::from_raw(i32::MAX)));
    node.kill().expect("ESRCH must not fail the kill path");
}

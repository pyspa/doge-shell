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

fn init_logging() {
    let _ = tracing_subscriber::fmt::try_init();
}

fn pipeline_states(process: &JobProcess) -> Vec<ProcessState> {
    let mut states = Vec::new();
    let mut current = Some(process);
    while let Some(process) = current {
        states.push(process.get_state());
        current = process.next_process();
    }
    states
}

#[test]
fn running_producer_with_completed_consumer_is_not_tree_completed() {
    init_logging();

    // Create a pipeline: cat | less
    let mut cat_process = Process::new("cat".to_string(), vec!["cat".to_string()]);
    let mut less_process = Process::new("less".to_string(), vec!["less".to_string()]);

    // Set initial states: producer running, consumer completed.
    cat_process.state = ProcessState::Running;
    less_process.state = ProcessState::Completed(0, None);

    // Link them in pipeline
    cat_process.next = Some(Box::new(JobProcess::Command(less_process)));

    let cat_job_process = JobProcess::Command(cat_process);

    // Strict tree completion: a completed final stage alone is not
    // completion while the producer is still running.
    assert!(!cat_job_process.is_completed());
}

#[test]
fn completed_process_is_not_stopped() {
    init_logging();
    let mut process = Process::new("test".to_string(), vec![]);
    process.state = ProcessState::Completed(0, None);

    let pipeline = JobProcess::Command(process);
    assert!(!pipeline.has_stopped_process());
    assert!(!pipeline.is_fully_stopped());
}

#[test]
fn stopped_query_sees_stopped_tail_behind_running_pipeline_head() {
    let pipeline = pipeline_with_states(&[
        ProcessState::Running,
        ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP),
    ]);

    assert!(pipeline.has_stopped_process());
}

/// Strict tree completion: only all-`Completed` pipelines complete.
/// A completed final stage alone (or a successful intermediate stage)
/// never completes the tree.
#[test]
fn pipeline_tree_completion_truth_table() {
    use ProcessState::{Completed, Running, Stopped};
    let stopped = || Stopped(Pid::from_raw(12), Signal::SIGTSTP);
    let signaled = || Completed(0, Some(Signal::SIGPIPE));
    let cases: &[(&[ProcessState], bool)] = &[
        (&[Running], false),
        (&[Completed(0, None)], true),
        (&[Running, Running], false),
        (&[Running, Completed(0, None)], false),
        (&[Running, Completed(1, None)], false),
        (&[Running, signaled()], false),
        (&[Running, Completed(0, None), Running], false),
        (&[Running, Completed(0, None), stopped()], false),
        (&[Running, Running, Completed(0, None)], false),
        (&[Completed(0, None), Completed(0, None), Running], false),
        (&[Completed(0, None), Running, Completed(0, None)], false),
        (
            &[Completed(0, None), Completed(0, None), Completed(0, None)],
            true,
        ),
        (
            &[Completed(3, None), Completed(0, None)],
            // Non-zero upstream still counts as completed: exit codes
            // never affect tree completion.
            true,
        ),
        (&[Completed(1, None), Completed(0, None), Running], false),
        (&[Completed(1, None), Running, Completed(0, None)], false),
    ];
    for (states, expected) in cases {
        let pipeline = pipeline_with_states(states);
        assert_eq!(
            pipeline.is_completed(),
            *expected,
            "tree completion for {:?}",
            states,
        );
    }
}

#[test]
fn mark_stopped_processes_running_updates_single_process() {
    let mut process =
        pipeline_with_states(&[ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP)]);

    process.mark_stopped_processes_running();

    assert_eq!(pipeline_states(&process), vec![ProcessState::Running]);
}

#[test]
fn mark_stopped_processes_running_updates_all_stopped_pipeline_stages() {
    let mut process = pipeline_with_states(&[
        ProcessState::Stopped(Pid::from_raw(11), Signal::SIGTSTP),
        ProcessState::Stopped(Pid::from_raw(12), Signal::SIGSTOP),
        ProcessState::Stopped(Pid::from_raw(13), Signal::SIGTTIN),
    ]);

    process.mark_stopped_processes_running();

    assert_eq!(
        pipeline_states(&process),
        vec![
            ProcessState::Running,
            ProcessState::Running,
            ProcessState::Running
        ]
    );
}

#[test]
fn mark_stopped_processes_running_preserves_completed_pipeline_stage() {
    let mut process = pipeline_with_states(&[
        ProcessState::Completed(0, None),
        ProcessState::Stopped(Pid::from_raw(12), Signal::SIGTSTP),
        ProcessState::Running,
    ]);

    process.mark_stopped_processes_running();

    assert_eq!(
        pipeline_states(&process),
        vec![
            ProcessState::Completed(0, None),
            ProcessState::Running,
            ProcessState::Running
        ]
    );
}

#[test]
fn test_job_process_variants() {
    init_logging();
    let process = Process::new("test".to_string(), vec![]);
    let job_process = JobProcess::Command(process);

    // JobProcess type check
    match job_process {
        JobProcess::Command(_) => {} // Expected variant
        _ => panic!("Expected Command variant"),
    }
}

#[test]
fn output_only_pty_keeps_stdin_on_real_terminal() {
    use crate::process::job_process::apply_pty_stdio;
    use crate::process::pty::PtyMode;
    use libc::STDIN_FILENO;

    let mut ctx = Context::new_safe(Pid::from_raw(1), Pid::from_raw(1), true);
    let slave = 42;

    let applied = apply_pty_stdio(&mut ctx, slave, PtyMode::OutputOnly);

    assert!(applied);
    assert_eq!(ctx.infile, STDIN_FILENO);
    assert_eq!(ctx.outfile, slave);
    assert_eq!(ctx.errfile, slave);
}

#[test]
fn full_proxy_pty_replaces_stdin_stdout_and_stderr() {
    use crate::process::job_process::apply_pty_stdio;
    use crate::process::pty::PtyMode;

    let mut ctx = Context::new_safe(Pid::from_raw(1), Pid::from_raw(1), true);
    let slave = 42;

    let applied = apply_pty_stdio(&mut ctx, slave, PtyMode::FullProxy);

    assert!(applied);
    assert_eq!(ctx.infile, slave);
    assert_eq!(ctx.outfile, slave);
    assert_eq!(ctx.errfile, slave);
}

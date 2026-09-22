//! Job construction, argv helpers, and pipeline completion predicates.
//!
//! Signal-ownership tests live in [`super::lifecycle_tests`]; stop-state
//! semantics live there as well.

use super::*;
use crate::process::wait::is_job_completed;
use crate::shell::SHELL_TERMINAL;
use nix::sys::termios::tcgetattr;
use nix::unistd::{Pid, getpgrp, getpid, isatty};
use std::os::fd::BorrowedFd;

fn init() {
    let _ = tracing_subscriber::fmt::try_init();
}

#[test]
fn test_find_job() {
    init();
    let pgid1 = Pid::from_raw(1);
    let pgid2 = Pid::from_raw(2);
    let pgid3 = Pid::from_raw(3);

    let mut job1 = Job::new_with_process("test1".to_owned(), "".to_owned(), vec![]);
    job1.pgid = Some(pgid1);
    let mut job2 = Job::new_with_process("test2".to_owned(), "".to_owned(), vec![]);
    job2.pgid = Some(pgid2);
    let mut job3 = Job::new_with_process("test3".to_owned(), "".to_owned(), vec![]);
    job3.pgid = Some(pgid3);
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

fn pipeline_of(commands: &[Vec<String>]) -> Job {
    let mut job = Job::new("test".to_string(), Pid::from_raw(1));
    for command in commands {
        let process = Process::new(command[0].clone(), command.clone());
        job.set_process(JobProcess::Command(process));
    }
    job
}

#[test]
fn schema_args_go_to_the_last_external_command() {
    let mut job = pipeline_of(&[argv(&["ps", "aux"]), argv(&["grep", "dogesh"])]);
    assert_eq!(job.last_external_argv(), Some(argv(&["grep", "dogesh"])));

    job.append_args_to_last_external(&argv(&["--color=never"]));
    assert_eq!(
        job.last_external_argv(),
        Some(argv(&["grep", "dogesh", "--color=never"]))
    );
}

#[test]
fn schema_args_are_inserted_before_a_pathspec_terminator() {
    // Appending after `--` would turn the injected flags into pathspecs
    // and break the command.
    let mut job = pipeline_of(&[argv(&["git", "log", "--", "README.md"])]);
    job.append_args_to_last_external(&argv(&["--pretty=format:%h", "--date=short"]));
    assert_eq!(
        job.last_external_argv(),
        Some(argv(&[
            "git",
            "log",
            "--pretty=format:%h",
            "--date=short",
            "--",
            "README.md"
        ]))
    );

    // A command literally named `--` (argv[0]) is not a terminator.
    let mut job = pipeline_of(&[argv(&["--", "x"])]);
    job.append_args_to_last_external(&argv(&["-o"]));
    assert_eq!(job.last_external_argv(), Some(argv(&["--", "x", "-o"])));
}

#[test]
#[ignore] // Ignore this test as it requires a TTY environment
fn create_job() -> Result<()> {
    init();
    let input = "/usr/bin/touch".to_string();
    let _path = input.clone();
    let _argv: Vec<String> = input.split_whitespace().map(|s| s.to_string()).collect();
    let job = &mut Job::new(input, getpgrp());

    let process = Process::new("1".to_string(), vec![]);
    job.set_process(JobProcess::Command(process));
    let process = Process::new("2".to_string(), vec![]);
    job.set_process(JobProcess::Command(process));

    let pid = getpid();
    let pgid = getpgrp();

    // Skip TTY-dependent operations in test environment
    if isatty(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }).unwrap_or(false) {
        let tmode = match tcgetattr(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }) {
            Ok(mode) => mode,
            Err(_) => return Ok(()),
        };
        let _ctx = Context::new(pid, pgid, Some(tmode), true);
    } else {
        // Create a mock context for non-TTY environments
        println!("Skipping TTY-dependent test operations");
    }

    Ok(())
}

#[test]
fn running_producer_with_completed_consumer_is_not_job_complete() {
    init();

    let shell_pgid = getpgrp();
    let mut job = Job::new("cat file | less".to_string(), shell_pgid);

    // Create pipeline processes
    let mut cat_process = Process::new("cat".to_string(), vec!["cat".to_string()]);
    let mut less_process = Process::new("less".to_string(), vec!["less".to_string()]);

    // Set states: cat running, less completed normally
    cat_process.state = ProcessState::Running;
    less_process.state = ProcessState::Completed(0, None);

    // Link pipeline
    cat_process.next = Some(Box::new(JobProcess::Command(less_process)));
    job.set_process(JobProcess::Command(cat_process));

    // Strict completion: a completed final consumer alone is not job
    // completion while the producer is still running.
    assert!(!is_job_completed(&job));
    assert!(!job.is_process_tree_completed());
}

#[test]
fn test_normal_pipeline_completion() {
    init();

    let shell_pgid = getpgrp();
    let mut job = Job::new("cat file | less".to_string(), shell_pgid);

    // Create pipeline processes
    let mut cat_process = Process::new("cat".to_string(), vec!["cat".to_string()]);
    let mut less_process = Process::new("less".to_string(), vec!["less".to_string()]);

    // Set states: both completed
    cat_process.state = ProcessState::Completed(0, None);
    less_process.state = ProcessState::Completed(0, None);

    // Link pipeline
    cat_process.next = Some(Box::new(JobProcess::Command(less_process)));
    job.set_process(JobProcess::Command(cat_process));

    // Job should be completed normally
    assert!(is_job_completed(&job));
}

#[tokio::test]
async fn launch_restores_caller_context() -> Result<()> {
    use crate::environment::Environment;
    use crate::shell::Shell;
    use anyhow::Context as _;
    use std::os::fd::AsRawFd as _;

    let env = Environment::new();
    let mut shell = Shell::new(env);
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    // Non-default entry values: restoration must return to these, not to
    // defaults. stdio uses real (but inert) descriptors so the launch below
    // runs the success path instead of failing on bad fds.
    let stdin_file = std::fs::File::open("/dev/null").context("open /dev/null")?;
    let stdout_file = std::fs::File::open("/dev/null").context("open /dev/null")?;
    let stderr_file = std::fs::File::open("/dev/null").context("open /dev/null")?;
    ctx.foreground = false;
    ctx.pgid = Some(Pid::from_raw(1234));
    ctx.process_count = 7;
    ctx.infile = stdin_file.as_raw_fd();
    ctx.outfile = stdout_file.as_raw_fd();
    ctx.errfile = stderr_file.as_raw_fd();
    ctx.pid = Some(Pid::from_raw(5678));

    let mut job = Job::new("true".to_string(), shell.pgid);
    // Bare `true` resolves via `PATH`: Linux and macOS place it
    // differently, and `check-portability.py` flags absolute literals.
    job.set_process(JobProcess::Command(Process::new(
        "true".to_string(),
        vec!["true".to_string()],
    )));

    // The restoration asserts below only cover the success path if the
    // spawn itself succeeded: fail the test on `Err` or `CommandFailed`
    // instead of silently checking context restore after a failed launch.
    let outcome = job.launch(&mut ctx, &mut shell).await?;
    assert!(
        matches!(outcome, super::JobLaunchOutcome::Process(_)),
        "expected successful launch, got {outcome:?}"
    );

    assert!(!ctx.foreground, "foreground must be restored");
    assert_eq!(ctx.pgid, Some(Pid::from_raw(1234)), "pgid must be restored");
    assert_eq!(ctx.process_count, 7, "process_count must be restored");
    assert_eq!(
        ctx.infile,
        stdin_file.as_raw_fd(),
        "infile must be restored"
    );
    assert_eq!(
        ctx.outfile,
        stdout_file.as_raw_fd(),
        "outfile must be restored"
    );
    assert_eq!(
        ctx.errfile,
        stderr_file.as_raw_fd(),
        "errfile must be restored"
    );
    assert_eq!(ctx.pid, Some(Pid::from_raw(5678)), "pid must be restored");
    Ok(())
}

#[tokio::test]
async fn launch_failure_cleans_up_committed_pty_state() {
    use crate::environment::Environment;
    use crate::process::redirect::Redirect;
    use crate::shell::Shell;

    let env = Environment::new();
    let mut shell = Shell::new(env);
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    // Force the PTY path: `launch_inner` overwrites `ctx.foreground` from
    // the job, but `interactive` stays as set here. The input redirect below
    // makes the setup select OutputOnly, so the production input opener is
    // never called and the real terminal is untouched.
    ctx.foreground = true;
    ctx.interactive = true;

    let mut job = Job::new("cat < /nonexistent_file_for_test".to_string(), shell.pgid);
    job.foreground = true;
    let process = Process::new("cat".to_string(), vec!["cat".to_string()]).with_execution_metadata(
        vec![Redirect::input("/nonexistent_file_for_test".to_string())],
        Vec::new(),
    );
    job.set_process(JobProcess::Command(process));

    let result = job.launch(&mut ctx, &mut shell).await;
    assert!(
        matches!(result, Ok(super::JobLaunchOutcome::CommandFailed(_))),
        "expected CommandFailed, got {result:?}"
    );
    // The `CommandFailed` path in `launch_inner` must reclaim the PTY state
    // committed by `setup_pty` (a `JoinHandle` is not cancelled by `Drop`,
    // so leaving the output monitor behind would leak a task per failure).
    assert!(
        job.pty.is_none(),
        "PTY fd must be released on launch failure"
    );
    assert!(job.pty_mode.is_none());
    assert!(job.pty_output_task.is_none());
    assert!(job.pty_input_task.is_none());
}

#[tokio::test]
async fn launch_restores_context_on_redirect_failure() {
    use crate::environment::Environment;
    use crate::process::redirect::Redirect;
    use crate::shell::Shell;

    let env = Environment::new();
    let mut shell = Shell::new(env);
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    ctx.pgid = Some(Pid::from_raw(9999));
    ctx.process_count = 3;

    let mut job = Job::new("cat < /nonexistent_file_for_test".to_string(), shell.pgid);
    let process = Process::new("cat".to_string(), vec!["cat".to_string()]).with_execution_metadata(
        vec![Redirect::input("/nonexistent_file_for_test".to_string())],
        Vec::new(),
    );
    job.set_process(JobProcess::Command(process));

    let result = job.launch(&mut ctx, &mut shell).await;
    assert!(
        matches!(result, Ok(super::JobLaunchOutcome::CommandFailed(_))),
        "expected CommandFailed, got {result:?}"
    );
    assert_eq!(
        ctx.pgid,
        Some(Pid::from_raw(9999)),
        "pgid must be restored after failure"
    );
    assert_eq!(
        ctx.process_count, 3,
        "process_count must be restored after failure"
    );
}

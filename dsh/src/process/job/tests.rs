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

//! Fatal pipeline launch rollback, owned-child admission and FD budgets.

use super::tests::{OwnedChildCleanup, argv, pipeline_of};
use super::*;
use crate::{environment::Environment, shell::Shell};
use nix::{
    errno::Errno,
    sys::wait::{WaitPidFlag, waitpid},
};

#[tokio::test]
async fn grouping_failure_reaps_current_and_upstream_children() {
    for fail_stage in 1..=3 {
        let mut shell = Shell::new(Environment::new());
        let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
        ctx.interactive = true;
        let mut job = pipeline_of(&[
            argv(&["sleep", "30"]),
            argv(&["sleep", "30"]),
            argv(&["sleep", "30"]),
        ]);
        job.disable_pty = true;
        job.fail_parent_group_setup = Some(fail_stage);
        let result = job.launch(&mut ctx, &mut shell).await;
        let mut stage = job.process.as_deref();
        let mut guards = Vec::new();
        while let Some(process) = stage {
            guards.push(OwnedChildCleanup(process.get_pid().unwrap()));
            stage = process.next_process();
        }
        // All exact owned PID guards exist before any assertion.
        assert_eq!(
            result.unwrap_err().to_string(),
            "injected parent group setup failure"
        );
        assert_eq!(guards.len(), fail_stage);
        assert!(job.is_process_tree_completed());
        for guard in &guards {
            assert_eq!(
                waitpid(guard.0, Some(WaitPidFlag::WNOHANG)),
                Err(Errno::ECHILD)
            );
        }
    }
}

#[tokio::test]
async fn bounded_rollback_transfers_resources_and_monitors_to_managed_jobs() {
    use crate::repl::confirmation::ConfirmationAction;
    use crate::shell::{
        parse::parse_execution_plan, plan::ProcessSubstitutionDirection,
        process_substitution::start_process_substitution,
    };
    let mut shell = Shell::new(Environment::new());
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    let plan = parse_execution_plan("printf owned", shell.environment.clone()).unwrap();
    let substitution = start_process_substitution(
        &mut shell,
        &ctx,
        &plan,
        ProcessSubstitutionDirection::Read,
        |_| Ok(ConfirmationAction::Yes),
    )
    .await
    .unwrap();
    let mut job = pipeline_of(&[argv(&["sleep", "30"])]);
    job.resources.add_process_substitution(substitution);
    job.disable_pty = true;
    job.fail_parent_group_setup = Some(1);
    job.defer_abort_reap = true;
    let result = job.launch(&mut ctx, &mut shell).await;
    let pid = shell
        .wait_jobs
        .first()
        .and_then(|owned| owned.pid)
        .expect("managed owner retained");
    let _cleanup = OwnedChildCleanup(pid);
    assert_eq!(
        result.unwrap_err().to_string(),
        "injected parent group setup failure"
    );
    assert!(job.process.is_none(), "ownership moved, never cloned");
    assert!(job.monitors.is_empty());
    assert!(job.resources.is_empty());
    assert_eq!(shell.wait_jobs.len(), 1);
    assert_eq!(shell.wait_jobs[0].monitors.len(), 1);
    assert!(!shell.wait_jobs[0].resources.is_empty());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let completed = loop {
        let completed = crate::shell::job::check_job_state(&mut shell)
            .await
            .unwrap();
        if !completed.is_empty() {
            break completed;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "managed waiter must reap"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    assert!(completed[0].is_process_tree_completed());
    assert_eq!(waitpid(pid, Some(WaitPidFlag::WNOHANG)), Err(Errno::ECHILD));
    assert!(shell.wait_jobs.is_empty());
}

#[tokio::test]
async fn fatal_launch_cleans_output_only_pty_tasks_and_restores_context() {
    use crate::process::redirect::Redirect;
    for grouping_failure in [false, true] {
        let mut shell = Shell::new(Environment::new());
        let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
        ctx.interactive = true;
        let entry = (
            ctx.infile,
            ctx.outfile,
            ctx.errfile,
            ctx.pgid,
            ctx.foreground,
        );
        let args = if grouping_failure {
            argv(&["sleep", "30"])
        } else {
            argv(&["true", "bad\0arg"])
        };
        let process = Process::new(args[0].clone(), args)
            .with_execution_metadata(vec![Redirect::input("/dev/null".into())], Vec::new());
        let mut job = Job::new("fatal PTY launch".into(), shell.pgid);
        job.set_process(JobProcess::Command(process));
        job.fail_parent_group_setup = grouping_failure.then_some(1);
        let result = job.launch(&mut ctx, &mut shell).await;
        let _cleanup = job.pid.map(OwnedChildCleanup);
        assert!(result.is_err());
        assert!(job.is_process_tree_completed());
        assert!(job.pty.is_none());
        assert!(job.pty_mode.is_none());
        assert!(job.pty_output_task.is_none());
        assert!(job.pty_input_task.is_none());
        assert_eq!(
            (
                ctx.infile,
                ctx.outfile,
                ctx.errfile,
                ctx.pgid,
                ctx.foreground
            ),
            entry
        );
    }
}

#[tokio::test]
async fn first_stage_failure_preserves_unrelated_child_and_borrowed_stdio() {
    use std::os::fd::AsRawFd;
    let mut sibling = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let _cleanup = OwnedChildCleanup(Pid::from_raw(sibling.id() as i32));
    let mut shell = Shell::new(Environment::new());
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
    let input = std::fs::File::open("/dev/null").unwrap();
    let output = tempfile::tempfile().unwrap();
    ctx.infile = input.as_raw_fd();
    ctx.outfile = output.as_raw_fd();
    ctx.errfile = output.as_raw_fd();
    ctx.captured_out = Some(output.as_raw_fd());
    ctx.pgid = Some(getpgrp());
    let mut job = pipeline_of(&[argv(&["true", "bad\0arg"])]);
    job.disable_pty = true;
    assert!(
        job.launch(&mut ctx, &mut shell)
            .await
            .unwrap_err()
            .to_string()
            .contains("CString")
    );
    assert!(sibling.try_wait().unwrap().is_none());
    assert_eq!(
        nix::unistd::write(&output, b"caller still owns output").unwrap(),
        24
    );
    let mut next = pipeline_of(&[argv(&["true"])]);
    next.disable_pty = true;
    assert!(next.launch(&mut ctx, &mut shell).await.is_ok());
}

#[test]
fn repeated_fatal_launch_reclaims_pipeline_fds() {
    use std::os::unix::process::CommandExt;
    const PROBE: &str = "DOGESH_PIPELINE_FD_PROBE";
    if std::env::var_os(PROBE).is_none() {
        let proof = tempfile::NamedTempFile::new().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "process::job::launch_failure_tests::repeated_fatal_launch_reclaims_pipeline_fds",
                "--nocapture",
            ])
            .env(PROBE, proof.path());
        unsafe {
            command.pre_exec(|| {
                let limit = libc::rlimit {
                    rlim_cur: 96,
                    rlim_max: 96,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "isolated FD probe failed: {status}");
                assert_eq!(
                    std::fs::read(proof.path()).unwrap(),
                    b"ok",
                    "isolated probe must actually run"
                );
                return;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("isolated FD probe timed out");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut shell = Shell::new(Environment::new());
            let mut ctx = Context::new_safe(shell.pid, shell.pgid, false);
            for _ in 0..128 {
                let mut job = pipeline_of(&[argv(&["true"]), argv(&["true", "bad\0arg"])]);
                job.disable_pty = true;
                let result = job.launch(&mut ctx, &mut shell).await;
                let _cleanup = job.pid.map(OwnedChildCleanup);
                assert!(
                    result.unwrap_err().to_string().contains("CString"),
                    "FD budget must survive repeated failures"
                );
            }

            // Leave just enough descriptors for the first capture pipe, then
            // force the second pipe (or exec-error pipe) to fail. Reopening the
            // complete reserved budget proves all partial setup was reclaimed.
            for (observer, free_slots) in [(true, 3), (false, 3), (false, 5)] {
                let mut held = Vec::new();
                loop {
                    match std::fs::File::open("/dev/null") {
                        Ok(file) => held.push(file),
                        Err(err) => {
                            assert_eq!(err.raw_os_error(), Some(libc::EMFILE));
                            break;
                        }
                    }
                }
                assert!(held.len() > free_slots);
                held.truncate(held.len() - free_slots);
                for _ in 0..128 {
                    if observer {
                        ctx.output_observer =
                            Some(dsh_types::observed_output::ObservedOutput::shared(1024));
                        let mut job = pipeline_of(&[argv(&["true"])]);
                        job.disable_pty = true;
                        let error = job.launch(&mut ctx, &mut shell).await.unwrap_err();
                        assert_eq!(error.to_string(), "failed stderr pipe");
                    } else {
                        ctx.output_observer = None;
                        ctx.foreground = false;
                        let mut process = Process::new("true".into(), argv(&["true"]));
                        let error = crate::process::fork::fork_process(
                            &ctx,
                            None,
                            &mut process,
                            &mut shell,
                            None,
                            None,
                            None,
                        )
                        .unwrap_err();
                        assert_eq!(
                            error.to_string(),
                            if free_slots == 3 {
                                "failed pipe"
                            } else {
                                "failed exec-error pipe"
                            }
                        );
                    }
                    let reserved: Vec<_> = (0..free_slots)
                        .map(|_| {
                            std::fs::File::open("/dev/null")
                                .expect("partial pipe setup must return its entire FD budget")
                        })
                        .collect();
                    drop(reserved);
                }
                drop(held);
            }
        });
    std::fs::write(std::env::var_os(PROBE).unwrap(), b"ok").unwrap();
}

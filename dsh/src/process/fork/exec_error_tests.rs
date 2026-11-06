use super::*;
use dsh_types::observed_output::ObservedOutput;
use std::collections::VecDeque;
use std::os::fd::{AsFd, AsRawFd};

enum ReadStep {
    Bytes(Vec<u8>),
    Error(i32),
    Eof,
}

fn failure_record() -> Vec<u8> {
    // Encode the existing native-layout pipe protocol without reading
    // uninitialized padding from a Rust struct.
    let mut bytes = vec![0; std::mem::size_of::<ChildExecError>()];
    bytes[std::mem::offset_of!(ChildExecError, stage)] = STAGE_EXECVE;
    let offset = std::mem::offset_of!(ChildExecError, errno);
    bytes[offset..offset + std::mem::size_of::<i32>()].copy_from_slice(&libc::EACCES.to_ne_bytes());
    bytes
}

fn drain_to(
    steps: Vec<ReadStep>,
    stderr: RawFd,
    target: Option<ParentExecDiagnostic<'_>>,
) -> (bool, usize) {
    let mut steps = VecDeque::from(steps);
    let reported = drain_exec_error_with_reader(
        |buffer| match steps.pop_front().expect("unexpected extra read") {
            ReadStep::Bytes(bytes) => {
                assert!(bytes.len() <= buffer.len());
                buffer[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            ReadStep::Error(errno) => Err(std::io::Error::from_raw_os_error(errno)),
            ReadStep::Eof => Ok(0),
        },
        "fixture",
        stderr,
        &[],
        target,
    );
    (reported, steps.len())
}

fn drain(steps: Vec<ReadStep>) -> (bool, String, usize) {
    let stderr = tempfile::NamedTempFile::new().unwrap();
    let (reported, remaining) = drain_to(steps, stderr.as_raw_fd(), None);
    let diagnostic = std::fs::read_to_string(stderr.path()).unwrap();
    (reported, diagnostic, remaining)
}

#[test]
fn exec_error_empty_eof_has_no_failure_or_diagnostic() {
    assert_eq!(drain(vec![ReadStep::Eof]), (false, String::new(), 0));
}

#[test]
fn exec_error_complete_record_needs_no_eof_read() {
    let (reported, diagnostic, remaining) = drain(vec![ReadStep::Bytes(failure_record())]);
    assert!(reported);
    assert!(diagnostic.contains("Permission denied"));
    assert_eq!(remaining, 0);
}

#[test]
fn exec_error_repeated_eintr_then_eof_has_no_failure_or_diagnostic() {
    assert_eq!(
        drain(vec![
            ReadStep::Error(libc::EINTR),
            ReadStep::Error(libc::EINTR),
            ReadStep::Eof,
        ]),
        (false, String::new(), 0)
    );
}

#[test]
fn exec_error_partial_reads_preserve_complete_failure() {
    let bytes = failure_record();
    let (reported, diagnostic, remaining) = drain(vec![
        ReadStep::Bytes(bytes[..2].to_vec()),
        ReadStep::Bytes(bytes[2..].to_vec()),
    ]);
    assert!(reported);
    assert!(diagnostic.contains("Permission denied"));
    assert_eq!(remaining, 0);
}

#[test]
fn exec_error_partial_eof_keeps_short_report_diagnostic() {
    let (reported, diagnostic, remaining) = drain(vec![
        ReadStep::Bytes(failure_record()[..2].to_vec()),
        ReadStep::Eof,
    ]);
    assert!(reported);
    assert!(diagnostic.contains("short exec-error report"));
    assert_eq!(remaining, 0);
}

#[test]
fn exec_error_initial_eintr_does_not_lose_failure_record() {
    let (reported, diagnostic, remaining) = drain(vec![
        ReadStep::Error(libc::EINTR),
        ReadStep::Bytes(failure_record()),
    ]);
    assert!(reported);
    assert!(diagnostic.contains("Permission denied"));
    assert_eq!(remaining, 0);
}

#[test]
fn exec_error_mid_record_eintr_preserves_partial_progress() {
    let bytes = failure_record();
    let (reported, diagnostic, remaining) = drain(vec![
        ReadStep::Bytes(bytes[..2].to_vec()),
        ReadStep::Error(libc::EINTR),
        ReadStep::Bytes(bytes[2..].to_vec()),
    ]);
    assert!(reported);
    assert!(diagnostic.contains("Permission denied"));
    assert!(!diagnostic.contains("short exec-error report"));
    assert_eq!(remaining, 0);
}

#[test]
fn exec_error_initial_read_error_is_not_success_eof_or_child_failure() {
    let (reported, diagnostic, remaining) = drain(vec![ReadStep::Error(libc::EBADF)]);
    assert!(!reported, "reader error is not a child failure record");
    assert!(diagnostic.contains("failed to read exec-error report"));
    assert!(diagnostic.contains(&std::io::Error::from_raw_os_error(libc::EBADF).to_string()));
    assert_eq!(remaining, 0);
}

#[test]
fn exec_error_mid_record_read_error_is_not_a_truncated_child_report() {
    let (reported, diagnostic, remaining) = drain(vec![
        ReadStep::Bytes(failure_record()[..2].to_vec()),
        ReadStep::Error(libc::EIO),
    ]);
    assert!(!reported, "reader error is not a child failure record");
    assert!(diagnostic.contains("failed to read exec-error report"));
    assert!(!diagnostic.contains("short exec-error report"));
    assert!(diagnostic.contains(&std::io::Error::from_raw_os_error(libc::EIO).to_string()));
    assert_eq!(remaining, 0);
}

#[test]
fn full_proxy_exec_diagnostic_reaches_caller_while_proxy_sink_is_writable() {
    let proxy = tempfile::NamedTempFile::new().unwrap();
    let caller = tempfile::NamedTempFile::new().unwrap();
    let mut captured = Vec::new();
    let observer = ObservedOutput::shared(4096);
    let target = ParentExecDiagnostic::for_stage(
        Some(PtyChildConfig {
            slave: proxy.as_raw_fd(),
            mode: PtyMode::FullProxy,
        }),
        false,
        proxy.as_raw_fd(),
        caller.as_file().as_fd(),
        &mut captured,
        Some(&observer),
    );
    assert_eq!(
        drain_to(
            vec![ReadStep::Bytes(failure_record())],
            proxy.as_raw_fd(),
            target
        ),
        (true, 0)
    );
    let displayed = std::fs::read_to_string(caller.path()).unwrap();
    assert!(
        displayed.contains("Permission denied"),
        "caller received {displayed:?}"
    );
    assert!(std::fs::read(proxy.path()).unwrap().is_empty());
    assert_eq!(captured, displayed.as_bytes());
    let observed = observer.lock().unwrap().snapshot();
    assert_eq!(observed.stdout, displayed);
    assert!(observed.stderr.is_empty());
    assert_eq!(displayed.matches("chmod(1)").count(), 1);
}

#[test]
fn full_proxy_diagnostic_routing_excludes_redirects_output_only_and_other_stderr() {
    let proxy = tempfile::NamedTempFile::new().unwrap();
    let caller = tempfile::NamedTempFile::new().unwrap();
    for (mode, has_redirect, same_stderr) in [
        (Some(PtyMode::FullProxy), true, true),
        (Some(PtyMode::OutputOnly), false, true),
        (Some(PtyMode::FullProxy), false, false),
        (None, false, true),
    ] {
        let stderr = tempfile::NamedTempFile::new().unwrap();
        let mut captured = Vec::new();
        let observer = ObservedOutput::shared(4096);
        let target = ParentExecDiagnostic::for_stage(
            mode.map(|mode| PtyChildConfig {
                slave: if same_stderr {
                    stderr.as_raw_fd()
                } else {
                    proxy.as_raw_fd()
                },
                mode,
            }),
            has_redirect,
            stderr.as_raw_fd(),
            caller.as_file().as_fd(),
            &mut captured,
            Some(&observer),
        );
        assert_eq!(
            drain_to(
                vec![ReadStep::Bytes(failure_record())],
                stderr.as_raw_fd(),
                target
            ),
            (true, 0)
        );
        assert!(
            std::fs::read_to_string(stderr.path())
                .unwrap()
                .contains("Permission denied")
        );
        assert!(std::fs::read(caller.path()).unwrap().is_empty());
        assert!(captured.is_empty());
        assert!(observer.lock().unwrap().snapshot().is_empty());
    }
}

#[test]
fn full_proxy_handshake_success_partial_and_read_failure_preserve_capture() {
    let bytes = failure_record();
    for (steps, reported, diagnostic) in [
        (vec![ReadStep::Eof], false, ""),
        (
            vec![
                ReadStep::Bytes(bytes[..2].to_vec()),
                ReadStep::Error(libc::EINTR),
                ReadStep::Bytes(bytes[2..].to_vec()),
            ],
            true,
            "Permission denied",
        ),
        (
            vec![ReadStep::Bytes(bytes[..2].to_vec()), ReadStep::Eof],
            true,
            "short exec-error report",
        ),
        (
            vec![ReadStep::Error(libc::EIO)],
            false,
            "failed to read exec-error report",
        ),
    ] {
        let proxy = tempfile::NamedTempFile::new().unwrap();
        let caller = tempfile::NamedTempFile::new().unwrap();
        let mut captured = Vec::new();
        let observer = ObservedOutput::shared(4096);
        let target = ParentExecDiagnostic {
            caller: caller.as_file().as_fd(),
            captured: &mut captured,
            observer: Some(&observer),
        };
        assert_eq!(
            drain_to(steps, proxy.as_raw_fd(), Some(target)),
            (reported, 0)
        );
        let displayed = std::fs::read_to_string(caller.path()).unwrap();
        if diagnostic.is_empty() {
            assert!(displayed.is_empty());
        } else {
            assert!(displayed.contains(diagnostic));
        }
        assert_eq!(captured, displayed.as_bytes());
        let observed = observer.lock().unwrap().snapshot();
        assert_eq!(observed.stdout, displayed);
        assert!(observed.stderr.is_empty());
        assert!(std::fs::read(proxy.path()).unwrap().is_empty());
    }
}

#[test]
fn full_proxy_display_failure_preserves_capture_and_observer() {
    let caller = tempfile::NamedTempFile::new().unwrap();
    let read_only = std::fs::File::open(caller.path()).unwrap();
    let proxy = tempfile::NamedTempFile::new().unwrap();
    let mut captured = Vec::new();
    let observer = ObservedOutput::shared(4096);
    let target = ParentExecDiagnostic {
        caller: read_only.as_fd(),
        captured: &mut captured,
        observer: Some(&observer),
    };
    assert_eq!(
        drain_to(
            vec![ReadStep::Bytes(failure_record())],
            proxy.as_raw_fd(),
            Some(target)
        ),
        (true, 0)
    );
    assert!(std::fs::read(caller.path()).unwrap().is_empty());
    assert!(std::fs::read(proxy.path()).unwrap().is_empty());
    let observed = observer.lock().unwrap().snapshot();
    assert!(observed.stdout.contains("Permission denied"));
    assert_eq!(observed.stdout.as_bytes(), captured);
    assert!(observed.stderr.is_empty());
}

#[test]
fn full_proxy_revoked_slave_does_not_affect_caller_delivery() {
    let pty = nix::pty::openpty(None, None).unwrap();
    drop(pty.master);
    let caller = tempfile::NamedTempFile::new().unwrap();
    let mut captured = Vec::new();
    let target = ParentExecDiagnostic {
        caller: caller.as_file().as_fd(),
        captured: &mut captured,
        observer: None,
    };
    assert_eq!(
        drain_to(
            vec![ReadStep::Bytes(failure_record())],
            pty.slave.as_raw_fd(),
            Some(target)
        ),
        (true, 0)
    );
    let displayed = std::fs::read_to_string(caller.path()).unwrap();
    assert!(displayed.contains("Permission denied"));
    assert_eq!(captured, displayed.as_bytes());
}

#[tokio::test]
async fn full_proxy_diagnostic_history_requires_completion_and_is_recorded_once() {
    use crate::environment::Environment;
    use crate::process::job_pty::capture_completed_output_and_history;
    use crate::process::{Job, JobProcess, ProcessState};
    use nix::sys::signal::Signal;

    let caller = tempfile::NamedTempFile::new().unwrap();
    let proxy = tempfile::NamedTempFile::new().unwrap();
    let observer = ObservedOutput::shared(4096);
    let mut job = Job::new_with_process("fixture".into(), "fixture".into(), vec!["fixture".into()]);
    let target = ParentExecDiagnostic {
        caller: caller.as_file().as_fd(),
        captured: &mut job.parent_exec_diagnostics,
        observer: Some(&observer),
    };
    assert_eq!(
        drain_to(
            vec![ReadStep::Bytes(failure_record())],
            proxy.as_raw_fd(),
            Some(target)
        ),
        (true, 0)
    );
    let displayed = std::fs::read_to_string(caller.path()).unwrap();
    let mut shell = Shell::new(Environment::new());
    let mut ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpgrp(), true);
    ctx.interactive = false;
    ctx.foreground = true;
    ctx.output_observer = Some(observer.clone());
    job.pty_output_task = Some(tokio::spawn(async { Ok("preceding output\n".to_string()) }));
    for state in [
        ProcessState::Running,
        ProcessState::Stopped(nix::unistd::getpid(), Signal::SIGTSTP),
    ] {
        if let Some(JobProcess::Command(process)) = job.process.as_deref_mut() {
            process.state = state;
        }
        assert!(
            capture_completed_output_and_history(&mut job, &ctx, &mut shell)
                .await
                .is_err()
        );
        assert_eq!(job.parent_exec_diagnostics, displayed.as_bytes());
        assert!(job.pty_output_task.is_some());
        assert_eq!(
            shell
                .environment
                .read()
                .session_output_state
                .output_history
                .len(),
            0
        );
    }
    if let Some(JobProcess::Command(process)) = job.process.as_deref_mut() {
        process.state = ProcessState::Completed(126, None);
    }
    job.refresh_lifecycle_state();
    capture_completed_output_and_history(&mut job, &ctx, &mut shell)
        .await
        .unwrap();
    capture_completed_output_and_history(&mut job, &ctx, &mut shell)
        .await
        .unwrap();
    assert!(job.parent_exec_diagnostics.is_empty());
    let env = shell.environment.read();
    let history = &env.session_output_state.output_history;
    assert_eq!(history.len(), 1);
    let entry = history.get(1).unwrap();
    assert_eq!(entry.stdout, format!("preceding output\n{displayed}"));
    assert!(entry.stderr.is_empty());
    assert_eq!(entry.exit_code, 126);
    assert_eq!(observer.lock().unwrap().snapshot().stdout, displayed);
}

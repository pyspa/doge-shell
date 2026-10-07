use super::*;
use std::collections::VecDeque;
use std::os::fd::AsRawFd;

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

fn drain(steps: Vec<ReadStep>) -> (bool, String, usize) {
    let mut steps = VecDeque::from(steps);
    let stderr = tempfile::NamedTempFile::new().unwrap();
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
        stderr.as_raw_fd(),
        &[],
        None,
    );
    let diagnostic = std::fs::read_to_string(stderr.path()).unwrap();
    (reported, diagnostic, steps.len())
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

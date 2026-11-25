//! Mask known credentials and generic secret patterns before trimming output.

use super::Shared;
use std::{
    fs::File,
    io::{ErrorKind, Read},
    os::fd::AsRawFd,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use crate::stream_mask::GenericStreamMask;

pub(super) fn safe_output(text: &str, limit: usize) -> String {
    let masked = dsh_types::safety_policy::redact_sensitive_text(text);
    let clean = escape_display(&masked, true);
    clean[..clean.floor_char_boundary(limit)].to_string()
}

pub(super) fn drain(mut file: File, shared: &Shared) {
    let fd = file.as_raw_fd();
    // SAFETY: file owns this read endpoint until the drain ends.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        shared.interrupt();
        return;
    }
    let mut bytes = [0; 8192];
    let mut finish_started = None;
    let mut generic = GenericStreamMask::new(&shared.secrets);
    loop {
        match file.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => {
                shared.charge(0, count as u64);
                shared.append_output(&generic.push(&bytes[..count]));
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if shared.capture_done.load(Ordering::Acquire) {
                    break;
                }
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: poll references one valid initialized descriptor.
                unsafe {
                    libc::poll(&mut poll, 1, 50);
                }
            }
            Err(_) => break,
        }
        if shared.capture_done.load(Ordering::Acquire)
            && finish_started.get_or_insert_with(Instant::now).elapsed()
                >= Duration::from_millis(250)
        {
            // Drain bytes already in the pipe after reaping, but do not let
            // an unrelated inherited writer prevent the shell from exiting.
            break;
        }
    }
    shared.append_output(&generic.finish());
}

/// Escape invisible/control characters losslessly. Approval fields also escape
/// backslashes, so a literal escape spelling cannot impersonate a real control.
pub(super) fn escape_display(text: &str, multiline: bool) -> String {
    static FORMAT: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\p{Cf}").expect("Unicode format class"));
    let mut output = String::new();
    for ch in text.chars() {
        let mut bytes = [0; 4];
        if !multiline && ch == '\\' {
            output.push_str("\\\\");
        } else if (ch.is_control() && !(multiline && matches!(ch, '\n' | '\t')))
            || FORMAT.is_match(ch.encode_utf8(&mut bytes))
        {
            output.extend(ch.escape_unicode());
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generic_redaction_precedes_the_actual_output_ring_boundary() {
        use dsh_types::background_task::{BudgetLimits, Profile, TaskScope, TaskState};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let scope = TaskScope {
            profile: Profile::ReadOnly,
            cwd: root.clone(),
            read_roots: vec![root],
            write_roots: vec![],
            environment_names: vec![],
            network_hosts: vec![],
            sandbox_verified: false,
            budget: BudgetLimits {
                time_ms: 10000,
                tool_calls: 100,
                tokens: 1000,
                output_bytes: 2000000,
            },
        };
        let shared = Shared::new(
            TaskState::new("mask-test".into(), scope).unwrap(),
            socket,
            None,
            vec!["known-value".into()],
        );
        let mut generic = GenericStreamMask::new(&shared.secrets);
        let input = format!(
            "known-value\nAPI_KEY=SYNTHETIC_SECRET_42\n\n{}",
            "z".repeat(262120)
        );
        for part in [&input.as_bytes()[..17], &input.as_bytes()[17..]] {
            shared.append_output(&generic.push(part));
        }
        shared.append_output(&generic.finish());
        let output = String::from_utf8(shared.output.lock().clone()).unwrap();
        assert!(!output.contains("SYNTHETIC_SECRET_42"));
        assert!(!output.contains("known-value"));
        assert!(output.len() <= super::super::OUTPUT_LIMIT);
        assert!(output.ends_with(&"z".repeat(100)));
    }
}

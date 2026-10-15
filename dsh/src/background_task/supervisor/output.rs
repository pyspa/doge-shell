//! Mask known credentials before merging streams or trimming the output ring.

use super::Shared;
use std::{
    fs::File,
    io::{ErrorKind, Read},
    os::fd::AsRawFd,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

pub(super) struct StreamMask<'a> {
    secrets: &'a [String],
    pending: Vec<u8>,
    withheld: bool,
}

impl<'a> StreamMask<'a> {
    pub(super) fn new(secrets: &'a [String]) -> Self {
        Self {
            secrets,
            pending: Vec::new(),
            withheld: false,
        }
    }
    pub(super) fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        // Pathological credential sizes must not turn masking into an
        // unbounded buffer or expensive repeated prefix comparisons.
        if self.secrets.iter().any(|secret| secret.len() > 4096) {
            if self.withheld {
                return Vec::new();
            }
            self.withheld = true;
            return b"[output withheld: oversized credential]\n".to_vec();
        }
        self.pending.extend_from_slice(bytes);
        let mut output = Vec::new();
        let mut consumed = 0;
        while consumed < self.pending.len() {
            let tail = &self.pending[consumed..];
            // Prefer waiting for a possible longer credential over emitting
            // an overlapping short match. Each stream owns its own suffix.
            if self
                .secrets
                .iter()
                .any(|secret| secret.len() > tail.len() && secret.as_bytes().starts_with(tail))
            {
                break;
            }
            if let Some(secret) = self
                .secrets
                .iter()
                .find(|secret| !secret.is_empty() && tail.starts_with(secret.as_bytes()))
            {
                output.extend_from_slice(b"[REDACTED]");
                consumed += secret.len();
            } else {
                output.push(self.pending[consumed]);
                consumed += 1;
            }
        }
        self.pending.drain(..consumed);
        output
    }
    pub(super) fn finish(&mut self) -> Vec<u8> {
        if self.pending.is_empty() {
            Vec::new()
        } else {
            self.pending.clear();
            b"[REDACTED]".to_vec()
        }
    }
}

pub(super) fn safe_output(text: &str, limit: usize) -> String {
    let masked = dsh_types::safety_policy::redact_sensitive_text(text);
    let clean: String = masked
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect();
    if clean.len() <= limit {
        clean
    } else {
        clean[..clean.floor_char_boundary(limit)].to_string()
    }
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
    let mut mask = StreamMask::new(&shared.secrets);
    loop {
        match file.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => {
                shared.charge(0, count as u64);
                shared.append_output(&mask.push(&bytes[..count]));
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
    shared.append_output(&mask.finish());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_split_across_reads_are_masked_before_tail_truncation() {
        let secrets = vec!["credential-value".into(), "credential".into()];
        for split in 1.."credential-value".len() {
            let mut mask = StreamMask::new(&secrets);
            let mut bytes = mask.push(b"before ");
            bytes.extend(mask.push(&b"credential-value"[..split]));
            bytes.extend(mask.push(&b"credential-value"[split..]));
            bytes.extend(mask.push(b" after"));
            bytes.extend(mask.finish());
            assert_eq!(bytes, b"before [REDACTED] after", "split {split}");
        }
        let mut mask = StreamMask::new(&secrets);
        assert!(mask.push(b"cred").is_empty());
        assert_eq!(mask.finish(), b"[REDACTED]");
        let oversized = vec!["x".repeat(4097)];
        let mut mask = StreamMask::new(&oversized);
        assert!(
            String::from_utf8(mask.push(b"raw secret fragment"))
                .unwrap()
                .contains("withheld")
        );
        assert!(mask.push(b"more raw bytes").is_empty());
    }
}

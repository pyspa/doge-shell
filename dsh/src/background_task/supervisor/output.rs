//! Mask known credentials and generic secret patterns before trimming output.

use super::Shared;
use std::{
    fs::File,
    io::{ErrorKind, Read},
    os::fd::AsRawFd,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

// Buffer only complete, independently redactable pieces. Cross-line patterns
// remain bounded until EOF; overflow withholds the remainder instead of leaking.
const GENERIC_PENDING_LIMIT: usize = 256 * 1024;
const WITHHELD: &[u8] = b"[output withheld: redaction limit or uncertain content]\n";

struct GenericStreamMask<'a> {
    secrets: &'a [String],
    pending: Vec<u8>,
    deferred: bool,
    withheld: bool,
}
impl<'a> GenericStreamMask<'a> {
    fn new(secrets: &'a [String]) -> Self {
        Self {
            secrets,
            pending: Vec::new(),
            deferred: false,
            withheld: false,
        }
    }
    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        if self.withheld {
            return Vec::new();
        }
        let mut output = Vec::new();
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            if self.secrets.iter().any(|secret| secret.len() > 4096)
                || self.pending.len().saturating_add(part.len()) > GENERIC_PENDING_LIMIT
            {
                self.pending.clear();
                self.withheld = true;
                output.extend_from_slice(WITHHELD);
                break;
            }
            self.pending.extend_from_slice(part);
            if part.last() == Some(&b'\n') && !self.deferred {
                let text = String::from_utf8_lossy(&self.pending);
                self.deferred = dsh_types::safety_policy::may_redact_across_lines(&text);
                if !self.deferred {
                    if let Some(masked) = self.masked_pending(false) {
                        output.extend(masked);
                        self.pending.clear();
                    } else {
                        // A known credential can itself cross this newline.
                        self.deferred = true;
                    }
                }
            }
        }
        output
    }
    fn finish(&mut self) -> Vec<u8> {
        if self.withheld || self.pending.is_empty() {
            return Vec::new();
        }
        let output = self
            .masked_pending(true)
            .expect("EOF resolves known suffixes");
        self.pending.clear();
        output
    }
    fn masked_pending(&self, eof: bool) -> Option<Vec<u8>> {
        // Even a partial known credential at EOF is withheld. Inspect original
        // bytes, including overlapping starts that a greedy match would skip.
        let partial = self.secrets.iter().any(|secret| {
            (1..secret.len().min(self.pending.len() + 1)).any(|len| {
                secret
                    .as_bytes()
                    .starts_with(&self.pending[self.pending.len() - len..])
            })
        });
        if partial {
            return if eof { Some(WITHHELD.to_vec()) } else { None };
        }
        let text = String::from_utf8_lossy(&self.pending);
        Some(
            dsh_types::safety_policy::redact_sensitive_text_with_known(&text, self.secrets)
                .text
                .into_bytes(),
        )
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_split_across_reads_are_masked_before_tail_truncation() {
        let secrets = vec!["credential-value".into(), "credential".into()];
        for split in 1.."credential-value".len() {
            let mut mask = GenericStreamMask::new(&secrets);
            let mut bytes = mask.push(b"before ");
            bytes.extend(mask.push(&b"credential-value"[..split]));
            bytes.extend(mask.push(&b"credential-value"[split..]));
            bytes.extend(mask.push(b" after"));
            bytes.extend(mask.finish());
            assert_eq!(bytes, b"before *** after", "split {split}");
        }
        let mut mask = GenericStreamMask::new(&secrets);
        assert!(mask.push(b"cred").is_empty());
        assert_eq!(mask.finish(), WITHHELD);
        let oversized = vec!["x".repeat(4097)];
        let mut mask = GenericStreamMask::new(&oversized);
        assert_eq!(mask.push(b"raw secret fragment"), WITHHELD);
        assert!(mask.push(b"more raw bytes").is_empty());
    }

    fn generic_chunks(input: &[u8], chunk: usize) -> Vec<u8> {
        let mut mask = GenericStreamMask::new(&[]);
        let mut output = Vec::new();
        for part in input.chunks(chunk) {
            output.extend(mask.push(part));
            assert!(mask.pending.len() <= GENERIC_PENDING_LIMIT);
        }
        output.extend(mask.finish());
        output
    }

    #[test]
    fn generic_patterns_match_canonical_redaction_at_every_split_and_eof() {
        for text in [
            "before API_KEY=synthetic-value after\n",
            "before ?token=synthetic-value&ok=1\n",
            "--password=synthetic-value\n",
            "--api-key \"synthetic value\"\n",
            "-p 'synthetic value'",
            "--password\nsynthetic-value",
            "--password \"synthetic\nvalue\"",
            "Authorization:\nBearer\nsynthetic-value",
            "authorization\n :\n bearer\nsynthetic-value\n",
            "Authorization: Bearer synthetic-value",
            "-----BEGIN RSA PRIVATE KEY-----\n",
            "unicode \u{017f}ecret=synthetic-value\n",
        ] {
            let expected = dsh_types::safety_policy::redact_sensitive_text(text);
            for split in 0..=text.len() {
                let mut mask = GenericStreamMask::new(&[]);
                let mut actual = mask.push(&text.as_bytes()[..split]);
                actual.extend(mask.push(&text.as_bytes()[split..]));
                actual.extend(mask.finish());
                assert_eq!(actual, expected.as_bytes(), "split {split}: {text:?}");
            }
            assert_eq!(generic_chunks(text.as_bytes(), 1), expected.as_bytes());
        }
        for text in ["--password \"unfinished", "--password 'unfinished\nvalue"] {
            assert_eq!(generic_chunks(text.as_bytes(), 1), b"--password ***");
        }
    }

    #[test]
    fn ordinary_lines_flush_but_cross_line_candidates_wait_for_eof() {
        let mut stdout = GenericStreamMask::new(&[]);
        let mut stderr = GenericStreamMask::new(&[]);
        assert_eq!(stdout.push(b"ordinary\n"), b"ordinary\n");
        assert!(stdout.push(b"Authorization:\n").is_empty());
        assert_eq!(stderr.push(b"independent\n"), b"independent\n");
        assert!(stdout.push(b"Bearer value\n").is_empty());
        assert_eq!(stdout.finish(), b"Authorization:\nBearer ***\n");
        assert!(stdout.finish().is_empty());
    }

    #[test]
    fn generic_buffer_limits_withhold_the_remainder_without_raw_eof() {
        for text in [
            "x".repeat(GENERIC_PENDING_LIMIT + 1),
            format!("API_KEY={}", "x".repeat(GENERIC_PENDING_LIMIT)),
            format!(
                "Authorization:{}Bearer value",
                "\n".repeat(GENERIC_PENDING_LIMIT)
            ),
            format!("--password \"{}\"", "x".repeat(GENERIC_PENDING_LIMIT)),
        ] {
            assert_eq!(generic_chunks(text.as_bytes(), 8192), WITHHELD);
        }
        let mut mask = GenericStreamMask::new(&[]);
        assert_eq!(mask.push(&vec![b'x'; GENERIC_PENDING_LIMIT + 1]), WITHHELD);
        assert!(mask.pending.is_empty());
        assert!(mask.push(b"secret suffix\n").is_empty());
        assert!(mask.finish().is_empty());
    }

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

    #[test]
    fn known_and_generic_redactors_cannot_destroy_each_others_recognition() {
        for (secret, text) in [
            ("secret", "--secret secondary-private-value\n"),
            (
                "Authorization",
                "Authorization:\nBearer secondary-private-value",
            ),
            ("API_KEY", "API_KEY=secondary-private-value\n"),
            ("API_KEY=known-value", "API_KEY=known-value\n"),
            ("known\nAPI_KEY=value", "known\nAPI_KEY=value\n"),
            ("partial\ncredential", "partial\ncreden"),
        ] {
            let secrets = vec![secret.to_string()];
            for split in 0..=text.len() {
                let mut mask = GenericStreamMask::new(&secrets);
                let mut output = mask.push(&text.as_bytes()[..split]);
                output.extend(mask.push(&text.as_bytes()[split..]));
                output.extend(mask.finish());
                let output = String::from_utf8(output).unwrap();
                assert!(
                    !output.contains("secondary-private-value"),
                    "{secret:?}, split {split}: {output:?}"
                );
                assert!(
                    !output.contains(secret),
                    "{secret:?}, split {split}: {output:?}"
                );
                assert!(!output.contains("partial\ncreden"));
            }
        }
        let oversized = vec!["x".repeat(4097)];
        let mut mask = GenericStreamMask::new(&oversized);
        assert_eq!(mask.push(b"raw suffix\n"), WITHHELD);
        assert!(mask.push(b"still withheld").is_empty());
        assert!(mask.finish().is_empty());
    }
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
mod union_stream_tests {
    use super::*;
    #[test]
    fn original_spans_survive_every_stream_split_including_overlapping_credentials() {
        for (known, input, expected) in [
            ("TOKEN", "TOKEN=private-dummy-value\n", "***=***\n"),
            ("aba", "ababa\n", "***\n"),
            ("unused-key", "TOKEN=private\\\nsecond\n", "TOKEN=***\n"),
            (
                "unused-key",
                "PUBLIC=\"hello TOKEN='private\nsecond'\"\n",
                "PUBLIC=\"hello TOKEN=***",
            ),
        ] {
            let secrets = vec![known.into()];
            for split in 0..=input.len() {
                let mut mask = GenericStreamMask::new(&secrets);
                let mut output = mask.push(&input.as_bytes()[..split]);
                output.extend(mask.push(&input.as_bytes()[split..]));
                output.extend(mask.finish());
                assert_eq!(output, expected.as_bytes(), "split {split}");
            }
        }
    }
}

//! Retain bounded, redacted logs while hashing all stdout, including omitted bytes.
use super::super::store::MAX_STREAM_BYTES;
use crate::stream_mask::GenericStreamMask;
use std::collections::VecDeque;

const HALF: usize = MAX_STREAM_BYTES / 2;
const GAP: &str = "\n... [truncated] ...\n";
const LINE_LIMIT: usize = 256 * 1024;
const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

pub(super) struct Capture {
    mask: GenericStreamMask<'static>,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    truncated: bool,
    preview: String,
}

impl Capture {
    pub(super) fn new() -> Self {
        Self {
            mask: GenericStreamMask::new(&[]),
            head: Vec::new(),
            tail: VecDeque::new(),
            truncated: false,
            preview: String::new(),
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) {
        let redacted = self.mask.push(bytes);
        self.retain(&redacted);
    }

    fn retain(&mut self, bytes: &[u8]) {
        if self.preview.is_empty() && !bytes.is_empty() {
            // The mask emits complete lines (or its last fragment at EOF).
            let text = std::str::from_utf8(bytes).expect("redacted UTF-8");
            if !text.trim().is_empty() {
                self.preview = super::preview(text);
            }
        }
        let split = (HALF - self.head.len()).min(bytes.len());
        self.head.extend_from_slice(&bytes[..split]);
        let rest = &bytes[split..];
        let excess = (self.tail.len() + rest.len()).saturating_sub(HALF);
        self.truncated |= excess > 0;
        self.tail.drain(..excess.min(self.tail.len()));
        self.tail.extend(&rest[rest.len().saturating_sub(HALF)..]);
    }

    pub(super) fn finish(mut self) -> (String, Option<String>) {
        let last = self.mask.finish();
        self.retain(&last);
        if self.truncated {
            // The mask emits valid UTF-8. Only the two cut points may split
            // a scalar. Reserve space for the marker inside the store limit.
            self.head.truncate(HALF - GAP.len());
            if let Err(error) = std::str::from_utf8(&self.head) {
                self.head.truncate(error.valid_up_to());
            }
            self.head.extend_from_slice(GAP.as_bytes());
            while self.tail.front().is_some_and(|byte| byte & 0xc0 == 0x80) {
                self.tail.pop_front();
            }
        }
        self.head.extend(self.tail);
        (
            String::from_utf8(self.head).expect("redacted UTF-8"),
            (!self.preview.is_empty()).then_some(self.preview),
        )
    }
}

pub(super) struct Digest {
    hash: u64,
    line: Vec<u8>,
    oversized: bool,
}

impl Digest {
    pub(super) fn new() -> Self {
        Self {
            hash: OFFSET,
            line: Vec::new(),
            oversized: false,
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) {
        for segment in bytes.split_inclusive(|b| *b == b'\n') {
            let end = segment.ends_with(b"\n");
            let data = &segment[..segment.len() - usize::from(end)];
            if !self.oversized && self.line.len() + data.len() > LINE_LIMIT {
                hash_bytes(&mut self.hash, &self.line);
                self.line.clear();
                self.oversized = true;
            }
            if self.oversized {
                hash_bytes(&mut self.hash, data);
            } else {
                self.line.extend_from_slice(data);
            }
            if end {
                self.end_line();
            }
        }
    }

    fn end_line(&mut self) {
        if !self.oversized {
            let text = String::from_utf8_lossy(&self.line);
            hash_bytes(
                &mut self.hash,
                console::strip_ansi_codes(&text).trim_end().as_bytes(),
            );
        }
        hash_bytes(&mut self.hash, b"\n");
        self.line.clear();
        self.oversized = false;
    }

    pub(super) fn finish(mut self) -> u64 {
        if !self.line.is_empty() || self.oversized {
            self.end_line();
        }
        self.hash
    }
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_includes_omitted_lines_and_ignores_read_boundaries() {
        let giant = vec![b'x'; LINE_LIMIT + 1];
        let mut bytes = b"\x1b[31mfirst\x1b[0m   \r\n\n".to_vec();
        bytes.extend_from_slice(&giant);
        bytes.extend_from_slice(b"\nlast");
        let mut expected = OFFSET;
        hash_bytes(&mut expected, b"first\n\n");
        hash_bytes(&mut expected, &giant);
        hash_bytes(&mut expected, b"\nlast\n");
        for size in [1, 16384, bytes.len()] {
            let mut digest = Digest::new();
            for chunk in bytes.chunks(size) {
                digest.push(chunk);
                assert!(digest.line.len() <= LINE_LIMIT);
            }
            assert_eq!(digest.finish(), expected);
        }
    }

    #[test]
    fn truncation_keeps_valid_masked_head_and_tail() {
        let mut capture = Capture::new();
        let input = format!(
            "first\n{}API_KEY='private\nvalue'\nlast\n",
            "🐕\n".repeat(5000)
        );
        for chunk in input.as_bytes().chunks(97) {
            capture.push(chunk);
            assert!(capture.head.len() <= HALF && capture.tail.len() <= HALF);
        }
        let (log, preview) = capture.finish();
        assert_eq!(preview.as_deref(), Some("first"));
        assert!(log.starts_with("first\n"));
        assert!(log.contains(GAP));
        assert!(log.ends_with("API_KEY=***\nlast\n"), "{log}");
        assert!(log.len() <= MAX_STREAM_BYTES);
    }
}

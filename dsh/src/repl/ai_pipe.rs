//! Parsing and bounded output capture for `command |! query`.
//!
//! The model only ever sees `AI_PIPE_OUTPUT_CHARS` of the output, so keeping
//! everything a command prints (`yes |! x`, `tail -f log |! x`) would grow
//! memory without limit for nothing. Each stream keeps its first and last
//! `limit / 2` bytes while it is read; the middle is dropped as it streams.
use std::collections::VecDeque;
use std::io::{ErrorKind, Read};
use std::process::{Command, ExitStatus, Stdio};

/// Bytes kept per stream (head half plus tail half).
pub(super) const AI_PIPE_CAPTURE_BYTES: usize = 256 * 1024;

const TRUNCATION_MARKER: &[u8] = b"\n...[output truncated]...\n";

pub(super) struct AiPipeCapture {
    pub stdout: String,
    pub stderr: String,
    pub status: ExitStatus,
}

/// Runs `command` with stdin detached and both output streams bounded.
///
/// The child stays in the caller's process group, so a terminal Ctrl-C
/// (with raw mode paused by the caller) reaches it and ends the capture.
pub(super) fn capture(mut command: Command, limit: usize) -> std::io::Result<AiPipeCapture> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stderr_reader = std::thread::spawn(move || {
        stderr
            .map(|stream| read_head_tail(stream, limit))
            .unwrap_or_default()
    });
    let stdout = stdout
        .map(|stream| read_head_tail(stream, limit))
        .unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let status = child.wait()?;
    Ok(AiPipeCapture {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        status,
    })
}

/// Whether the user stopped a `|!` capture with Ctrl-C: the shell saw
/// SIGINT, or the child died of it. Either way nothing is sent to the AI.
pub(super) fn was_interrupted(got_sigint: bool, capture: Option<&AiPipeCapture>) -> bool {
    use std::os::unix::process::ExitStatusExt as _;
    got_sigint || capture.is_some_and(|capture| capture.status.signal() == Some(nix::libc::SIGINT))
}

fn read_head_tail(mut reader: impl Read, limit: usize) -> Vec<u8> {
    let half = limit / 2;
    let mut head = Vec::new();
    let mut tail: VecDeque<u8> = VecDeque::new();
    let mut dropped = false;
    let mut buf = [0u8; 8192];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            // The shell's SIGINT handler is installed without SA_RESTART.
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let mut chunk = &buf[..n];
        if head.len() < half {
            let take = (half - head.len()).min(chunk.len());
            head.extend_from_slice(&chunk[..take]);
            chunk = &chunk[take..];
        }
        tail.extend(chunk);
        if tail.len() > half {
            let excess = tail.len() - half;
            tail.drain(..excess);
            dropped = true;
        }
    }
    if dropped {
        head.extend_from_slice(TRUNCATION_MARKER);
    }
    head.extend(tail);
    head
}

/// Splits `command |! query` at the first top-level `|!`. A `|!` inside
/// quotes or escapes belongs to the command, so `grep 'a|!b' f` is not an AI
/// pipe. Surrounding quotes around the query are removed.
pub(crate) fn parse_ai_pipe(input: &str) -> Option<(String, String)> {
    let idx = crate::completion::command_segment::ai_pipe_marker(input)?;
    let command = input[..idx].trim();
    let query_part = input[idx + 2..].trim();
    let query = query_part
        .strip_prefix('"')
        .and_then(|q| q.strip_suffix('"'))
        .or_else(|| {
            query_part
                .strip_prefix('\'')
                .and_then(|q| q.strip_suffix('\''))
        })
        .unwrap_or(query_part);

    if command.is_empty() || query.is_empty() {
        return None;
    }
    Some((command.to_string(), query.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(line: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(line);
        command
    }

    #[test]
    fn small_output_is_kept_whole() {
        let out = capture(sh("printf 'out'; printf 'err' >&2; exit 3"), 1024).unwrap();
        assert_eq!(out.stdout, "out");
        assert_eq!(out.stderr, "err");
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn large_output_keeps_head_and_tail_within_the_limit() {
        let out = capture(
            sh("printf 'HEAD'; head -c 1000000 /dev/zero | tr '\\0' x; printf 'TAIL'"),
            1000,
        )
        .unwrap();
        assert!(out.stdout.starts_with("HEAD"), "{:?}", &out.stdout[..10]);
        assert!(out.stdout.ends_with("TAIL"));
        assert!(out.stdout.contains("[output truncated]"));
        assert!(out.stdout.len() <= 1000 + TRUNCATION_MARKER.len());
    }

    #[test]
    fn a_child_killed_by_ctrl_c_counts_as_interrupted() {
        let killed = capture(sh("printf partial; kill -INT $$"), 1024).unwrap();
        assert_eq!(killed.stdout, "partial");
        assert!(was_interrupted(false, Some(&killed)));

        let finished = capture(sh("exit 130"), 1024).unwrap();
        assert!(!was_interrupted(false, Some(&finished)));
        // The shell's own SIGINT flag wins even when the child ignored it.
        assert!(was_interrupted(true, Some(&finished)));
        assert!(was_interrupted(true, None));
        assert!(!was_interrupted(false, None));
    }

    #[test]
    fn stdin_is_detached_so_readers_do_not_block() {
        let out = capture(sh("cat; echo done"), 1024).unwrap();
        assert_eq!(out.stdout, "done\n");
    }

    #[test]
    fn ai_pipe_marker_inside_quotes_or_escapes_is_not_an_ai_pipe() {
        for input in [
            "grep 'foo|!bar' file.txt",
            "echo \"a |! b\"",
            "echo a \\|! b",
            "echo # |! comment",
            "echo $(printf '|!')",
        ] {
            assert_eq!(parse_ai_pipe(input), None, "{input:?}");
        }
    }

    #[test]
    fn ai_pipe_query_may_contain_another_marker() {
        assert_eq!(
            parse_ai_pipe("grep 'a|!b' f |! what's the |! about"),
            Some((
                "grep 'a|!b' f".to_string(),
                "what's the |! about".to_string()
            ))
        );
    }
}

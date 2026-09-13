//! Running the shell command and capturing its output: the bounded, keep-both-ends byte log (`CappedCapture`), draining a child's pipes on background threads so a full pipe buffer can't deadlock the wait
//! (`drain_pipe`/`DrainedPipe`), the timeout/cancel-aware run loop
//! (`run_with_timeout_cancel`), and rendering the JSON result the model sees
//! (`render_result`).
use super::*;
use std::collections::VecDeque;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Serialize the result so that it survives the global tool-output cap intact.
///
/// The per-stream budgets bound the *raw* text, but JSON escaping can double or
/// sextuple it. Handing an oversized object to the shared truncator produced a
/// middle-cut, unparseable JSON document.
pub(super) fn render_result(
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    note: Option<String>,
) -> String {
    let mut budget = MAX_STREAM_CHARS;

    loop {
        let mut result = json!({
            "exit_code": exit_code,
            "stdout": dsh_openai::turn::truncate_middle(stdout, budget),
            "stderr": dsh_openai::turn::truncate_middle(stderr, budget),
        });

        if let Some(note) = &note
            && let Some(map) = result.as_object_mut()
        {
            map.insert("note".into(), json!(note));
        }

        let rendered = result.to_string();
        if rendered.len() <= super::super::MAX_OUTPUT_LENGTH || budget <= MIN_STREAM_CHARS {
            return rendered;
        }

        budget /= 2;
    }
}
pub(super) struct CapturedRun {
    pub(super) status: Option<ExitStatus>,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    pub(super) timed_out: bool,
    pub(super) cancelled: bool,
    /// The readers never reached end of stream, so what follows is whatever had
    /// arrived when the grace period ran out.
    pub(super) drain_incomplete: bool,
}
/// Drain a child pipe on its own thread, into a buffer the caller can read at
/// any time.
///
/// Polling only for exit deadlocks as soon as the child fills a pipe buffer, so
/// both streams have to be read while we wait. The buffer is shared rather than
/// sent once at EOF: a surviving grandchild holds the write end open, so EOF may
/// never arrive, and waiting for a single end-of-stream message meant giving up
/// with *nothing* — a command whose output had already been read in full still
/// reported an empty stdout.
///
/// The pipe keeps being drained past `MAX_CAPTURED_BYTES`, so a chatty child
/// never blocks on a full pipe while memory stays bounded.
fn drain_pipe<R>(pipe: Option<R>) -> DrainedPipe
where
    R: Read + Send + 'static,
{
    let drained = DrainedPipe {
        buffer: Arc::new(Mutex::new(CappedCapture::default())),
        at_eof: Arc::new(AtomicBool::new(false)),
    };
    let writer = Arc::clone(&drained.buffer);
    let at_eof = Arc::clone(&drained.at_eof);

    std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut chunk = [0_u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        // A panic elsewhere must not cost us the output we
                        // already have: the buffer is a plain byte log, so a
                        // poisoned lock has nothing broken to protect.
                        let mut buffer = writer
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        buffer.push(&chunk[..read]);
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        at_eof.store(true, Ordering::Release);
    });

    drained
}
/// A bounded byte log that keeps both ends of what it was given.
///
/// Compiler errors, test failures and stack traces live at the *end* of a
/// command's output — the same reason `truncate_middle` cuts the middle — so a
/// cap that keeps the first N bytes and throws the rest away hides the very
/// thing the model has to react to.
#[derive(Default)]
pub(super) struct CappedCapture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    dropped: usize,
}
impl CappedCapture {
    const HEAD_BYTES: usize = MAX_CAPTURED_BYTES / 2;
    const TAIL_BYTES: usize = MAX_CAPTURED_BYTES - Self::HEAD_BYTES;

    pub(super) fn push(&mut self, mut bytes: &[u8]) {
        let head_room = Self::HEAD_BYTES.saturating_sub(self.head.len());
        if head_room > 0 {
            let take = head_room.min(bytes.len());
            self.head.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }

        self.tail.extend(bytes);
        while self.tail.len() > Self::TAIL_BYTES {
            self.tail.pop_front();
            self.dropped += 1;
        }
    }

    pub(super) fn snapshot(&self) -> Vec<u8> {
        let mut out = self.head.clone();
        if self.dropped > 0 {
            out.extend_from_slice(
                format!(
                    "\n... (dropped {} bytes from the middle of the capture) ...\n",
                    self.dropped
                )
                .as_bytes(),
            );
        }
        out.extend(self.tail.iter().copied());
        out
    }
}
/// A pipe being drained in the background: what has been read so far, and
/// whether the reader reached the end of the stream.
struct DrainedPipe {
    buffer: Arc<Mutex<CappedCapture>>,
    at_eof: Arc<AtomicBool>,
}
impl DrainedPipe {
    fn at_eof(&self) -> bool {
        self.at_eof.load(Ordering::Acquire)
    }

    /// Whatever the drain thread has collected so far.
    ///
    /// Called once the child is gone (or the deadline passed): the reader may
    /// still be blocked on a grandchild's copy of the write end, and its
    /// progress is worth more than the EOF that is never coming.
    fn snapshot(&self) -> Vec<u8> {
        self.buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .snapshot()
    }
}
/// Run `command` under `sh -c`.
///
/// Direct `Command::new` execution meant no pipes, no redirection, no `&&` and
/// no globbing, so `cargo test 2>&1 | tail -40` could not be expressed at all
/// and every multi-step job cost one round trip per step. The shell here is
/// what makes the command line real; what makes it safe is `authorize`, which
/// has already put the whole line through the shell's own parser and safety
/// guard.
pub(super) fn run_with_timeout_cancel(
    command: &str,
    cwd: Option<&Path>,
    timeout: Duration,
    cancel: &dyn Fn() -> bool,
) -> Result<CapturedRun, String> {
    let mut builder = Command::new("sh");
    builder
        .arg("-c")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group, so a timeout can take the whole tree down instead
        // of orphaning whatever the command spawned.
        .process_group(0);

    if let Some(cwd) = cwd {
        builder.current_dir(cwd);
    }

    let mut child = builder.spawn().map_err(|err| err.to_string())?;

    let stdout_reader = drain_pipe(child.stdout.take());
    let stderr_reader = drain_pipe(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(err) => return Err(err.to_string()),
        }

        cancelled = cancel();
        if cancelled || Instant::now() >= deadline {
            kill_process_group(&child);
            let _ = child.kill();
            let _ = child.wait();
            timed_out = !cancelled;
            break None;
        }

        std::thread::sleep(TIMEOUT_POLL_INTERVAL);
    };

    // Bounded: a surviving grandchild still holds the write end of the pipe, so
    // EOF may never arrive. Give the readers a moment to catch up with what the
    // child already wrote, then take whatever they have.
    let drained = wait_for_drain(&[&stdout_reader, &stderr_reader], DRAIN_GRACE);
    let stdout = stdout_reader.snapshot();
    let stderr = stderr_reader.snapshot();

    Ok(CapturedRun {
        status,
        stdout,
        stderr,
        timed_out,
        cancelled,
        drain_incomplete: !drained,
    })
}
/// Wait for the readers to reach end of stream, or for `grace` to run out.
///
/// A normal command hits EOF within microseconds of exiting; only a surviving
/// grandchild holding the write end open runs the clock down, and that is
/// exactly the case the grace period bounds.
///
/// Returns whether every reader got there, so the caller can say so when the
/// output it hands back is only as much as had arrived.
fn wait_for_drain(readers: &[&DrainedPipe], grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if readers.iter().all(|reader| reader.at_eof()) {
            return true;
        }
        std::thread::sleep(DRAIN_POLL_INTERVAL);
    }
    readers.iter().all(|reader| reader.at_eof())
}
/// Signal the whole group the child leads, so background grandchildren die too.
pub(crate) fn kill_process_group(child: &std::process::Child) {
    let Ok(pid) = i32::try_from(child.id()) else {
        return;
    };
    // `process_group(0)` made the child its own group leader, so pgid == pid.
    let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), nix::sys::signal::SIGKILL);
}

//! Background output capture monitors.
//!
//! One [`OutputMonitor`] owns the read end of a capture pipe created for a
//! background (or observed foreground) external command. The reader fd is
//! always `O_NONBLOCK` (a structural invariant established in
//! [`OutputMonitor::new`]), wrapped in `tokio::io::unix::AsyncFd` so the
//! explicit-ownership path ([`OutputMonitor::drain_to_eof`]) can wait for
//! readiness without a thread. The ready-now paths read the fd directly
//! and never wait.
//!
//! Three drain semantics share one newline-framing parser:
//!
//! - running job: [`OutputMonitor::drain_ready_now`]
//!   (wait-free, byte-budgeted, partial retained),
//! - completed-job reconciliation: [`OutputMonitor::finalize_ready_now`]
//!   (sync, never waits: drains whatever the kernel already holds until
//!   `WouldBlock`, then publishes the pending fragment because monitor
//!   ownership ends here),
//! - explicit ownership waits (`wait PID`, `fg`):
//!   [`OutputMonitor::drain_to_eof`] (blocks until EOF).
//!
//! `finalize_ready_now` never touches the Tokio readiness cache: after
//! `waitpid` observes completion, bytes the child wrote before exiting are
//! already committed to the pipe, and a direct non-blocking `read` recovers
//! them even if the reactor has not delivered a readiness event yet.
//!
//! Terminal rendering is best-effort and isolated from pipe ownership:
//! once rendering fails, this monitor stops rendering but continues draining,
//! capturing and observing output for the rest of its lifetime.

use anyhow::{Context as _, Result};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use std::borrow::Cow;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::OwnedFd;

use crate::terminal::renderer::TerminalRenderer;
use dsh_types::observed_output::{ObservedStream, SharedOutputObserver};

const RUNNING_DRAIN_BUDGET_BYTES: usize = 256 * 1024;
const FIRST_MONITOR_OUTPUT_PREFIX: &[u8] = b"\r\n";
const READ_CHUNK_BYTES: usize = 4096;
const RENDER_FLUSH_BYTES: usize = 8192;
// NOTE: `MAX_PENDING_CONTROL_BYTES` stays in `super` (io.rs): it belongs to
// `PtyDisplayBuffer`, which did not move.

fn append_output_chunk(output_started: &mut bool, buffer: &mut Vec<u8>, chunk: &[u8]) {
    if !*output_started {
        *output_started = true;
        buffer.extend_from_slice(FIRST_MONITOR_OUTPUT_PREFIX);
    }
    buffer.extend_from_slice(chunk);
}

fn is_would_block(err: &std::io::Error) -> bool {
    err.kind() == ErrorKind::WouldBlock || err.raw_os_error() == Some(libc::EAGAIN)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadyDrainOutcome {
    bytes_read: usize,
    eof: bool,
    budget_exhausted: bool,
}

#[derive(Debug)]
pub struct OutputMonitor {
    /// Non-blocking pipe reader. Readiness waiting
    /// ([`OutputMonitor::drain_to_eof`])
    /// goes through the `AsyncFd` reactor side; the ready-now paths
    /// ([`OutputMonitor::drain_ready_now`],
    /// [`OutputMonitor::finalize_ready_now`]) read the underlying fd
    /// directly so a not-yet-dispatched readiness event cannot hide bytes
    /// the kernel already holds.
    inner: tokio::io::unix::AsyncFd<std::fs::File>,
    /// Incomplete record bytes kept across calls. New chunk bytes are
    /// appended here first, so a multi-byte character split across reads
    /// is only projected to the text observer once its record completes.
    /// Published only on newline, at EOF, or at monitor retirement
    /// (`finalize_ready_now`); a running ready-now drain never publishes it.
    pending_line: Vec<u8>,
    pub(crate) outputed: bool,
    pub captured_output: Vec<u8>,
    // Cached renderer to avoid repeated allocations.
    // Safe to hold as it no longer holds StdoutLock persistently.
    pub(crate) renderer: TerminalRenderer,
    renderer_failed: bool,
    observer: Option<SharedOutputObserver>,
    observed_stream: ObservedStream,
}

impl OutputMonitor {
    /// Take ownership of a capture-pipe read end. The fd is switched to
    /// `O_NONBLOCK` here, so every later read on it is wait-free by
    /// construction.
    ///
    /// Must run before the child spawns: on failure the `OwnedFd` drops and
    /// closes the read end (the caller still owns the write end), and no
    /// child exists yet, so no orphan path opens.
    pub fn new(
        fd: OwnedFd,
        observer: Option<SharedOutputObserver>,
        observed_stream: ObservedStream,
    ) -> Result<Self> {
        let file = std::fs::File::from(fd);
        let current = fcntl(&file, FcntlArg::F_GETFL).context("fcntl F_GETFL failed")?;
        let flags = OFlag::from_bits_truncate(current) | OFlag::O_NONBLOCK;
        fcntl(&file, FcntlArg::F_SETFL(flags)).context("fcntl F_SETFL failed")?;
        let inner = tokio::io::unix::AsyncFd::new(file).context("OutputMonitor AsyncFd failed")?;
        Ok(OutputMonitor {
            inner,
            pending_line: Vec::new(),
            outputed: false,
            captured_output: Vec::new(),
            renderer: TerminalRenderer::new(),
            renderer_failed: false,
            observer,
            observed_stream,
        })
    }

    pub(crate) fn stream(&self) -> ObservedStream {
        self.observed_stream
    }

    /// Text projection of the raw capture for text-only consumers
    /// (`OutputHistory`). The authority stays [`Self::captured_output`]
    /// (`Vec<u8>`); this converts at the boundary only.
    pub(crate) fn captured_text_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.captured_output)
    }

    fn append_line(&mut self, buffer: &mut Vec<u8>, line: &[u8]) {
        append_output_chunk(&mut self.outputed, buffer, line);
        // Raw capture: never lossy-convert here; text projection happens
        // only for the text-only observer below.
        self.captured_output.extend_from_slice(line);
        if let Some(observer) = &self.observer
            && let Ok(mut observer) = observer.lock()
        {
            observer.append(self.observed_stream, &String::from_utf8_lossy(line));
        }
    }

    fn flush_terminal(renderer: &mut TerminalRenderer, buffer: &[u8]) -> Result<()> {
        if buffer.is_empty() {
            return Ok(());
        }

        renderer.write_all(buffer)?;
        renderer.flush()?;
        Ok(())
    }

    /// Best-effort display flush: a renderer failure disables rendering for
    /// the rest of this monitor's lifetime (sticky `renderer_failed`), logs
    /// one diagnostic, and never propagates as a drain `Err`. The display
    /// scratch buffer is always cleared so a long-running job cannot grow it
    /// without bound; capture and observer data is already recorded.
    fn flush_display_with<F>(&mut self, display: &mut Vec<u8>, flush: &mut F)
    where
        F: FnMut(&mut TerminalRenderer, &[u8]) -> Result<()>,
    {
        if display.is_empty() {
            return;
        }

        if !self.renderer_failed
            && let Err(err) = flush(&mut self.renderer, display)
        {
            self.renderer_failed = true;
            tracing::warn!(
                stream = ?self.observed_stream,
                error = %err,
                "OutputMonitor renderer failed; terminal rendering disabled for this monitor"
            );
        }

        display.clear();
    }

    /// Publish one complete record exactly once to the renderer buffer, the
    /// raw capture, and the observer.
    ///
    /// Raw bytes are authoritative: capture and terminal rendering keep
    /// them byte-exact, including invalid UTF-8. Only the text-only
    /// `SharedOutputObserver` receives a `String::from_utf8_lossy` projection,
    /// computed after the record completes so a valid multi-byte character
    /// split across reads is never replaced with U+FFFD. Invalid UTF-8 is
    /// presentation data, never a drain error.
    fn publish_record(&mut self, buffer: &mut Vec<u8>, record: &[u8]) {
        self.append_line(buffer, record);
    }

    /// Frame raw chunk bytes into newline-terminated records.
    ///
    /// A single `read` can return many lines (or a trailing fragment), so
    /// one syscall is never assumed to be one record. Complete records are
    /// published byte-exact; the trailing fragment stays in `pending_line`.
    /// Newline `0x0A` never appears inside a UTF-8 multi-byte sequence, so
    /// record framing never splits a character for the observer projection.
    fn feed_bytes(&mut self, display: &mut Vec<u8>, bytes: &[u8]) {
        self.pending_line.extend_from_slice(bytes);
        while let Some(newline) = self.pending_line.iter().position(|&byte| byte == b'\n') {
            let record: Vec<u8> = self.pending_line.drain(..=newline).collect();
            self.publish_record(display, &record);
        }
    }

    /// Publish the pending fragment as the final record. Used at EOF and at
    /// monitor retirement, where keeping it would lose it with the monitor.
    fn flush_pending_fragment(&mut self, display: &mut Vec<u8>) {
        if self.pending_line.is_empty() {
            return;
        }
        let fragment = std::mem::take(&mut self.pending_line);
        self.publish_record(display, &fragment);
    }

    /// Running-job drain: consume whatever the kernel already holds without
    /// waiting for anything.
    ///
    /// Sync by design: there is nothing to await. Reads the `O_NONBLOCK` fd
    /// directly until `WouldBlock`, EOF, or the per-call byte budget, so a
    /// silent monitor returns after ~one syscall and a chatty monitor cannot
    /// monopolize a lifecycle scan. A partial line with no newline stays in
    /// `pending_line` for a future call; only EOF publishes it early.
    pub fn drain_ready_now(&mut self) -> Result<()> {
        let mut flush = Self::flush_terminal;
        let _ = self.drain_ready_now_with_budget(RUNNING_DRAIN_BUDGET_BYTES, &mut flush)?;
        Ok(())
    }

    fn drain_ready_now_with_budget<F>(
        &mut self,
        budget: usize,
        flush: &mut F,
    ) -> Result<ReadyDrainOutcome>
    where
        F: FnMut(&mut TerminalRenderer, &[u8]) -> Result<()>,
    {
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        let mut display = Vec::new();
        let mut bytes_read = 0usize;
        let mut eof = false;
        let mut budget_exhausted = false;
        loop {
            if bytes_read >= budget {
                budget_exhausted = true;
                break;
            }
            let remaining = budget - bytes_read;
            let read_len = remaining.min(chunk.len());
            match self.inner.get_ref().read(&mut chunk[..read_len]) {
                Ok(0) => {
                    // EOF: no future output can complete the fragment.
                    self.flush_pending_fragment(&mut display);
                    eof = true;
                    break;
                }
                // A signal interrupted the syscall, not the stream: retry
                // rather than failing the scan that owns this drain.
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Ok(n) => {
                    bytes_read += n;
                    self.feed_bytes(&mut display, &chunk[..n]);
                    if self.renderer_failed || display.len() >= RENDER_FLUSH_BYTES {
                        self.flush_display_with(&mut display, flush);
                    }
                }
                Err(err) if is_would_block(&err) => break,
                Err(err) => return Err(err.into()),
            }
        }
        if !display.is_empty() {
            self.flush_display_with(&mut display, flush);
        }
        Ok(ReadyDrainOutcome {
            bytes_read,
            eof,
            budget_exhausted,
        })
    }

    /// Explicit-ownership-wait drain: consume until EOF, publishing the
    /// pending fragment there. `wait PID` / `fg` only.
    pub async fn drain_to_eof(&mut self) -> Result<()> {
        let mut flush = Self::flush_terminal;
        self.drain_to_eof_with(&mut flush).await
    }

    async fn drain_to_eof_with<F>(&mut self, flush: &mut F) -> Result<()>
    where
        F: FnMut(&mut TerminalRenderer, &[u8]) -> Result<()>,
    {
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        let mut display = Vec::new();
        let mut first_error: Option<anyhow::Error> = None;
        loop {
            let mut readiness = self.inner.readable().await?;
            match readiness.try_io(|inner| inner.get_ref().read(&mut chunk)) {
                Ok(Ok(0)) => {
                    self.flush_pending_fragment(&mut display);
                    break;
                }
                // A signal interrupted the syscall, not the stream: retry.
                Ok(Err(err)) if err.kind() == ErrorKind::Interrupted => continue,
                Ok(Ok(n)) => {
                    self.feed_bytes(&mut display, &chunk[..n]);
                    if self.renderer_failed || display.len() >= RENDER_FLUSH_BYTES {
                        self.flush_display_with(&mut display, flush);
                    }
                }
                Ok(Err(err)) if is_would_block(&err) => {
                    readiness.clear_ready();
                    continue;
                }
                Ok(Err(err)) => {
                    if first_error.is_none() {
                        first_error = Some(err.into());
                    }
                    break;
                }
                Err(_) => continue,
            }
        }
        if !display.is_empty() {
            self.flush_display_with(&mut display, flush);
        }
        if let Some(err) = first_error {
            return Err(err);
        }
        Ok(())
    }

    /// Completed-job reconciliation: drain whatever the kernel already holds
    /// without waiting for anything.
    ///
    /// Sync by design: there is nothing to await. `waitpid` observed the
    /// canonical tree completed, so every byte the child wrote before
    /// exiting is already committed to the pipe; a direct non-blocking read
    /// recovers it even if the Tokio reactor has not delivered a readiness
    /// event yet (which is exactly the pass-4-poll / pass-5-complete race
    /// this replaces). A descendant still holding the write end only yields
    /// `WouldBlock`, which ends the drain — its future output is out of
    /// scope, and waiting for its EOF would stall the prompt.
    ///
    /// The pending fragment is always published before returning, with or
    /// without EOF or a trailing newline: this monitor retires here, so
    /// keeping it would drop those bytes with the monitor.
    pub fn finalize_ready_now(&mut self) -> Result<()> {
        let mut flush = Self::flush_terminal;
        self.finalize_ready_now_with(&mut flush)
    }

    fn finalize_ready_now_with<F>(&mut self, flush: &mut F) -> Result<()>
    where
        F: FnMut(&mut TerminalRenderer, &[u8]) -> Result<()>,
    {
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        let mut display = Vec::new();
        let mut first_error: Option<anyhow::Error> = None;
        loop {
            match self.inner.get_ref().read(&mut chunk) {
                Ok(0) => break,
                // A signal interrupted the syscall, not the stream: retry
                // the wait-free read rather than recording an error.
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Ok(n) => {
                    self.feed_bytes(&mut display, &chunk[..n]);
                    if self.renderer_failed || display.len() >= RENDER_FLUSH_BYTES {
                        self.flush_display_with(&mut display, flush);
                    }
                }
                Err(err) if is_would_block(&err) => break,
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(err.into());
                    }
                    break;
                }
            }
        }
        self.flush_pending_fragment(&mut display);
        if !display.is_empty() {
            self.flush_display_with(&mut display, flush);
        }
        if let Some(err) = first_error {
            return Err(err);
        }
        Ok(())
    }
}

#[cfg(test)]
mod renderer_failure_tests;

#[cfg(test)]
mod tests;

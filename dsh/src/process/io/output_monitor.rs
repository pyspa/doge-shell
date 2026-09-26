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
use std::io::{ErrorKind, Read, Write};
use std::os::fd::OwnedFd;

use crate::terminal::renderer::TerminalRenderer;
use dsh_types::observed_output::{ObservedStream, SharedOutputObserver};

const RUNNING_DRAIN_BUDGET_BYTES: usize = 256 * 1024;
const FIRST_MONITOR_OUTPUT_PREFIX: &str = "\r\n";
const READ_CHUNK_BYTES: usize = 4096;
const RENDER_FLUSH_BYTES: usize = 8192;
// NOTE: `MAX_PENDING_CONTROL_BYTES` stays in `super` (io.rs): it belongs to
// `PtyDisplayBuffer`, which did not move.

fn append_output_chunk(output_started: &mut bool, buffer: &mut String, chunk: &str) {
    if !*output_started {
        *output_started = true;
        buffer.push_str(FIRST_MONITOR_OUTPUT_PREFIX);
    }
    buffer.push_str(chunk);
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
    /// is only validated once its record completes.
    /// Published only on newline, at EOF, or at monitor retirement
    /// (`finalize_ready_now`); a running ready-now drain never publishes it.
    pending_line: Vec<u8>,
    pub(crate) outputed: bool,
    pub captured_output: String,
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
            captured_output: String::new(),
            renderer: TerminalRenderer::new(),
            renderer_failed: false,
            observer,
            observed_stream,
        })
    }

    pub(crate) fn stream(&self) -> ObservedStream {
        self.observed_stream
    }

    fn append_line(&mut self, buffer: &mut String, line: &str) {
        append_output_chunk(&mut self.outputed, buffer, line);
        // Also capture the raw line (we might want to be careful about prefixes/newlines)
        // The line from read_line includes the newline character usually.
        self.captured_output.push_str(line);
        if let Some(observer) = &self.observer
            && let Ok(mut observer) = observer.lock()
        {
            observer.append(self.observed_stream, line);
        }
    }

    fn flush_terminal(renderer: &mut TerminalRenderer, buffer: &str) -> Result<()> {
        if buffer.is_empty() {
            return Ok(());
        }

        renderer.write_all(buffer.as_bytes())?;
        renderer.flush()?;
        Ok(())
    }

    /// Best-effort display flush: a renderer failure disables rendering for
    /// the rest of this monitor's lifetime (sticky `renderer_failed`), logs
    /// one diagnostic, and never propagates as a drain `Err`. The display
    /// scratch buffer is always cleared so a long-running job cannot grow it
    /// without bound; capture and observer data is already recorded.
    fn flush_display_with<F>(&mut self, display: &mut String, flush: &mut F)
    where
        F: FnMut(&mut TerminalRenderer, &str) -> Result<()>,
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
    /// capture, and the observer.
    ///
    /// UTF-8 validation mirrors the old `read_line` semantics: invalid UTF-8
    /// is an error and the offending bytes are dropped, so the stream
    /// continues with the next record. Restoring them into `pending_line`
    /// would fail conversion again on every retry, stalling all later output
    /// behind one bad record.
    fn publish_record(&mut self, buffer: &mut String, record: Vec<u8>) -> Result<usize> {
        let len = record.len();
        match String::from_utf8(record) {
            Ok(line) => {
                self.append_line(buffer, &line);
                Ok(len)
            }
            Err(_) => Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            )
            .into()),
        }
    }

    /// Frame raw chunk bytes into newline-terminated records.
    ///
    /// A single `read` can return many lines (or a trailing fragment), so
    /// one syscall is never assumed to be one record. Complete records are
    /// published; the trailing fragment stays in `pending_line`. The first
    /// publish error is remembered while the rest of the buffer is still
    /// fully consumed, so one invalid record never discards the valid
    /// records read alongside it.
    fn feed_bytes(&mut self, display: &mut String, bytes: &[u8]) -> Option<anyhow::Error> {
        let mut first_error = None;
        self.pending_line.extend_from_slice(bytes);
        while let Some(newline) = self.pending_line.iter().position(|&byte| byte == b'\n') {
            let record: Vec<u8> = self.pending_line.drain(..=newline).collect();
            if let Err(err) = self.publish_record(display, record)
                && first_error.is_none()
            {
                first_error = Some(err);
            }
        }
        first_error
    }

    /// Publish the pending fragment as the final record. Used at EOF and at
    /// monitor retirement, where keeping it would lose it with the monitor.
    fn flush_pending_fragment(&mut self, display: &mut String) -> Option<anyhow::Error> {
        if self.pending_line.is_empty() {
            return None;
        }
        let fragment = std::mem::take(&mut self.pending_line);
        self.publish_record(display, fragment).err()
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
        F: FnMut(&mut TerminalRenderer, &str) -> Result<()>,
    {
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        let mut display = String::new();
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
                    let _ = self.flush_pending_fragment(&mut display);
                    eof = true;
                    break;
                }
                // A signal interrupted the syscall, not the stream: retry
                // rather than failing the scan that owns this drain.
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Ok(n) => {
                    bytes_read += n;
                    // A bad record drops itself; later records in the same
                    // chunk still publish, and the running drain stays `Ok`.
                    let _ = self.feed_bytes(&mut display, &chunk[..n]);
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
        F: FnMut(&mut TerminalRenderer, &str) -> Result<()>,
    {
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        let mut display = String::new();
        let mut first_error: Option<anyhow::Error> = None;
        loop {
            let mut readiness = self.inner.readable().await?;
            match readiness.try_io(|inner| inner.get_ref().read(&mut chunk)) {
                Ok(Ok(0)) => {
                    if let Some(err) = self.flush_pending_fragment(&mut display)
                        && first_error.is_none()
                    {
                        first_error = Some(err);
                    }
                    break;
                }
                // A signal interrupted the syscall, not the stream: retry.
                Ok(Err(err)) if err.kind() == ErrorKind::Interrupted => continue,
                Ok(Ok(n)) => {
                    if let Some(err) = self.feed_bytes(&mut display, &chunk[..n])
                        && first_error.is_none()
                    {
                        first_error = Some(err);
                    }
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
        F: FnMut(&mut TerminalRenderer, &str) -> Result<()>,
    {
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        let mut display = String::new();
        let mut first_error: Option<anyhow::Error> = None;
        loop {
            match self.inner.get_ref().read(&mut chunk) {
                Ok(0) => break,
                // A signal interrupted the syscall, not the stream: retry
                // the wait-free read rather than recording an error.
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Ok(n) => {
                    if let Some(err) = self.feed_bytes(&mut display, &chunk[..n])
                        && first_error.is_none()
                    {
                        first_error = Some(err);
                    }
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
        if let Some(err) = self.flush_pending_fragment(&mut display)
            && first_error.is_none()
        {
            first_error = Some(err);
        }
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

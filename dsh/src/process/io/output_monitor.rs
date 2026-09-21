//! Background output capture monitors.
//!
//! One [`OutputMonitor`] owns the read end of a capture pipe created for a
//! background (or observed foreground) external command. The reader fd is
//! always `O_NONBLOCK` (a structural invariant established in
//! [`OutputMonitor::new`]), wrapped in `tokio::io::unix::AsyncFd` so the
//! running-job paths can wait for readiness without a thread.
//!
//! Three drain semantics share one newline-framing parser:
//!
//! - running background job: [`OutputMonitor::drain_available`] (bounded
//!   wait/poll, partial lines stay pending for future output),
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

use anyhow::{Context as _, Result};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::OwnedFd;
use std::time::Duration;
use tokio::time;

use crate::terminal::renderer::TerminalRenderer;
use dsh_types::observed_output::{ObservedStream, SharedOutputObserver};

const MONITOR_TIMEOUT: u64 = 200;
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

#[derive(Debug)]
pub struct OutputMonitor {
    /// Non-blocking pipe reader. Readiness waiting
    /// ([`OutputMonitor::drain_available`], [`OutputMonitor::drain_to_eof`])
    /// goes through the `AsyncFd` reactor side; the completion path
    /// ([`OutputMonitor::finalize_ready_now`]) reads the underlying fd
    /// directly so a not-yet-dispatched readiness event cannot hide bytes
    /// the kernel already holds.
    inner: tokio::io::unix::AsyncFd<std::fs::File>,
    /// Incomplete record bytes kept across calls. New chunk bytes are
    /// appended here first, so a multi-byte character split across reads
    /// (or across a timeout) is only validated once its record completes.
    /// Published only on newline, at EOF, or at monitor retirement
    /// (`finalize_ready_now`); a running-job timeout never publishes it.
    pending_line: Vec<u8>,
    pub(crate) outputed: bool,
    pub captured_output: String,
    // Cached renderer to avoid repeated allocations.
    // Safe to hold as it no longer holds StdoutLock persistently.
    pub(crate) renderer: TerminalRenderer,
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

    fn flush_buffer(&mut self, buffer: &str) -> Result<()> {
        if buffer.is_empty() {
            return Ok(());
        }

        self.renderer.write_all(buffer.as_bytes())?;
        self.renderer.flush()?;
        Ok(())
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

    /// Running-job drain: consume currently-available output, waiting at
    /// most `quiet_period` for more. A partial line with no newline stays in
    /// `pending_line` for a future call; only EOF publishes it early.
    pub async fn drain_available(&mut self) -> Result<()> {
        self.drain_available_for(Duration::from_millis(MONITOR_TIMEOUT))
            .await
    }

    pub(crate) async fn drain_available_for(&mut self, quiet_period: Duration) -> Result<()> {
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        let mut display = String::new();
        loop {
            let mut readiness = match time::timeout(quiet_period, self.inner.readable()).await {
                // Quiet period elapsed with no new output: a running job may
                // still produce more later, so the pending fragment stays.
                Err(_) => break,
                Ok(readiness) => readiness?,
            };
            match readiness.try_io(|inner| inner.get_ref().read(&mut chunk)) {
                Ok(Ok(0)) => {
                    // EOF: no future output can complete the fragment.
                    let _ = self.flush_pending_fragment(&mut display);
                    break;
                }
                // A signal interrupted the syscall, not the stream: retry
                // rather than failing the wait that owns this drain.
                Ok(Err(err)) if err.kind() == ErrorKind::Interrupted => continue,
                Ok(Ok(n)) => {
                    // A bad record drops itself; later records in the same
                    // chunk still publish, and the running drain stays `Ok`.
                    let _ = self.feed_bytes(&mut display, &chunk[..n]);
                    if display.len() >= RENDER_FLUSH_BYTES {
                        self.flush_buffer(&display)?;
                        display.clear();
                    }
                }
                Ok(Err(err)) if is_would_block(&err) => {
                    readiness.clear_ready();
                    continue;
                }
                Ok(Err(err)) => return Err(err.into()),
                // Readiness was lost between poll and read; re-wait rather
                // than spin.
                Err(_) => continue,
            }
        }
        if !display.is_empty() {
            self.flush_buffer(&display)?;
        }
        Ok(())
    }

    /// Explicit-ownership-wait drain: consume until EOF, publishing the
    /// pending fragment there. `wait PID` / `fg` only.
    pub async fn drain_to_eof(&mut self) -> Result<()> {
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
                    if display.len() >= RENDER_FLUSH_BYTES {
                        self.flush_buffer(&display)?;
                        display.clear();
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
            self.flush_buffer(&display)?;
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
                    if display.len() >= RENDER_FLUSH_BYTES {
                        self.flush_buffer(&display)?;
                        display.clear();
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
            self.flush_buffer(&display)?;
        }
        if let Some(err) = first_error {
            return Err(err);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{MONITOR_TIMEOUT, OutputMonitor, append_output_chunk};
    use dsh_types::observed_output::{ObservedStream, SharedOutputObserver};
    use std::io::Write as _;
    use std::os::fd::IntoRawFd as _;
    use std::os::unix::io::FromRawFd as _;
    use std::time::Duration;

    /// Write bytes to a pipe writer without closing it, so the reader
    /// observes data but neither newline/EOF completion nor writer close.
    fn write_to_pipe(writer: &mut std::fs::File, data: &[u8]) {
        writer.write_all(data).expect("write output");
        writer.flush().expect("flush output");
    }

    fn unnamed_pipe() -> (std::os::fd::OwnedFd, std::fs::File) {
        let (read, write) = nix::unistd::pipe().expect("pipe");
        // `File` borrows nothing: the writer stays open until the caller
        // drops it, so tests control EOF explicitly.
        let writer = std::fs::File::from(write);
        (read, writer)
    }

    fn stdout_monitor(read: std::os::fd::OwnedFd) -> OutputMonitor {
        OutputMonitor::new(read, None, ObservedStream::Stdout).expect("create monitor")
    }

    #[test]
    fn append_output_chunk_prefixes_only_first_chunk() {
        let mut started = false;
        let mut buffer = String::new();

        append_output_chunk(&mut started, &mut buffer, "first\n");
        append_output_chunk(&mut started, &mut buffer, "second\n");

        assert_eq!(buffer, "\r\nfirst\nsecond\n");
    }

    #[test]
    fn append_output_chunk_keeps_payload_unchanged() {
        let mut started = false;
        let mut buffer = String::new();

        append_output_chunk(&mut started, &mut buffer, "\u{1b}[31mred\u{1b}[0m\n");

        assert_eq!(buffer, "\r\n\u{1b}[31mred\u{1b}[0m\n");
    }

    #[tokio::test]
    async fn output_monitor_drain_to_eof_waits_for_late_output() {
        let (read, write) = nix::unistd::pipe().expect("pipe");
        let mut monitor = stdout_monitor(read);
        let write_fd = write.into_raw_fd();

        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(MONITOR_TIMEOUT + 50)).await;
            let mut file = unsafe { std::fs::File::from_raw_fd(write_fd) };
            file.write_all(b"late output").expect("write output");
        });

        monitor.drain_available().await.expect("drain available");
        assert_eq!(monitor.captured_output, "");

        monitor.drain_to_eof().await.expect("drain to eof");
        writer.await.expect("writer task");
        assert_eq!(monitor.captured_output, "late output");
    }

    #[tokio::test]
    async fn output_monitor_timeout_preserves_partial_line() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        // Partial line without newline or EOF: drain_available must time out
        // without capturing it, and without losing it.
        write_to_pipe(&mut writer, b"PARTIAL");
        monitor
            .drain_available_for(Duration::from_millis(5))
            .await
            .expect("drain available");
        assert_eq!(monitor.captured_output, "");

        // Complete the line and close: the whole record must arrive exactly once.
        write_to_pipe(&mut writer, b"-TAIL\n");
        drop(writer);
        monitor.drain_to_eof().await.expect("drain to eof");
        assert_eq!(monitor.captured_output, "PARTIAL-TAIL\n");
    }

    #[tokio::test]
    async fn output_monitor_timeout_preserves_no_newline_eof() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        // `<(printf ...)` shape: bytes with no newline, timed out once, then EOF.
        write_to_pipe(&mut writer, b"NO-NEWLINE");
        monitor
            .drain_available_for(Duration::from_millis(5))
            .await
            .expect("drain available");
        assert_eq!(monitor.captured_output, "");

        drop(writer);
        monitor.drain_to_eof().await.expect("drain to eof");
        assert_eq!(monitor.captured_output, "NO-NEWLINE");
    }

    #[tokio::test]
    async fn output_monitor_timeout_resume_does_not_duplicate() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        write_to_pipe(&mut writer, b"first-");
        monitor
            .drain_available_for(Duration::from_millis(5))
            .await
            .expect("drain available");
        assert_eq!(monitor.captured_output, "");

        write_to_pipe(&mut writer, b"second-");
        monitor
            .drain_available_for(Duration::from_millis(5))
            .await
            .expect("drain available");
        assert_eq!(monitor.captured_output, "");

        write_to_pipe(&mut writer, b"third\n");
        drop(writer);
        monitor.drain_to_eof().await.expect("drain to eof");
        assert_eq!(monitor.captured_output, "first-second-third\n");
    }

    #[tokio::test]
    async fn output_monitor_timeout_preserves_split_utf8() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        // "あ" is E3 81 82 in UTF-8; split it across the timeout boundary so a
        // partial-byte UTF-8 conversion must not run or fail.
        write_to_pipe(&mut writer, &[0xE3]);
        monitor
            .drain_available_for(Duration::from_millis(5))
            .await
            .expect("drain available");
        assert_eq!(monitor.captured_output, "");

        write_to_pipe(&mut writer, &[0x81, 0x82, b'\n']);
        drop(writer);
        monitor.drain_to_eof().await.expect("drain to eof");
        assert_eq!(monitor.captured_output, "あ\n");
    }

    #[tokio::test]
    async fn output_monitor_invalid_utf8_does_not_stall_later_output() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        // One invalid-UTF-8 record followed by valid lines: like `read_line`,
        // the bad record is dropped with an error, but later output must
        // still arrive instead of stalling behind the retained prefix.
        write_to_pipe(&mut writer, b"\xff\n");
        write_to_pipe(&mut writer, b"after\n");
        drop(writer);
        let _ = monitor.drain_available_for(Duration::from_millis(5)).await;
        monitor.drain_to_eof().await.expect("drain to eof");
        assert_eq!(monitor.captured_output, "after\n");
    }

    #[tokio::test]
    async fn output_monitor_completion_fd_is_nonblocking() {
        use nix::fcntl::{FcntlArg, OFlag, fcntl};

        let (read, _writer) = nix::unistd::pipe().expect("pipe");
        let monitor = stdout_monitor(read);
        let flags = fcntl(monitor.inner.get_ref(), FcntlArg::F_GETFL).expect("F_GETFL");
        assert!(
            OFlag::from_bits_truncate(flags).contains(OFlag::O_NONBLOCK),
            "OutputMonitor must own an O_NONBLOCK reader, flags: {flags:#x}"
        );
    }

    /// Reconciliation with no new bytes returns immediately: the writer is
    /// still open (no EOF), yet `finalize_ready_now` is sync and cannot
    /// wait on anything.
    #[tokio::test]
    async fn output_monitor_completion_would_block_is_terminal() {
        let (read, _writer) = nix::unistd::pipe().expect("pipe");
        let mut monitor = stdout_monitor(read);

        monitor
            .finalize_ready_now()
            .expect("ready-now with no data is Ok");
        assert_eq!(monitor.captured_output, "");
    }

    /// Basic completion: a newline-terminated record is recovered with the
    /// writer still open (no EOF), exactly once.
    #[tokio::test]
    async fn output_monitor_completion_ready_now_basic() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        write_to_pipe(&mut writer, b"READY\n");
        monitor.finalize_ready_now().expect("ready-now drain");

        assert_eq!(monitor.captured_output, "READY\n");
        // The writer stays open: this never waited for EOF.
        drop(writer);
    }

    /// The pass-4/pass-5 race in miniature: a poll observes nothing, the
    /// child writes afterwards, and completion reconciliation must still
    /// recover those bytes instead of dropping the monitor.
    #[tokio::test]
    async fn output_monitor_completion_race_late_write_after_poll() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        monitor
            .drain_available_for(Duration::from_millis(5))
            .await
            .expect("pass-4 style poll");
        assert_eq!(monitor.captured_output, "");

        // Written after the poll, before completion is observed.
        write_to_pipe(&mut writer, b"LATE\n");
        monitor.finalize_ready_now().expect("pass-5 style finalize");

        assert_eq!(monitor.captured_output, "LATE\n");
        drop(writer);
    }

    /// A partial line held across the running drain (no newline, no EOF,
    /// writer open) is published at retirement: monitor ownership ends
    /// here, so keeping it would lose it with the monitor.
    #[tokio::test]
    async fn output_monitor_completion_prior_partial_no_newline() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        write_to_pipe(&mut writer, b"PARTIAL");
        monitor
            .drain_available_for(Duration::from_millis(5))
            .await
            .expect("running drain holds the partial line");
        assert_eq!(monitor.captured_output, "");

        monitor
            .finalize_ready_now()
            .expect("retirement publishes it");
        assert_eq!(monitor.captured_output, "PARTIAL");
        assert!(monitor.pending_line.is_empty());
        drop(writer);
    }

    /// A prior fragment plus a late tail join into one record, exactly once.
    #[tokio::test]
    async fn output_monitor_completion_partial_plus_late_tail() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        write_to_pipe(&mut writer, b"PART");
        monitor
            .drain_available_for(Duration::from_millis(5))
            .await
            .expect("drain available");
        assert_eq!(monitor.captured_output, "");

        write_to_pipe(&mut writer, b"IAL\n");
        monitor.finalize_ready_now().expect("ready-now drain");
        assert_eq!(monitor.captured_output, "PARTIAL\n");
        drop(writer);
    }

    /// A multi-byte character split across the timeout boundary is only
    /// validated once complete.
    #[tokio::test]
    async fn output_monitor_completion_split_utf8() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        write_to_pipe(&mut writer, &[0xE3]);
        monitor
            .drain_available_for(Duration::from_millis(5))
            .await
            .expect("drain available");
        assert_eq!(monitor.captured_output, "");

        write_to_pipe(&mut writer, &[0x81, 0x82, b'\n']);
        monitor.finalize_ready_now().expect("ready-now drain");
        assert_eq!(monitor.captured_output, "あ\n");
        drop(writer);
    }

    /// One raw `read` returning several lines plus a trailing fragment
    /// publishes every line and the fragment (writer still open, no EOF).
    #[tokio::test]
    async fn output_monitor_completion_multiple_lines_one_read() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        write_to_pipe(&mut writer, b"one\ntwo\nthree");
        monitor.finalize_ready_now().expect("ready-now drain");

        assert_eq!(monitor.captured_output, "one\ntwo\nthree");
        drop(writer);
    }

    /// An invalid record read alongside a valid one drops only itself: the
    /// method may report the error, but the valid record still arrives.
    #[tokio::test]
    async fn output_monitor_completion_invalid_utf8_keeps_valid_record() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        write_to_pipe(&mut writer, b"\xff\nafter\n");
        let _ = monitor.finalize_ready_now();

        assert_eq!(monitor.captured_output, "after\n");
        drop(writer);
    }

    /// Capture, renderer prefix, and observer all observe the retired bytes
    /// exactly once — even for a fragment with no newline and no EOF.
    #[tokio::test]
    async fn output_monitor_completion_observer_exactly_once() {
        let observer: SharedOutputObserver =
            dsh_types::observed_output::ObservedOutput::shared(1024);
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = OutputMonitor::new(read, Some(observer.clone()), ObservedStream::Stdout)
            .expect("create monitor");

        write_to_pipe(&mut writer, b"READY\nFRAGMENT");
        monitor.finalize_ready_now().expect("ready-now drain");

        assert_eq!(monitor.captured_output, "READY\nFRAGMENT");
        let observed = observer.lock().expect("observer lock");
        let stdout_text = observed.snapshot().stdout;
        assert_eq!(stdout_text.matches("READY\n").count(), 1);
        assert!(stdout_text.ends_with("FRAGMENT"));
        drop(writer);
    }

    /// The first-output `\r\n` prefix fires once on the retirement path
    /// too: the `outputed` flag flips on the first retired record and stays
    /// set, so the display buffer keeps its exactly-once prefix contract.
    #[tokio::test]
    async fn output_monitor_completion_first_output_prefix_once() {
        let (read, mut writer) = unnamed_pipe();
        let mut monitor = stdout_monitor(read);

        assert!(!monitor.outputed);
        write_to_pipe(&mut writer, b"first\n");
        monitor.finalize_ready_now().expect("ready-now drain");
        assert!(monitor.outputed);
        let captured_after_first = monitor.captured_output.clone();

        write_to_pipe(&mut writer, b"second\n");
        monitor
            .finalize_ready_now()
            .expect("second ready-now drain");
        assert!(monitor.outputed);
        assert_eq!(
            monitor.captured_output,
            format!("{captured_after_first}second\n")
        );
        drop(writer);
    }
}

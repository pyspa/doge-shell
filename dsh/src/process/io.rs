use anyhow::{Context as _, Result};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::unistd::{isatty, pipe2};
use std::io::{Read, Write};
use std::os::fd::BorrowedFd;
use std::os::fd::OwnedFd;
use std::os::unix::io::{FromRawFd, IntoRawFd, RawFd};
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::io::unix::AsyncFd;
use tokio::{fs, io, time};

use crate::terminal::renderer::{TerminalRenderer, flush_stdout_bytes};
use dsh_types::Context;
use dsh_types::observed_output::{ObservedStream, SharedOutputObserver};
use libc::STDIN_FILENO;

const MONITOR_TIMEOUT: u64 = 200;
const FIRST_MONITOR_OUTPUT_PREFIX: &str = "\r\n";
const MAX_PENDING_CONTROL_BYTES: usize = 4096;

fn append_output_chunk(output_started: &mut bool, buffer: &mut String, chunk: &str) {
    if !*output_started {
        *output_started = true;
        buffer.push_str(FIRST_MONITOR_OUTPUT_PREFIX);
    }
    buffer.push_str(chunk);
}

#[derive(Debug)]
pub struct OutputMonitor {
    pub(crate) reader: io::BufReader<fs::File>,
    pub(crate) outputed: bool,
    pub captured_output: String,
    // Cached renderer to avoid repeated allocations.
    // Safe to hold as it no longer holds StdoutLock persistently.
    pub(crate) renderer: TerminalRenderer,
    observer: Option<SharedOutputObserver>,
    observed_stream: ObservedStream,
}

impl OutputMonitor {
    pub fn new(
        fd: RawFd,
        observer: Option<SharedOutputObserver>,
        observed_stream: ObservedStream,
    ) -> Self {
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let reader = io::BufReader::new(file);
        OutputMonitor {
            reader,
            outputed: false,
            captured_output: String::new(),
            renderer: TerminalRenderer::new(),
            observer,
            observed_stream,
        }
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

    pub async fn output(&mut self) -> Result<usize> {
        let mut line = String::new();
        match time::timeout(
            Duration::from_millis(MONITOR_TIMEOUT),
            self.reader.read_line(&mut line),
        )
        .await
        {
            Ok(Ok(len)) => {
                if len > 0 {
                    let mut buffer = String::new();
                    self.append_line(&mut buffer, &line);
                    self.flush_buffer(&buffer)?;
                }
                Ok(len)
            }
            Ok(Err(_)) | Err(_) => Ok(0),
        }
    }

    pub async fn drain_available(&mut self) -> Result<()> {
        let mut buffer = String::new();
        loop {
            let mut line = String::new();
            match time::timeout(
                Duration::from_millis(MONITOR_TIMEOUT),
                self.reader.read_line(&mut line),
            )
            .await
            {
                Ok(Ok(readed)) => {
                    if readed == 0 {
                        break;
                    } else {
                        self.append_line(&mut buffer, &line);
                    }
                }
                Ok(Err(_)) | Err(_) => {
                    break;
                }
            }
        }
        if !buffer.is_empty() {
            self.flush_buffer(&buffer)?;
        }
        Ok(())
    }

    pub async fn drain_to_eof(&mut self) -> Result<()> {
        let mut buffer = String::new();
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).await?;
            if read == 0 {
                break;
            }
            self.append_line(&mut buffer, &line);
            if buffer.len() >= 8192 {
                self.flush_buffer(&buffer)?;
                buffer.clear();
            }
        }
        if !buffer.is_empty() {
            self.flush_buffer(&buffer)?;
        }
        Ok(())
    }
}

pub struct PtyMonitor {
    inner: AsyncFd<std::fs::File>,
    pub captured_output: Vec<u8>,
    display_buffer: PtyDisplayBuffer,
    observer: Option<SharedOutputObserver>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PtyDisplayState {
    Ground,
    Escape,
    Csi,
    Osc,
    OscEscape,
    ControlString,
    ControlStringEscape,
}

#[derive(Debug)]
struct PtyDisplayBuffer {
    stdout_is_tty: bool,
    state: PtyDisplayState,
    pending_control: Vec<u8>,
    last_passthrough_byte: Option<u8>,
}

impl PtyDisplayBuffer {
    fn new(stdout_is_tty: bool) -> Self {
        Self {
            stdout_is_tty,
            state: PtyDisplayState::Ground,
            pending_control: Vec::new(),
            last_passthrough_byte: None,
        }
    }

    fn push_chunk(&mut self, data: &[u8]) -> Vec<u8> {
        if !self.stdout_is_tty {
            self.last_passthrough_byte = data.last().copied();
            return data.to_vec();
        }

        let mut output = Vec::with_capacity(data.len());
        for &byte in data {
            self.push_byte(byte, &mut output);
            self.last_passthrough_byte = Some(byte);
        }
        output
    }

    fn finish(&mut self) -> Vec<u8> {
        self.state = PtyDisplayState::Ground;
        std::mem::take(&mut self.pending_control)
    }

    fn push_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        match self.state {
            PtyDisplayState::Ground => self.push_ground_byte(byte, output),
            PtyDisplayState::Escape => self.push_escape_byte(byte, output),
            PtyDisplayState::Csi => self.push_csi_byte(byte, output),
            PtyDisplayState::Osc => self.push_osc_byte(byte, output),
            PtyDisplayState::OscEscape => self.push_osc_escape_byte(byte, output),
            PtyDisplayState::ControlString => self.push_control_string_byte(byte, output),
            PtyDisplayState::ControlStringEscape => {
                self.push_control_string_escape_byte(byte, output)
            }
        }
    }

    fn push_ground_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        if byte == b'\x1b' {
            self.pending_control.push(byte);
            self.state = PtyDisplayState::Escape;
        } else if byte == b'\n' && self.last_passthrough_byte != Some(b'\r') {
            output.extend_from_slice(b"\r\n");
        } else {
            output.push(byte);
        }
    }

    fn push_escape_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        self.pending_control.push(byte);
        match byte {
            b'[' => self.state = PtyDisplayState::Csi,
            b']' => self.state = PtyDisplayState::Osc,
            b'P' | b'X' | b'^' | b'_' => self.state = PtyDisplayState::ControlString,
            _ => self.flush_pending(output),
        }
        self.flush_pending_if_too_large(output);
    }

    fn push_csi_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        self.pending_control.push(byte);
        if (0x40..=0x7e).contains(&byte) {
            self.flush_pending(output);
        } else {
            self.flush_pending_if_too_large(output);
        }
    }

    fn push_osc_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        self.pending_control.push(byte);
        match byte {
            b'\x07' => self.flush_pending(output),
            b'\x1b' => self.state = PtyDisplayState::OscEscape,
            _ => self.flush_pending_if_too_large(output),
        }
    }

    fn push_osc_escape_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        self.pending_control.push(byte);
        if byte == b'\\' {
            self.flush_pending(output);
        } else if byte == b'\x1b' {
            self.state = PtyDisplayState::OscEscape;
            self.flush_pending_if_too_large(output);
        } else {
            self.state = PtyDisplayState::Osc;
            self.flush_pending_if_too_large(output);
        }
    }

    fn push_control_string_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        self.pending_control.push(byte);
        match byte {
            b'\x07' => self.flush_pending(output),
            b'\x1b' => self.state = PtyDisplayState::ControlStringEscape,
            _ => self.flush_pending_if_too_large(output),
        }
    }

    fn push_control_string_escape_byte(&mut self, byte: u8, output: &mut Vec<u8>) {
        self.pending_control.push(byte);
        if byte == b'\\' {
            self.flush_pending(output);
        } else if byte == b'\x1b' {
            self.state = PtyDisplayState::ControlStringEscape;
            self.flush_pending_if_too_large(output);
        } else {
            self.state = PtyDisplayState::ControlString;
            self.flush_pending_if_too_large(output);
        }
    }

    fn flush_pending(&mut self, output: &mut Vec<u8>) {
        output.append(&mut self.pending_control);
        self.state = PtyDisplayState::Ground;
    }

    fn flush_pending_if_too_large(&mut self, output: &mut Vec<u8>) {
        if self.pending_control.len() >= MAX_PENDING_CONTROL_BYTES {
            self.flush_pending(output);
        }
    }
}

impl PtyMonitor {
    pub fn new(fd: RawFd, observer: Option<SharedOutputObserver>) -> Result<Self> {
        let file = unsafe { std::fs::File::from_raw_fd(fd) };

        // Set non-blocking mode
        let current_flags = fcntl(&file, FcntlArg::F_GETFL).context("fcntl F_GETFL failed")?;
        let flags = OFlag::from_bits_truncate(current_flags) | OFlag::O_NONBLOCK;
        fcntl(&file, FcntlArg::F_SETFL(flags)).context("fcntl F_SETFL failed")?;

        let inner = AsyncFd::new(file).context("AsyncFd creation failed")?;

        Ok(PtyMonitor {
            inner,
            captured_output: Vec::new(),
            display_buffer: PtyDisplayBuffer::new(
                isatty(unsafe { BorrowedFd::borrow_raw(libc::STDOUT_FILENO) }).unwrap_or(false),
            ),
            observer,
        })
    }

    pub async fn process_output(&mut self) -> Result<()> {
        self.process_output_with(flush_stdout_bytes).await
    }

    async fn process_output_with<F>(&mut self, mut flush_stdout: F) -> Result<()>
    where
        F: FnMut(&[u8]) -> std::io::Result<()>,
    {
        let mut buf = [0u8; 4096];
        let mut stdout_error = None;

        loop {
            // Use timeout to avoid blocking indefinitely when PTY is closed
            let guard_result =
                tokio::time::timeout(Duration::from_millis(100), self.inner.readable()).await;

            let mut guard = match guard_result {
                Ok(Ok(g)) => g,
                Ok(Err(e)) => {
                    // AsyncFd error - likely PTY was closed
                    tracing::debug!("PtyMonitor: AsyncFd error: {}", e);
                    break;
                }
                Err(_timeout) => {
                    // Timeout - check if we can read without blocking (for draining)
                    // This handles the case where PTY master is closed but data remains
                    continue;
                }
            };

            let res = guard.try_io(|inner| inner.get_ref().read(&mut buf));

            match res {
                Ok(Ok(0)) => {
                    tracing::debug!("PtyMonitor: EOF detected");
                    break;
                }
                Ok(Ok(n)) => {
                    tracing::debug!("PtyMonitor: Read {} bytes", n);
                    let data = &buf[..n];
                    let display_bytes = self.display_buffer.push_chunk(data);

                    if stdout_error.is_none()
                        && let Err(err) = flush_stdout(&display_bytes)
                            .context("PtyMonitor: failed to flush stdout")
                    {
                        stdout_error = Some(err);
                    }

                    // Capture
                    self.captured_output.extend_from_slice(data);
                    if let Some(observer) = &self.observer
                        && let Ok(mut observer) = observer.lock()
                    {
                        observer.append(ObservedStream::Stdout, &String::from_utf8_lossy(data));
                    }
                }
                Ok(Err(e)) => {
                    // Check for WouldBlock or EAGAIN (os error 11)
                    // On Linux, EAGAIN == EWOULDBLOCK, but explicit check is safer
                    let is_would_block = e.kind() == std::io::ErrorKind::WouldBlock
                        || e.raw_os_error() == Some(libc::EAGAIN);

                    if is_would_block {
                        // Clear readiness state and retry
                        guard.clear_ready();
                        continue;
                    }
                    // Check for EIO (OS error 5) which means EOF on Linux PTY
                    if let Some(os_err) = e.raw_os_error()
                        && os_err == 5
                    {
                        tracing::debug!("PtyMonitor: EIO detected (EOF)");
                        break;
                    }
                    tracing::error!("PtyMonitor: Error reading: {}", e);
                    return Err(e.into());
                }
                Err(_would_block) => {
                    // try_io returned WouldBlock, resource not ready
                    // guard.clear_ready() is called automatically by try_io on WouldBlock
                    continue;
                }
            }
        }

        // Flush pending control bytes unless an earlier display write failed.
        let remaining = self.display_buffer.finish();
        if stdout_error.is_none()
            && let Err(err) =
                flush_stdout(&remaining).context("PtyMonitor: failed to flush final stdout bytes")
        {
            stdout_error = Some(err);
        }

        match stdout_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

/// A pipe whose ends do not survive `exec`.
///
/// Only the descriptors a child is given as its stdin/stdout/stderr should
/// reach the new program, and `dup2` clears `FD_CLOEXEC` on the copy, so those
/// still work. Without this the *other* end leaked into every child: `yes |
/// head -1` left `yes` holding the read end of its own output pipe, so it never
/// got EPIPE and blocked forever after the shell had moved on.
pub(crate) fn cloexec_pipe() -> nix::Result<(OwnedFd, OwnedFd)> {
    pipe2(OFlag::O_CLOEXEC)
}

pub(crate) fn create_pipe(ctx: &mut Context) -> Result<Option<RawFd>> {
    let (pout, pin) = cloexec_pipe().context("failed pipe")?;
    ctx.outfile = pin.into_raw_fd();
    Ok(Some(pout.into_raw_fd()))
}

/// Wire up stdout when nothing redirects it.
///
/// Redirections are applied separately, straight onto the opened file, so all
/// that is left here is the pipeline/capture default.
///
/// `ctx.captured_out` is checked first: command substitution and
/// `execute_with_capture` hand the job a pipe that way, and it must win over
/// the pipeline default below.
pub(crate) fn default_output_wiring(ctx: &mut Context, stdout: RawFd) {
    if let Some(out) = ctx.captured_out {
        ctx.outfile = out;
    } else if ctx.infile != STDIN_FILENO {
        ctx.outfile = stdout;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MONITOR_TIMEOUT, OutputMonitor, PtyDisplayBuffer, PtyMonitor, append_output_chunk,
    };
    use dsh_types::observed_output::ObservedStream;
    use nix::unistd::pipe;
    use std::io::{ErrorKind, Write as _};
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::time::Duration;

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

    #[test]
    fn normalize_tty_newlines_converts_bare_lf() {
        let mut buffer = PtyDisplayBuffer::new(true);

        let normalized = buffer.push_chunk(b"first\nsecond\n");

        assert_eq!(normalized, b"first\r\nsecond\r\n");
        assert!(buffer.finish().is_empty());
    }

    #[test]
    fn normalize_tty_newlines_preserves_existing_crlf() {
        let mut buffer = PtyDisplayBuffer::new(true);

        let normalized = buffer.push_chunk(b"first\r\nsecond\r\n");

        assert_eq!(normalized, b"first\r\nsecond\r\n");
        assert!(buffer.finish().is_empty());
    }

    #[test]
    fn normalize_tty_newlines_handles_ansi_colored_output() {
        let mut buffer = PtyDisplayBuffer::new(true);

        let normalized = buffer.push_chunk(b"\x1b[31mred\x1b[0m\n");

        assert_eq!(normalized, b"\x1b[31mred\x1b[0m\r\n");
        assert!(buffer.finish().is_empty());
    }

    #[test]
    fn normalize_tty_newlines_preserves_split_crlf_across_chunks() {
        let mut buffer = PtyDisplayBuffer::new(true);

        let first = buffer.push_chunk(b"prefix\r");
        let second = buffer.push_chunk(b"\nsuffix\n");

        assert_eq!(first, b"prefix\r");
        assert_eq!(second, b"\nsuffix\r\n");
        assert!(buffer.finish().is_empty());
    }

    #[test]
    fn normalize_tty_newlines_preserves_carriage_return_progress_updates() {
        let mut buffer = PtyDisplayBuffer::new(true);

        let normalized = buffer.push_chunk(b"loading\rstep2\r");

        assert_eq!(normalized, b"loading\rstep2\r");
        assert!(buffer.finish().is_empty());
    }

    #[test]
    fn pty_display_buffer_holds_split_csi_until_complete() {
        let mut buffer = PtyDisplayBuffer::new(true);

        let first = buffer.push_chunk(b"\x1b[3");
        let second = buffer.push_chunk(b"1mred\x1b[0m\n");

        assert!(first.is_empty());
        assert_eq!(second, b"\x1b[31mred\x1b[0m\r\n");
        assert!(buffer.finish().is_empty());
    }

    #[test]
    fn pty_display_buffer_holds_split_osc_until_st() {
        let mut buffer = PtyDisplayBuffer::new(true);

        let first = buffer.push_chunk(b"\x1b]0;title");
        let second = buffer.push_chunk(b"\x1b\\done\n");

        assert!(first.is_empty());
        assert_eq!(second, b"\x1b]0;title\x1b\\done\r\n");
        assert!(buffer.finish().is_empty());
    }

    #[test]
    fn pty_display_buffer_flushes_incomplete_control_on_finish() {
        let mut buffer = PtyDisplayBuffer::new(true);

        let output = buffer.push_chunk(b"\x1b]0;unterminated");
        let final_output = buffer.finish();

        assert!(output.is_empty());
        assert_eq!(final_output, b"\x1b]0;unterminated");
    }

    #[test]
    fn pty_display_buffer_limits_unterminated_control_growth() {
        let mut buffer = PtyDisplayBuffer::new(true);
        let mut input = b"\x1b]".to_vec();
        input.extend(std::iter::repeat_n(b'a', super::MAX_PENDING_CONTROL_BYTES));

        let output = buffer.push_chunk(&input);

        assert!(!output.is_empty());
        assert!(buffer.finish().is_empty());
    }

    #[test]
    fn pty_display_buffer_preserves_non_tty_bytes() {
        let mut buffer = PtyDisplayBuffer::new(false);

        let output = buffer.push_chunk(b"\x1b[31mred\x1b[0m\n");

        assert_eq!(output, b"\x1b[31mred\x1b[0m\n");
        assert!(buffer.finish().is_empty());
    }

    #[test]
    fn pty_display_buffer_handles_many_colored_lines() {
        let mut buffer = PtyDisplayBuffer::new(true);
        let mut input = Vec::new();
        let mut expected = Vec::new();
        for index in 0..1024 {
            let line = format!("\x1b[31mline-{index}\x1b[0m\n");
            input.extend_from_slice(line.as_bytes());
            expected.extend_from_slice(format!("\x1b[31mline-{index}\x1b[0m\r\n").as_bytes());
        }

        let output = buffer.push_chunk(&input);

        assert_eq!(output, expected);
        assert_eq!(input.last(), Some(&b'\n'));
        assert!(buffer.finish().is_empty());
    }

    #[tokio::test]
    async fn output_monitor_drain_to_eof_waits_for_late_output() {
        let (read_fd, write_fd) = pipe().expect("pipe");
        let mut monitor = OutputMonitor::new(read_fd.into_raw_fd(), None, ObservedStream::Stdout);
        let write_fd = write_fd.into_raw_fd();

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
    async fn pty_monitor_drains_to_eof_after_stdout_failure() {
        let (read_fd, write_fd) = pipe().expect("pipe");
        let mut monitor = PtyMonitor::new(read_fd.into_raw_fd(), None).expect("create monitor");
        let mut writer = unsafe { std::fs::File::from_raw_fd(write_fd.into_raw_fd()) };
        let expected = vec![b'x'; 8192];
        writer.write_all(&expected).expect("write output");
        drop(writer);

        let mut flush_calls = 0;
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            monitor.process_output_with(|_| {
                flush_calls += 1;
                Err(std::io::Error::new(
                    ErrorKind::BrokenPipe,
                    "test stdout failure",
                ))
            }),
        )
        .await
        .expect("monitor did not drain to EOF");

        let err = result.expect_err("stdout failure should propagate after draining");
        assert!(
            err.to_string()
                .contains("PtyMonitor: failed to flush stdout")
        );
        assert_eq!(flush_calls, 1);
        assert_eq!(monitor.captured_output, expected);
    }
}

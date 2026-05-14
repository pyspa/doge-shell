use anyhow::{Context as _, Result};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::unistd::{isatty, pipe};
use std::io::{Read, Write};
use std::os::fd::BorrowedFd;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::io::unix::AsyncFd;
use tokio::{fs, io, time};

use super::redirect::Redirect;
use crate::terminal::renderer::TerminalRenderer;
use dsh_types::Context;
use dsh_types::observed_output::{ObservedStream, SharedOutputObserver};
use libc::STDIN_FILENO;

const MONITOR_TIMEOUT: u64 = 200;
const FIRST_MONITOR_OUTPUT_PREFIX: &str = "\r\n";

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

    pub async fn output_all(&mut self, block: bool) -> Result<()> {
        let mut len = 1;
        let mut buffer = String::new();
        while len != 0 {
            let mut line = String::new();
            match time::timeout(
                Duration::from_millis(MONITOR_TIMEOUT),
                self.reader.read_line(&mut line),
            )
            .await
            {
                Ok(Ok(readed)) => {
                    len = readed;
                    if readed == 0 {
                        if !block {
                            break;
                        }
                    } else {
                        self.append_line(&mut buffer, &line);
                    }
                }
                Ok(Err(_)) | Err(_) => {
                    if !block {
                        break;
                    }
                }
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
    stdout_is_tty: bool,
    last_passthrough_byte: Option<u8>,
    observer: Option<SharedOutputObserver>,
}

fn normalize_tty_newlines(data: &[u8], last_byte: &mut Option<u8>) -> Option<Vec<u8>> {
    let mut prev = *last_byte;
    let mut normalized = Vec::new();
    let mut changed = false;

    for (index, &byte) in data.iter().enumerate() {
        if byte == b'\n' && prev != Some(b'\r') {
            if !changed {
                normalized.reserve(data.len() + 8);
                normalized.extend_from_slice(&data[..index]);
                changed = true;
            }
            normalized.push(b'\r');
            normalized.push(b'\n');
        } else if changed {
            normalized.push(byte);
        }
        prev = Some(byte);
    }

    *last_byte = prev;

    if changed { Some(normalized) } else { None }
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
            stdout_is_tty: isatty(unsafe { BorrowedFd::borrow_raw(libc::STDOUT_FILENO) })
                .unwrap_or(false),
            last_passthrough_byte: None,
            observer,
        })
    }

    pub async fn process_output(&mut self) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        let mut buf = [0u8; 4096];
        let mut stdout = tokio::io::stdout();

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
                    let display_data = if self.stdout_is_tty {
                        normalize_tty_newlines(data, &mut self.last_passthrough_byte)
                    } else {
                        self.last_passthrough_byte = data.last().copied();
                        None
                    };
                    let display_bytes = display_data.as_deref().unwrap_or(data);

                    // Print to real stdout (Passthrough) - use async write
                    if let Err(e) = stdout.write_all(display_bytes).await {
                        tracing::error!("PtyMonitor: Failed to write to stdout: {}", e);
                    }
                    if let Err(e) = stdout.flush().await {
                        tracing::error!("PtyMonitor: Failed to flush stdout: {}", e);
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

        // Final flush to ensure all output is displayed
        let _ = stdout.flush().await;
        Ok(())
    }
}

pub(crate) fn create_pipe(ctx: &mut Context) -> Result<Option<RawFd>> {
    let (pout, pin) = pipe().context("failed pipe")?;
    ctx.outfile = pin.into_raw_fd();
    Ok(Some(pout.into_raw_fd()))
}

pub(crate) fn handle_output_redirect(
    ctx: &mut Context,
    redirect: &Option<Redirect>,
    stdout: RawFd,
) -> Result<Option<RawFd>> {
    if let Some(output) = redirect {
        match output {
            Redirect::StdoutOutput(_file) | Redirect::StdoutAppend(_file) => {
                let (pout, pin) = pipe().context("failed pipe")?;
                ctx.outfile = pin.into_raw_fd();
                Ok(Some(pout.into_raw_fd()))
            }
            Redirect::StderrOutput(file) | Redirect::StderrAppend(file) => {
                tracing::debug!("🔀 REDIRECT: StderrOutput/Append to file: {}", file);
                let (pout, pin) = pipe().context("failed pipe")?;
                tracing::debug!(
                    "🔀 REDIRECT: Created redirect pipe - read_end={:?}, write_end={:?}",
                    pout,
                    pin
                );
                ctx.errfile = pin.into_raw_fd();
                tracing::debug!("🔀 REDIRECT: Set ctx.errfile={}", ctx.errfile);
                Ok(Some(pout.into_raw_fd()))
            }
            Redirect::StdouterrOutput(file) | Redirect::StdouterrAppend(file) => {
                tracing::debug!("🔀 REDIRECT: StdouterrOutput/Append to file: {}", file);
                let (pout, pin) = pipe().context("failed pipe")?;
                tracing::debug!(
                    "🔀 REDIRECT: Created redirect pipe - read_end={:?}, write_end={:?}",
                    pout,
                    pin
                );
                ctx.outfile = pin.as_raw_fd(); // Keep alive for errfile
                ctx.errfile = pin.into_raw_fd();
                tracing::debug!(
                    "🔀 REDIRECT: Set ctx.outfile={}, ctx.errfile={}",
                    ctx.outfile,
                    ctx.errfile
                );
                Ok(Some(pout.into_raw_fd()))
            }
            Redirect::Input(_) => Ok(None),
        }
    } else {
        if let Some(out) = ctx.captured_out {
            ctx.outfile = out;
        } else if ctx.infile != STDIN_FILENO {
            ctx.outfile = stdout;
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::{append_output_chunk, normalize_tty_newlines};

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
        let mut last = None;

        let normalized = normalize_tty_newlines(b"first\nsecond\n", &mut last)
            .expect("bare LF should be normalized");

        assert_eq!(normalized, b"first\r\nsecond\r\n");
        assert_eq!(last, Some(b'\n'));
    }

    #[test]
    fn normalize_tty_newlines_preserves_existing_crlf() {
        let mut last = None;

        let normalized = normalize_tty_newlines(b"first\r\nsecond\r\n", &mut last);

        assert!(normalized.is_none());
        assert_eq!(last, Some(b'\n'));
    }

    #[test]
    fn normalize_tty_newlines_handles_ansi_colored_output() {
        let mut last = None;

        let normalized = normalize_tty_newlines(b"\x1b[31mred\x1b[0m\n", &mut last)
            .expect("colored LF should be normalized");

        assert_eq!(normalized, b"\x1b[31mred\x1b[0m\r\n");
        assert_eq!(last, Some(b'\n'));
    }

    #[test]
    fn normalize_tty_newlines_preserves_split_crlf_across_chunks() {
        let mut last = None;

        let first = normalize_tty_newlines(b"prefix\r", &mut last);
        let second = normalize_tty_newlines(b"\nsuffix\n", &mut last)
            .expect("second chunk should only normalize the bare trailing LF");

        assert!(first.is_none());
        assert_eq!(second, b"\nsuffix\r\n");
        assert_eq!(last, Some(b'\n'));
    }

    #[test]
    fn normalize_tty_newlines_preserves_carriage_return_progress_updates() {
        let mut last = None;

        let normalized = normalize_tty_newlines(b"loading\rstep2\r", &mut last);

        assert!(normalized.is_none());
        assert_eq!(last, Some(b'\r'));
    }
}

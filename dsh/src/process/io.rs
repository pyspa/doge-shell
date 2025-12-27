use anyhow::{Context as _, Result};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::unistd::pipe;
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::io::unix::AsyncFd;
use tokio::{fs, io, time};

use super::redirect::Redirect;
use crate::terminal::renderer::TerminalRenderer;
use dsh_types::Context;
use libc::STDIN_FILENO;

const MONITOR_TIMEOUT: u64 = 200;

#[derive(Debug)]
pub struct OutputMonitor {
    pub(crate) reader: io::BufReader<fs::File>,
    pub(crate) outputed: bool,
    pub captured_output: String,
    // Cached renderer to avoid repeated allocations.
    // Safe to hold as it no longer holds StdoutLock persistently.
    pub(crate) renderer: TerminalRenderer,
}

impl OutputMonitor {
    pub fn new(fd: RawFd) -> Self {
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let reader = io::BufReader::new(file);
        OutputMonitor {
            reader,
            outputed: false,
            captured_output: String::new(),
            renderer: TerminalRenderer::new(),
        }
    }

    fn append_line(&mut self, buffer: &mut String, line: &str, first_prefix: &str) {
        if !self.outputed {
            self.outputed = true;
            buffer.push_str(first_prefix);
        }
        buffer.push_str(line);
        // Also capture the raw line (we might want to be careful about prefixes/newlines)
        // The line from read_line includes the newline character usually.
        self.captured_output.push_str(line);
    }

    fn flush_buffer(&mut self, buffer: &str) -> Result<()> {
        if buffer.is_empty() {
            return Ok(());
        }

        self.renderer.write_all(buffer.as_bytes())?;
        self.renderer.flush()?;
        Ok(())
    }

    #[allow(dead_code)]
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
                    self.append_line(&mut buffer, &line, "\n\r");
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
                        self.append_line(&mut buffer, &line, "\r");
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
}

impl PtyMonitor {
    pub fn new(fd: RawFd) -> Result<Self> {
        let file = unsafe { std::fs::File::from_raw_fd(fd) };

        // Set non-blocking mode
        let current_flags =
            fcntl(file.as_raw_fd(), FcntlArg::F_GETFL).context("fcntl F_GETFL failed")?;
        let flags = OFlag::from_bits_truncate(current_flags) | OFlag::O_NONBLOCK;
        fcntl(file.as_raw_fd(), FcntlArg::F_SETFL(flags)).context("fcntl F_SETFL failed")?;

        let inner = AsyncFd::new(file).context("AsyncFd creation failed")?;

        Ok(PtyMonitor {
            inner,
            captured_output: Vec::new(),
        })
    }

    pub async fn process_output(&mut self) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        let mut buf = [0u8; 8192];
        let mut stdout = tokio::io::stdout();

        loop {
            let mut guard = self.inner.readable().await?;
            let res = guard.try_io(|inner| inner.get_ref().read(&mut buf));

            match res {
                Ok(Ok(0)) => {
                    tracing::debug!("PtyMonitor: EOF detected");
                    return Ok(());
                }
                Ok(Ok(n)) => {
                    tracing::debug!("PtyMonitor: Read {} bytes", n);
                    let data = &buf[..n];

                    // Print to real stdout (Passthrough) - use async write
                    if let Err(e) = stdout.write_all(data).await {
                        tracing::error!("PtyMonitor: Failed to write to stdout: {}", e);
                    }
                    if let Err(e) = stdout.flush().await {
                        tracing::error!("PtyMonitor: Failed to flush stdout: {}", e);
                    }

                    // Capture
                    self.captured_output.extend_from_slice(data);
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
                        return Ok(());
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
    }
}

pub(crate) fn create_pipe(ctx: &mut Context) -> Result<Option<RawFd>> {
    let (pout, pin) = pipe().context("failed pipe")?;
    ctx.outfile = pin;
    Ok(Some(pout))
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
                ctx.outfile = pin;
                Ok(Some(pout))
            }
            Redirect::StderrOutput(file) | Redirect::StderrAppend(file) => {
                tracing::debug!("🔀 REDIRECT: StderrOutput/Append to file: {}", file);
                let (pout, pin) = pipe().context("failed pipe")?;
                tracing::debug!(
                    "🔀 REDIRECT: Created redirect pipe - read_end={}, write_end={}",
                    pout,
                    pin
                );
                ctx.errfile = pin;
                tracing::debug!("🔀 REDIRECT: Set ctx.errfile={}", ctx.errfile);
                Ok(Some(pout))
            }
            Redirect::StdouterrOutput(file) | Redirect::StdouterrAppend(file) => {
                tracing::debug!("🔀 REDIRECT: StdouterrOutput/Append to file: {}", file);
                let (pout, pin) = pipe().context("failed pipe")?;
                tracing::debug!(
                    "🔀 REDIRECT: Created redirect pipe - read_end={}, write_end={}",
                    pout,
                    pin
                );
                ctx.outfile = pin;
                ctx.errfile = pin;
                tracing::debug!(
                    "🔀 REDIRECT: Set ctx.outfile={}, ctx.errfile={}",
                    ctx.outfile,
                    ctx.errfile
                );
                Ok(Some(pout))
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

use anyhow::{Context as _, Result};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use nix::unistd::{close, dup2, pipe};
use std::io::Write;
use std::os::unix::io::{FromRawFd, RawFd};
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use tokio::{fs, io, time};

use super::redirect::Redirect;
use crate::terminal::renderer::TerminalRenderer;
use dsh_types::Context;
use libc::STDIN_FILENO;

/// RAII wrapper for file descriptors to ensure proper cleanup
#[allow(dead_code)]
struct FileDescriptor {
    fd: RawFd,
    should_close: bool,
}

#[allow(dead_code)]
impl FileDescriptor {
    fn new(fd: RawFd) -> Self {
        Self {
            fd,
            should_close: true,
        }
    }

    fn new_no_close(fd: RawFd) -> Self {
        Self {
            fd,
            should_close: false,
        }
    }

    fn raw(&self) -> RawFd {
        self.fd
    }

    fn leak(mut self) -> RawFd {
        self.should_close = false;
        self.fd
    }
}

impl Drop for FileDescriptor {
    fn drop(&mut self) {
        if self.should_close && self.fd >= 0 {
            close(self.fd).ok(); // Ignore errors in destructor
        }
    }
}

struct RawModeGuard {
    disabled: bool,
}

impl RawModeGuard {
    fn new() -> Self {
        let disabled = disable_raw_mode().is_ok();
        Self { disabled }
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.disabled {
            enable_raw_mode().ok();
        }
    }
}

pub(crate) fn copy_fd(src: RawFd, dst: RawFd) -> Result<()> {
    if src != dst && src >= 0 && dst >= 0 {
        dup2(src, dst).map_err(|e| anyhow::anyhow!("dup2 failed: {}", e))?;

        // Only close if it's not a standard file descriptor
        if src > 2 {
            close(src).map_err(|e| anyhow::anyhow!("close failed: {}", e))?;
        }
    }
    Ok(())
}

const MONITOR_TIMEOUT: u64 = 200;

#[derive(Debug)]
pub struct OutputMonitor {
    pub(crate) reader: io::BufReader<fs::File>,
    pub(crate) outputed: bool,
}

impl OutputMonitor {
    pub fn new(fd: RawFd) -> Self {
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let reader = io::BufReader::new(file);
        OutputMonitor {
            reader,
            outputed: false,
        }
    }

    fn append_line(&mut self, buffer: &mut String, line: &str, first_prefix: &str) {
        if !self.outputed {
            self.outputed = true;
            buffer.push_str(first_prefix);
        }
        buffer.push_str(line);
    }

    fn flush_buffer(&mut self, buffer: &str) -> Result<()> {
        if buffer.is_empty() {
            return Ok(());
        }

        let _guard = RawModeGuard::new();
        let mut renderer = TerminalRenderer::new();
        renderer.write_all(buffer.as_bytes())?;
        renderer.flush()?;
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
            _ => {
                tracing::debug!("🔀 REDIRECT: No matching redirect pattern");
                Ok(None)
            }
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

//! Bounded, concurrent stdout/stderr draining for task-provider subprocesses.
use anyhow::{Context, Result, bail};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::io::{ErrorKind, Read};
use std::os::fd::AsFd;
use std::process::{Child, Output};
use std::time::{Duration, Instant};

// A ceiling per stream, with overflow reported rather than parsed as a partial
// successful response. Even at this exact size, probe EOF with a non-empty
// read buffer rather than mistaking a zero-sized read request for EOF.
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

pub(super) fn collect(mut child: Child, timeout: Duration) -> Result<Output> {
    let result = (|| {
        let mut stdout = child.stdout.take().context("missing provider stdout")?;
        let mut stderr = child.stderr.take().context("missing provider stderr")?;
        set_nonblocking(&stdout)?;
        set_nonblocking(&stderr)?;
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut out_eof = false;
        let mut err_eof = false;
        let mut status = None;
        loop {
            if !out_eof {
                out_eof = drain(&mut stdout, &mut out)?;
            }
            if !err_eof {
                err_eof = drain(&mut stderr, &mut err)?;
            }
            if status.is_none() {
                status = child.try_wait()?;
            }
            if let Some(status) = status
                && out_eof
                && err_eof
            {
                return Ok(Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
            if Instant::now() >= deadline {
                bail!("task provider timed out");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    if result.is_err() {
        // The spawning owner creates this process group. Kill descendants too,
        // including one that inherited a pipe after the direct child exited.
        let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn set_nonblocking(pipe: &impl AsFd) -> Result<()> {
    let flags = OFlag::from_bits_truncate(fcntl(pipe, FcntlArg::F_GETFL)?);
    fcntl(pipe, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    Ok(())
}

// Return true only on actual EOF; a short or WouldBlock read is not EOF.
fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>) -> Result<bool> {
    let mut buffer = [0; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                if bytes.len() + count > MAX_OUTPUT_BYTES {
                    bail!("task provider output limit exceeded");
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

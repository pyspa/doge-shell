//! A temporary reader owner while a parent builtin can block writing capture.
//!
//! No Tokio worker is required to make progress. The wake pipe ends this
//! interval without waiting for capture EOF: the Job still owns writer closure
//! and final record publication. Drop joins too, including cancellation/unwind.

use super::OutputMonitor;
use anyhow::{Context as _, Result, anyhow};
use std::os::fd::{AsRawFd, OwnedFd};
use std::thread::JoinHandle;

pub(crate) struct ConcurrentOutputDrain {
    wake: Option<OwnedFd>,
    worker: Option<JoinHandle<Result<OutputMonitor>>>,
}

impl ConcurrentOutputDrain {
    pub(crate) fn start(monitor: OutputMonitor) -> Result<Self> {
        let (wake_reader, wake_writer) = super::super::cloexec_pipe()?;
        let worker = std::thread::Builder::new()
            .name("builtin-capture".into())
            .spawn(move || drain_until_stopped(monitor, wake_reader))
            .context("failed to start parent builtin capture reader")?;
        Ok(Self {
            wake: Some(wake_writer),
            worker: Some(worker),
        })
    }

    pub(crate) fn finish(mut self) -> Result<OutputMonitor> {
        self.join()
    }

    fn join(&mut self) -> Result<OutputMonitor> {
        // Closing our only wake writer produces POLLHUP, even for a silent
        // producer. Never close the capture writer: Job owns that descriptor.
        drop(self.wake.take());
        self.worker
            .take()
            .expect("capture worker joined exactly once")
            .join()
            .map_err(|_| anyhow!("parent builtin capture reader panicked"))?
    }
}

impl Drop for ConcurrentOutputDrain {
    fn drop(&mut self) {
        if self.worker.is_some()
            && let Err(error) = self.join()
        {
            tracing::warn!(%error, "failed to stop parent builtin capture reader");
        }
    }
}

fn drain_until_stopped(mut monitor: OutputMonitor, wake: OwnedFd) -> Result<OutputMonitor> {
    let mut descriptors = [
        libc::pollfd {
            fd: monitor.inner.get_ref().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: both descriptors remain owned for the whole worker interval;
        // poll only writes revents in this live two-element array.
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, -1) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("parent builtin capture poll failed");
        }
        // Prioritize stop over a busy producer. Job will drain any remaining
        // bytes and pending fragment after closing its writer as usual.
        if descriptors[1].revents != 0 {
            return Ok(monitor);
        }
        if descriptors[0].revents != 0 {
            monitor.drain_ready_now()?;
            if descriptors[0].revents & libc::POLLHUP != 0 {
                return Ok(monitor);
            }
        }
    }
}

#[cfg(test)]
mod tests;

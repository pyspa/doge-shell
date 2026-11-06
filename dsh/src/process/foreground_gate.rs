//! Parent approval for external commands that share the shell's terminal.
//!
//! The child must not exec until its group owns the terminal. Release happens
//! before draining the exec-error pipe, whose EOF itself requires exec.

use super::io::cloexec_pipe;
use nix::unistd::{Pid, setpgid, tcsetpgrp, write};
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};
use tracing::debug;

pub(super) struct ForegroundGate {
    reader: OwnedFd,
    writer: OwnedFd,
}

impl ForegroundGate {
    pub(super) fn new() -> std::io::Result<Self> {
        let (reader, writer) = cloexec_pipe()?;
        Ok(Self { reader, writer })
    }

    pub(super) fn reader_fd(&self) -> RawFd {
        self.reader.as_raw_fd()
    }

    pub(super) fn writer_fd(&self) -> RawFd {
        self.writer.as_raw_fd()
    }

    /// No fallible return after fork: even a denied handoff leaves the child
    /// admitted through the existing Job path and reaped by its canonical waiter.
    /// Closing without a token aborts the child; EOF never authorizes exec.
    pub(super) fn release(self, child: Pid, pgid: Pid) {
        self.release_with(|| {
            #[cfg(test)]
            if super::foreground_handoff_tests::reject_handoff() {
                return Err(nix::errno::Errno::EPERM);
            }
            setpgid(child, pgid)?;
            tcsetpgrp(
                unsafe { BorrowedFd::borrow_raw(crate::shell::SHELL_TERMINAL) },
                pgid,
            )
        });
    }

    fn release_with(self, handoff: impl FnOnce() -> nix::Result<()>) {
        let Self { reader, writer } = self;
        drop(reader);
        if let Err(error) = handoff() {
            debug!("foreground launch handoff failed: {error}");
            return;
        }
        loop {
            match write(&writer, b"F") {
                Err(nix::errno::Errno::EINTR) => continue,
                result => {
                    if let Err(error) = result {
                        debug!("foreground launch gate release failed: {error}");
                    }
                    break;
                }
            }
        }
    }
}

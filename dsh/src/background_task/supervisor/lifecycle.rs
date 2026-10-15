//! Detect a disconnected worker during approval without consuming protocol data
//! or waiting on its PID. The supervisor remains the sole worker wait owner.

use super::*;
use std::os::fd::AsRawFd;

impl Shared {
    pub(super) fn worker_disconnected(&self) -> bool {
        // In-process test contexts have no worker. A reaped worker cannot retain
        // authority either; normal completion has already cleared its admission.
        if self.process.lock().is_none() {
            return false;
        }
        disconnected(&self.shutdown)
    }

    pub(super) fn cancel_disconnected(&self) {
        if !self.worker_disconnected() {
            return;
        }
        {
            let mut inner = self.inner.lock();
            inner.task.cancel();
            inner.pending = None;
            inner.admitted = None;
        }
        self.canceled.store(true, Ordering::Release);
        self.changed.notify_all();
        // Commands take jobs then recheck admission immediately before spawning.
        self.jobs.lock().cancel_all();
    }
}

fn disconnected(socket: &std::os::unix::net::UnixStream) -> bool {
    let mut fd = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // poll reports HUP/ERR regardless of requested events. It never reads
    // bytes, and buffered protocol frames therefore keep their single reader.
    let result = unsafe { libc::poll(&mut fd, 1, 0) };
    if result < 0 {
        return std::io::Error::last_os_error().kind() != ErrorKind::Interrupted;
    }
    if fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return true;
    }
    if fd.revents & libc::POLLIN != 0 {
        let mut byte = 0u8;
        // A write-half-close can report readable EOF without HUP on Unix.
        // Peek leaves any queued frame for the sole protocol reader.
        return unsafe {
            libc::recv(
                socket.as_raw_fd(),
                (&mut byte as *mut u8).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        } == 0;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn liveness_probe_preserves_frames_and_detects_half_close_and_full_close() {
        let (mut socket, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(!disconnected(&socket));
        peer.write_all(b"x").unwrap();
        assert!(!disconnected(&socket));
        let mut byte = [0];
        socket.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [b'x']);
        peer.shutdown(Shutdown::Write).unwrap();
        assert!(disconnected(&socket));
        drop(peer);
        assert!(disconnected(&socket));
    }
}

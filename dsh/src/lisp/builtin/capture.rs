//! One owned pipe reader for Lisp `sh!`, draining while the evaluator runs.
//!
//! A dedicated reader is necessary even on a current-thread runtime: parent
//! builtins can synchronously write more than the pipe capacity. It never waits
//! for children. Retirement drains ready bytes without waiting for inherited
//! writers (stopped/background jobs), and Drop also retires and joins it.
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};

pub(super) struct CaptureReader {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<io::Result<Vec<u8>>>>,
}

impl CaptureReader {
    pub(super) fn new(fd: OwnedFd) -> io::Result<Self> {
        let file = File::from(fd);
        let flags = fcntl(&file, FcntlArg::F_GETFL)?;
        fcntl(
            &file,
            FcntlArg::F_SETFL(OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK),
        )?;
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("lisp-capture".into())
            .spawn(move || drain(file, &reader_stop))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub(super) fn finish(mut self) -> io::Result<Vec<u8>> {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .expect("reader owner")
            .join()
            .map_err(|_| io::Error::other("sh! capture reader panicked"))?
    }
}

impl Drop for CaptureReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn drain(file: File, stop: &AtomicBool) -> io::Result<Vec<u8>> {
    let fd = file.as_raw_fd();
    drain_with(file, stop, || {
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll points to one initialized entry, owned by this reader.
        if unsafe { libc::poll(&mut poll, 1, 20) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(())
    })
}

fn drain_with(
    mut file: impl Read,
    stop: &AtomicBool,
    mut wait: impl FnMut() -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0; 65536];
    // More than a normal pipe's ready capacity on both supported OSes. Bound
    // retirement even if a descendant continues producing after eval returns;
    // report an error rather than silently return a truncated capture.
    let mut retiring = false;
    let mut retirement_budget: usize = 1024 * 1024;
    loop {
        retiring |= stop.load(Ordering::Acquire);
        match file.read(&mut chunk) {
            Ok(0) => return Ok(output),
            Ok(n) => {
                output.extend_from_slice(&chunk[..n]);
                if retiring {
                    retirement_budget = retirement_budget.saturating_sub(n);
                    if retirement_budget == 0 {
                        return Err(io::Error::other(
                            "sh! capture still producing after execution returned",
                        ));
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if retiring {
                    return Ok(output);
                }
                wait()?;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn retirement_bounds_a_producer_with_partial_chunks() {
        struct PartialFirst(bool);
        impl Read for PartialFirst {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                let n = if self.0 {
                    bytes.len()
                } else {
                    self.0 = true;
                    1
                };
                bytes[..n].fill(b'x');
                Ok(n)
            }
        }
        let error = drain_with(PartialFirst(false), &AtomicBool::new(true), || {
            panic!("ready producer")
        })
        .expect_err("retirement must fail rather than wrap or truncate");
        assert!(error.to_string().contains("still producing"));
    }

    #[test]
    fn drop_retires_reader_with_a_retained_writer() {
        let (read, _write) = crate::process::io::cloexec_pipe().unwrap();
        drop(CaptureReader::new(read).unwrap());
    }

    #[test]
    fn read_errors_are_reported() {
        let stop = AtomicBool::new(true);
        let file = tempfile::tempdir().unwrap();
        assert!(drain(File::open(file.path()).unwrap(), &stop).is_err());
    }

    #[test]
    fn retirement_does_not_wait_for_a_retained_writer() {
        let (read, write) = crate::process::io::cloexec_pipe().unwrap();
        let reader = CaptureReader::new(read).unwrap();
        let mut writer = File::from(write);
        writer.write_all(b"ready").unwrap();
        assert_eq!(reader.finish().unwrap(), b"ready");
    }
}

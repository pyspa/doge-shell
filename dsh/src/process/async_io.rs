use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, Interest, ReadBuf};

pub struct AsyncStdin {
    inner: AsyncFd<File>,
}

impl AsyncStdin {
    pub fn open_tty() -> std::io::Result<Self> {
        // On macOS, kqueue rejects the special /dev/tty alias with EINVAL.
        // Resolve fd 0 to the underlying terminal device, then open a distinct
        // file description so O_NONBLOCK cannot leak to stdout.
        let stdin = unsafe { BorrowedFd::borrow_raw(libc::STDIN_FILENO) };
        Self::open_tty_from_fd(stdin)
    }

    fn open_tty_from_fd(fd: BorrowedFd<'_>) -> std::io::Result<Self> {
        let path = nix::unistd::ttyname(fd).map_err(std::io::Error::from)?;
        Self::open_path(&path)
    }

    fn open_path(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        Ok(Self {
            inner: AsyncFd::with_interest(file, Interest::READABLE)?,
        })
    }
}

impl AsyncRead for AsyncStdin {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            let mut guard = match self.inner.poll_read_ready(cx) {
                Poll::Ready(Ok(guard)) => guard,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };

            let fd = self.inner.get_ref().as_raw_fd();
            // SAFETY: FFI call to read from raw fd
            let res = unsafe {
                libc::read(
                    fd,
                    buf.unfilled_mut().as_mut_ptr() as *mut libc::c_void,
                    buf.remaining(),
                )
            };

            if res < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    guard.clear_ready();
                    continue;
                }
                return Poll::Ready(Err(err));
            }

            let n = res as usize;
            unsafe { buf.assume_init(n) };
            buf.advance(n);
            return Poll::Ready(Ok(()));
        }
    }
}

pub struct AsyncPtyMasterWriter {
    inner: AsyncFd<File>,
}

impl AsyncPtyMasterWriter {
    pub fn new(file: File) -> std::io::Result<Self> {
        Ok(Self {
            inner: AsyncFd::with_interest(file, Interest::WRITABLE)?,
        })
    }
}

impl AsyncWrite for AsyncPtyMasterWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        loop {
            let mut guard = match self.inner.poll_write_ready(cx) {
                Poll::Ready(Ok(guard)) => guard,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };

            let fd = self.inner.get_ref().as_raw_fd();
            // SAFETY: FFI call to write to raw fd
            let res = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };

            if res < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    guard.clear_ready();
                    continue;
                }
                return Poll::Ready(Err(err));
            }

            return Poll::Ready(Ok(res as usize));
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::AsyncStdin;
    use crate::process::Pty;
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use std::os::fd::AsFd;

    #[tokio::test]
    async fn nonblocking_tty_reader_does_not_change_existing_handle_flags() {
        let pty = Pty::new().expect("create pty");
        let original_flags =
            OFlag::from_bits_truncate(fcntl(&pty.slave, FcntlArg::F_GETFL).expect("get flags"));

        let reader =
            AsyncStdin::open_tty_from_fd(pty.slave.as_fd()).expect("open independent tty reader");

        let unchanged_flags =
            OFlag::from_bits_truncate(fcntl(&pty.slave, FcntlArg::F_GETFL).expect("get flags"));
        let reader_flags = OFlag::from_bits_truncate(
            fcntl(reader.inner.get_ref(), FcntlArg::F_GETFL).expect("get reader flags"),
        );

        assert_eq!(unchanged_flags, original_flags);
        assert!(!original_flags.contains(OFlag::O_NONBLOCK));
        assert!(reader_flags.contains(OFlag::O_NONBLOCK));
    }
}

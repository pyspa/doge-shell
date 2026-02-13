use nix::libc::{F_GETFL, F_SETFL, O_NONBLOCK};
use std::os::unix::io::RawFd;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub struct AsyncStdin {
    pub inner: AsyncFd<RawFd>,
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

            let fd = self.inner.get_ref();
            // SAFETY: FFI call to read from raw fd
            let res = unsafe {
                libc::read(
                    *fd,
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
    inner: AsyncFd<RawFd>,
}

impl AsyncPtyMasterWriter {
    pub fn new(fd: RawFd) -> std::io::Result<Self> {
        Ok(Self {
            inner: AsyncFd::new(fd)?,
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

            let fd = self.inner.get_ref();
            // SAFETY: FFI call to write to raw fd
            let res = unsafe { libc::write(*fd, buf.as_ptr() as *const libc::c_void, buf.len()) };

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

pub struct NonBlockingFdGuard {
    fd: RawFd,
    orig_flags: Option<libc::c_int>,
}

impl NonBlockingFdGuard {
    pub fn new(fd: RawFd) -> Self {
        let orig_flags = unsafe { libc::fcntl(fd, F_GETFL) };
        if orig_flags >= 0 {
            unsafe { libc::fcntl(fd, F_SETFL, orig_flags | O_NONBLOCK) };
            Self {
                fd,
                orig_flags: Some(orig_flags),
            }
        } else {
            Self {
                fd,
                orig_flags: None,
            }
        }
    }
}

impl Drop for NonBlockingFdGuard {
    fn drop(&mut self) {
        if let Some(flags) = self.orig_flags {
            unsafe { libc::fcntl(self.fd, F_SETFL, flags) };
        }
    }
}

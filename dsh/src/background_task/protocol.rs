//! Bounded JSON messages over a private connected Unix socket, never stdin.
//! A deadline covers the entire frame, including partial I/O. Any framing or
//! transport error permanently closes the channel; callers must not replay it.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// Maximum JSON payload bytes; the terminating newline is not counted.
pub(super) const MAX_FRAME: usize = 2 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum WorkerMessage {
    Tool {
        serial: u64,
        name: String,
        arguments: String,
    },
    Confirm {
        serial: u64,
        message: String,
    },
    Complete {
        ok: bool,
        tokens_used: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ParentMessage {
    Reply {
        serial: u64,
        result: Result<String, String>,
    },
    Approval {
        serial: u64,
        allow: bool,
    },
}

// Never put model arguments, file contents or approval text into diagnostics.
impl std::fmt::Debug for WorkerMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tool { serial, .. } => f
                .debug_struct("Tool")
                .field("serial", serial)
                .finish_non_exhaustive(),
            Self::Confirm { serial, .. } => f
                .debug_struct("Confirm")
                .field("serial", serial)
                .finish_non_exhaustive(),
            Self::Complete { ok, tokens_used } => f
                .debug_struct("Complete")
                .field("ok", ok)
                .field("tokens_used", tokens_used)
                .finish(),
        }
    }
}
impl std::fmt::Debug for ParentMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reply { serial, result } => f
                .debug_struct("Reply")
                .field("serial", serial)
                .field("ok", &result.is_ok())
                .finish_non_exhaustive(),
            Self::Approval { serial, allow } => f
                .debug_struct("Approval")
                .field("serial", serial)
                .field("allow", allow)
                .finish(),
        }
    }
}

/// One owner per endpoint. Serial/epoch validation belongs to the host state
/// machine, not to this transport. Do not clone the socket into tool children.
pub(super) struct Channel {
    stream: UnixStream,
    incoming: Vec<u8>,
    failed: bool,
}

impl Channel {
    pub(super) fn new(stream: UnixStream) -> io::Result<Self> {
        // Reject disconnected endpoints and non-Unix descriptors supplied to a
        // hidden worker entry point. Bootstrap consumes its own descriptor.
        let fd = stream.as_raw_fd();
        let mut address: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let mut address_len = std::mem::size_of_val(&address) as libc::socklen_t;
        let mut socket_type: libc::c_int = 0;
        let mut type_len = std::mem::size_of_val(&socket_type) as libc::socklen_t;
        // SAFETY: stream owns fd and each output buffer has its declared size.
        if unsafe {
            libc::getsockname(
                fd,
                (&mut address as *mut libc::sockaddr_storage).cast(),
                &mut address_len,
            )
        } < 0
            || unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_TYPE,
                    (&mut socket_type as *mut libc::c_int).cast(),
                    &mut type_len,
                )
            } < 0
        {
            return Err(io::Error::last_os_error());
        }
        if address.ss_family as libc::c_int != libc::AF_UNIX || socket_type != libc::SOCK_STREAM {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "background channel requires a Unix stream",
            ));
        }
        stream.peer_addr()?;
        stream.set_nonblocking(true)?;
        // SAFETY: stream owns fd throughout both fcntl calls.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            stream,
            incoming: Vec::new(),
            failed: false,
        })
    }

    pub(super) fn send<T: Serialize>(&mut self, message: &T, timeout: Duration) -> io::Result<()> {
        self.ensure_open()?;
        let result = (|| {
            let deadline = deadline(timeout)?;
            let mut payload = BoundedPayload(Vec::new());
            serde_json::to_writer(&mut payload, message).map_err(io::Error::other)?;
            payload.0.push(b'\n');
            let mut remaining_bytes = payload.0.as_slice();
            while !remaining_bytes.is_empty() {
                remaining(deadline)?;
                match self.stream.write(remaining_bytes) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(count) => remaining_bytes = &remaining_bytes[count..],
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        wait_ready(&self.stream, libc::POLLOUT, deadline)?;
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })();
        self.finish(result)
    }

    pub(super) fn receive<T: DeserializeOwned>(&mut self, timeout: Duration) -> io::Result<T> {
        self.ensure_open()?;
        let result = (|| {
            let deadline = deadline(timeout)?;
            let mut scanned = 0;
            loop {
                remaining(deadline)?;
                if let Some(relative_end) = self.incoming[scanned..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                {
                    let end = scanned + relative_end;
                    if end > MAX_FRAME {
                        return Err(io::Error::other("background frame exceeds limit"));
                    }
                    let message = serde_json::from_slice(&self.incoming[..end])
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                    self.incoming.drain(..=end);
                    return Ok(message);
                }
                if self.incoming.len() > MAX_FRAME {
                    return Err(io::Error::other("background frame exceeds limit"));
                }
                scanned = self.incoming.len();
                let mut chunk = [0u8; 8192];
                let capacity = chunk.len().min(MAX_FRAME + 1 - self.incoming.len());
                match self.stream.read(&mut chunk[..capacity]) {
                    Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                    Ok(count) => self.incoming.extend_from_slice(&chunk[..count]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        wait_ready(&self.stream, libc::POLLIN, deadline)?;
                    }
                    Err(error) => return Err(error),
                }
            }
        })();
        self.finish(result)
    }

    fn ensure_open(&self) -> io::Result<()> {
        if self.failed {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "background channel is closed",
            ))
        } else {
            Ok(())
        }
    }

    fn finish<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        if result.is_err() {
            self.failed = true;
            self.incoming.clear();
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
        }
        result
    }
}

struct BoundedPayload(Vec<u8>);
impl Write for BoundedPayload {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME.saturating_sub(self.0.len()) {
            return Err(io::Error::other("background frame exceeds limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// Do not change socket options between partial reads. Darwin rejects
// setsockopt after a peer closes even when its final frame is still buffered.
// Nonblocking I/O plus poll retains the whole-frame deadline on both platforms.
fn wait_ready(stream: &UnixStream, events: libc::c_short, deadline: Instant) -> io::Result<()> {
    loop {
        let left = remaining(deadline)?;
        let timeout_ms = left
            .as_nanos()
            .div_ceil(1_000_000)
            .min(libc::c_int::MAX as u128) as libc::c_int;
        let mut fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        // SAFETY: stream owns this descriptor and poll receives one initialized entry.
        let result = unsafe { libc::poll(&mut fd, 1, timeout_ms) };
        if result > 0 {
            // HUP also wakes the caller: read must drain queued data before EOF.
            // The following nonblocking IO reports any actual transport error.
            return Ok(());
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

fn deadline(timeout: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid frame deadline"))
}
fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "background frame deadline expired"))
}

#[cfg(test)]
mod tests {
    use super::*;
    const TIMEOUT: Duration = Duration::from_secs(2);

    #[test]
    fn typed_messages_preserve_escaped_newlines_and_adjacent_frames() {
        let (left, right) = UnixStream::pair().unwrap();
        let mut worker = Channel::new(left).unwrap();
        let mut parent = Channel::new(right).unwrap();
        // A wire fixture independent of send() catches symmetric encoder /
        // decoder framing mistakes as well as escaped and adjacent frames.
        worker.stream.write_all(
            b"{\"type\":\"tool\",\"serial\":7,\"name\":\"read_file\",\"arguments\":\"line1\\nline2\"}\n{\"type\":\"complete\",\"ok\":true,\"tokens_used\":12}\n"
        ).unwrap();
        assert!(
            matches!(parent.receive::<WorkerMessage>(TIMEOUT).unwrap(), WorkerMessage::Tool { serial: 7, arguments, .. } if arguments == "line1\nline2")
        );
        assert!(matches!(
            parent.receive::<WorkerMessage>(TIMEOUT).unwrap(),
            WorkerMessage::Complete {
                ok: true,
                tokens_used: 12
            }
        ));
        parent
            .send(
                &ParentMessage::Reply {
                    serial: 7,
                    result: Err("denied".into()),
                },
                TIMEOUT,
            )
            .unwrap();
        assert!(
            matches!(worker.receive::<ParentMessage>(TIMEOUT).unwrap(), ParentMessage::Reply { serial: 7, result: Err(reason) } if reason == "denied")
        );
        let flags = unsafe { libc::fcntl(worker.stream.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        // The final complete frame can already be queued when the peer exits.
        // Receive must not configure socket options on a disconnected endpoint.
        parent
            .send(
                &ParentMessage::Approval {
                    serial: 9,
                    allow: true,
                },
                TIMEOUT,
            )
            .unwrap();
        drop(parent);
        assert!(matches!(
            worker.receive::<ParentMessage>(TIMEOUT).unwrap(),
            ParentMessage::Approval {
                serial: 9,
                allow: true
            }
        ));
        assert_eq!(
            worker.receive::<ParentMessage>(TIMEOUT).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        let secret = WorkerMessage::Confirm {
            serial: 8,
            message: "do-not-log".into(),
        };
        assert!(!format!("{secret:?}").contains("do-not-log"));
    }

    #[test]
    fn malformed_unknown_and_incomplete_messages_close_the_channel() {
        for frame in [
            b"not json\n".as_slice(),
            b"{\"type\":\"complete\",\"ok\":true,\"tokens_used\":0,\"extra\":1}\n",
            b"{\"type\":\"complete\"",
        ] {
            let (left, mut right) = UnixStream::pair().unwrap();
            let mut channel = Channel::new(left).unwrap();
            right.write_all(frame).unwrap();
            right.shutdown(std::net::Shutdown::Write).unwrap();
            assert!(channel.receive::<WorkerMessage>(TIMEOUT).is_err());
            assert_eq!(
                channel
                    .receive::<WorkerMessage>(TIMEOUT)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::BrokenPipe
            );
        }
    }

    #[test]
    fn bootstrap_rejects_a_datagram_descriptor() {
        let (datagram, _peer) = std::os::unix::net::UnixDatagram::pair().unwrap();
        let fd: std::os::fd::OwnedFd = datagram.into();
        assert!(Channel::new(UnixStream::from(fd)).is_err());
    }

    #[test]
    fn exact_limit_is_accepted_and_larger_input_is_rejected() {
        for extra in [0, 1] {
            let (left, mut right) = UnixStream::pair().unwrap();
            let mut channel = Channel::new(left).unwrap();
            // Keep fixture construction and sender startup outside the frame
            // deadline, so this checks the byte boundary rather than startup.
            let frame = format!("\"{}\"\n", "x".repeat(MAX_FRAME - 2 + extra));
            let (ready, started) = std::sync::mpsc::sync_channel(0);
            let sender = std::thread::spawn(move || {
                ready.send(()).unwrap();
                right.write_all(frame.as_bytes())
            });
            started
                .recv_timeout(TIMEOUT)
                .expect("frame sender did not start");
            let result = channel.receive::<String>(TIMEOUT);
            let sent = sender.join().expect("frame sender panicked");
            if extra == 0 {
                let payload = result.unwrap_or_else(|error| {
                    panic!(
                        "exact-limit frame rejected: kind={:?}, error={error}; sender={sent:?}",
                        error.kind()
                    )
                });
                assert_eq!(payload.len(), MAX_FRAME - 2);
                sent.expect("exact-limit frame sender failed");
            } else {
                let error = result.expect_err("oversized frame was accepted");
                assert_eq!(
                    error.kind(),
                    io::ErrorKind::Other,
                    "{error}; sender={sent:?}"
                );
                assert_eq!(error.to_string(), "background frame exceeds limit");
                // The receiver deliberately closes early on an oversized
                // frame; a sender broken pipe is legitimate in this branch.
                if let Err(error) = sent {
                    assert!(
                        matches!(
                            error.kind(),
                            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                        ),
                        "unexpected oversized-frame sender failure: {error}"
                    );
                }
                assert_eq!(
                    channel.receive::<String>(TIMEOUT).unwrap_err().kind(),
                    io::ErrorKind::BrokenPipe
                );
            }
        }
    }

    #[test]
    fn oversized_output_fails_before_writing_a_frame() {
        let (left, mut right) = UnixStream::pair().unwrap();
        let mut channel = Channel::new(left).unwrap();
        assert!(channel.send(&"x".repeat(MAX_FRAME), TIMEOUT).is_err());
        let mut bytes = [0];
        assert_eq!(right.read(&mut bytes).unwrap(), 0);
    }

    #[test]
    fn stalled_read_and_write_are_bounded_and_not_reusable() {
        let (left, _right) = UnixStream::pair().unwrap();
        let mut channel = Channel::new(left).unwrap();
        let start = Instant::now();
        assert_eq!(
            channel
                .receive::<WorkerMessage>(Duration::from_millis(25))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(start.elapsed() < TIMEOUT);
        assert_eq!(
            channel
                .send(
                    &WorkerMessage::Complete {
                        ok: false,
                        tokens_used: 0
                    },
                    TIMEOUT
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );

        let (left, _right) = UnixStream::pair().unwrap();
        let send_buffer: libc::c_int = 4096;
        // SAFETY: left owns the socket; option points at one initialized int.
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    left.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&send_buffer as *const libc::c_int).cast(),
                    std::mem::size_of_val(&send_buffer) as libc::socklen_t,
                )
            },
            0
        );
        let mut channel = Channel::new(left).unwrap();
        let start = Instant::now();
        // More than the configured send buffer, with no peer consuming it.
        assert_eq!(
            channel
                .send(&"x".repeat(MAX_FRAME - 2), Duration::from_millis(25))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(start.elapsed() < TIMEOUT);
    }
}

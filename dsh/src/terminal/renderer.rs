const DEFAULT_BUFFER_CAPACITY: usize = 4096;
use std::io::{self, Write};

pub(crate) fn flush_stdout_bytes(bytes: &[u8]) -> io::Result<()> {
    // Escape sequences bypass libtest's capture and land on the terminal that
    // is running `cargo test` — cursor moves, `ED` erases and OSC 133 prompt
    // markers included. Tests that care about rendering write into a `Vec<u8>`
    // instead, so dropping the bytes here costs them nothing.
    if !super::terminal_control_enabled() {
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    write_all_with_wait(&mut handle, bytes, wait_stdout_writable)
}

fn write_all_with_wait<W, F>(writer: &mut W, mut bytes: &[u8], mut wait: F) -> io::Result<()>
where
    W: Write,
    F: FnMut() -> io::Result<()>,
{
    while !bytes.is_empty() {
        match writer.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write terminal output",
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => wait()?,
            Err(err) => return Err(err),
        }
    }

    loop {
        match writer.flush() {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => wait()?,
            Err(err) => return Err(err),
        }
    }
}

fn wait_stdout_writable() -> io::Result<()> {
    let mut descriptor = libc::pollfd {
        fd: libc::STDOUT_FILENO,
        events: libc::POLLOUT,
        revents: 0,
    };

    loop {
        let result = unsafe { libc::poll(&mut descriptor, 1, -1) };
        if result > 0 {
            let failure_events = libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
            if descriptor.revents & failure_events != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    format!("stdout poll failed with events 0x{:x}", descriptor.revents),
                ));
            }
            if descriptor.revents & libc::POLLOUT != 0 {
                return Ok(());
            }
        } else if result < 0 {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }
}

/// Buffered terminal renderer that batches commands before flushing to stdout.
/// Does not hold StdoutLock persistently to allow safe reuse and sharing.
#[derive(Debug)]
pub struct TerminalRenderer {
    buffer: Vec<u8>,
}

impl Default for TerminalRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalRenderer {
    /// Create a renderer with default buffer capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_BUFFER_CAPACITY)
    }

    /// Create a renderer with a custom initial buffer capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        TerminalRenderer {
            buffer: Vec::with_capacity(capacity.max(1)),
        }
    }

    /// Flush buffered commands to the terminal and clear the buffer.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        flush_stdout_bytes(&self.buffer)?;
        self.buffer.clear();
        Ok(())
    }
}

impl Write for TerminalRenderer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // Delegate to inherent flush method to actually write to stdout
        TerminalRenderer::flush(self)
    }
}

#[cfg(test)]
mod tests {
    use super::write_all_with_wait;
    use std::io::{self, Write};

    #[derive(Default)]
    struct RecoverableWriter {
        bytes: Vec<u8>,
        write_attempts: usize,
        flush_attempts: usize,
    }

    impl Write for RecoverableWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.write_attempts += 1;
            match self.write_attempts {
                1 => Err(io::Error::from(io::ErrorKind::Interrupted)),
                2 => Err(io::Error::from(io::ErrorKind::WouldBlock)),
                3 => {
                    let written = bytes.len().min(2);
                    self.bytes.extend_from_slice(&bytes[..written]);
                    Ok(written)
                }
                _ => {
                    self.bytes.extend_from_slice(bytes);
                    Ok(bytes.len())
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flush_attempts += 1;
            if self.flush_attempts == 1 {
                Err(io::Error::from(io::ErrorKind::WouldBlock))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn retries_interrupted_would_block_and_partial_writes_without_data_loss() {
        let mut writer = RecoverableWriter::default();
        let mut waits = 0;

        write_all_with_wait(&mut writer, b"colored output\r\n", || {
            waits += 1;
            Ok(())
        })
        .expect("write all bytes");

        assert_eq!(writer.bytes, b"colored output\r\n");
        assert_eq!(waits, 2);
        assert_eq!(writer.write_attempts, 4);
        assert_eq!(writer.flush_attempts, 2);
    }
}

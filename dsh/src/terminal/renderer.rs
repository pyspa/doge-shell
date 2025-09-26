use std::io::{self, StdoutLock, Write};

const DEFAULT_BUFFER_CAPACITY: usize = 4096;

/// Buffered terminal renderer that batches commands before flushing to stdout.
pub struct TerminalRenderer {
    stdout: StdoutLock<'static>,
    buffer: Vec<u8>,
}

impl TerminalRenderer {
    /// Create a renderer with default buffer capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_BUFFER_CAPACITY)
    }

    /// Create a renderer with a custom initial buffer capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        let stdout = std::io::stdout();
        TerminalRenderer {
            stdout: stdout.lock(),
            buffer: Vec::with_capacity(capacity.max(1)),
        }
    }

    /// Flush buffered commands to the terminal and clear the buffer.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        self.stdout.write_all(&self.buffer)?;
        self.stdout.flush()?;
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
        // No-op: buffer is flushed explicitly via `flush` / `finish`.
        Ok(())
    }
}

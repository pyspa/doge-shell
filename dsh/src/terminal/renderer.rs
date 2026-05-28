const DEFAULT_BUFFER_CAPACITY: usize = 4096;
use std::io::{self, Write};

pub(crate) fn flush_stdout_bytes(bytes: &[u8]) -> io::Result<()> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    if !bytes.is_empty() {
        handle.write_all(bytes)?;
    }
    handle.flush()
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

use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObservedOutputSnapshot {
    pub stdout: String,
    pub stderr: String,
}

impl ObservedOutputSnapshot {
    pub fn is_empty(&self) -> bool {
        self.stdout.is_empty() && self.stderr.is_empty()
    }
}

#[derive(Debug)]
pub struct ObservedOutput {
    stdout: String,
    stderr: String,
    max_bytes_per_stream: usize,
}

pub type SharedOutputObserver = Arc<Mutex<ObservedOutput>>;

impl ObservedOutput {
    pub fn new(max_bytes_per_stream: usize) -> Self {
        Self {
            stdout: String::new(),
            stderr: String::new(),
            max_bytes_per_stream,
        }
    }

    pub fn shared(max_bytes_per_stream: usize) -> SharedOutputObserver {
        Arc::new(Mutex::new(Self::new(max_bytes_per_stream)))
    }

    pub fn append(&mut self, stream: ObservedStream, chunk: &str) {
        match stream {
            ObservedStream::Stdout => {
                append_bounded(&mut self.stdout, chunk, self.max_bytes_per_stream)
            }
            ObservedStream::Stderr => {
                append_bounded(&mut self.stderr, chunk, self.max_bytes_per_stream)
            }
        }
    }

    pub fn snapshot(&self) -> ObservedOutputSnapshot {
        ObservedOutputSnapshot {
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
        }
    }
}

fn append_bounded(buffer: &mut String, chunk: &str, max_bytes: usize) {
    if max_bytes == 0 || chunk.is_empty() {
        return;
    }

    buffer.push_str(chunk);
    if buffer.len() <= max_bytes {
        return;
    }

    const PREFIX: &str = "... (truncated)\n";
    let keep_bytes = max_bytes.saturating_sub(PREFIX.len());
    if keep_bytes == 0 {
        buffer.clear();
        return;
    }

    let mut start = buffer.len().saturating_sub(keep_bytes);
    while start < buffer.len() && !buffer.is_char_boundary(start) {
        start += 1;
    }

    let tail = buffer[start..].to_string();
    buffer.clear();
    buffer.push_str(PREFIX);
    buffer.push_str(&tail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_collects_streams() {
        let mut output = ObservedOutput::new(1024);
        output.append(ObservedStream::Stdout, "hello\n");
        output.append(ObservedStream::Stderr, "warn\n");

        let snapshot = output.snapshot();
        assert_eq!(snapshot.stdout, "hello\n");
        assert_eq!(snapshot.stderr, "warn\n");
    }

    #[test]
    fn observer_truncates_on_char_boundary() {
        let mut output = ObservedOutput::new(24);
        output.append(ObservedStream::Stdout, &"あ".repeat(20));

        let snapshot = output.snapshot();
        assert!(snapshot.stdout.len() <= 24);
        assert!(snapshot.stdout.is_char_boundary(snapshot.stdout.len()));
    }
}

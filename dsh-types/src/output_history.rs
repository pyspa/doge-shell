//! Output history management for command output capture
//!
//! Provides a ring buffer to store recent command outputs,
//! accessible via $OUT[N] and $ERR[N] variables.

use std::collections::VecDeque;
use std::time::SystemTime;

/// Default maximum number of output entries to keep
const DEFAULT_MAX_ENTRIES: usize = 100;

/// Default maximum size per entry (1MB)
const DEFAULT_MAX_SIZE_PER_ENTRY: usize = 1024 * 1024;

/// Default maximum total size (50MB)
const DEFAULT_MAX_TOTAL_SIZE: usize = 50 * 1024 * 1024;

/// A single output history entry
#[derive(Debug, Clone)]
pub struct OutputEntry {
    /// The command that was executed
    pub command: String,
    /// Standard output from the command
    pub stdout: String,
    /// Standard error from the command
    pub stderr: String,
    /// Exit code of the command
    pub exit_code: i32,
    /// Timestamp when the command was executed
    pub timestamp: SystemTime,
}

impl OutputEntry {
    /// Create a new output entry
    pub fn new(command: String, stdout: String, stderr: String, exit_code: i32) -> Self {
        Self {
            command,
            stdout,
            stderr,
            exit_code,
            timestamp: SystemTime::now(),
        }
    }

    /// Get the total size of this entry in bytes
    pub fn size(&self) -> usize {
        self.command.len() + self.stdout.len() + self.stderr.len()
    }

    /// Truncate the output if it exceeds the given size
    pub fn truncate(&mut self, max_size: usize) {
        let current_size = self.stdout.len() + self.stderr.len();
        if current_size <= max_size {
            return;
        }

        // Distribute the max size between stdout and stderr proportionally
        let total = self.stdout.len() + self.stderr.len();
        if total == 0 {
            return;
        }

        let stdout_ratio = self.stdout.len() as f64 / total as f64;
        let stdout_max = (max_size as f64 * stdout_ratio) as usize;
        let stderr_max = max_size.saturating_sub(stdout_max);

        if self.stdout.len() > stdout_max {
            self.stdout.truncate(stdout_max);
            self.stdout.push_str("\n... (truncated)");
        }
        if self.stderr.len() > stderr_max {
            self.stderr.truncate(stderr_max);
            self.stderr.push_str("\n... (truncated)");
        }
    }
}

/// Ring buffer for storing command output history
#[derive(Debug)]
pub struct OutputHistory {
    /// The stored entries (most recent first)
    entries: VecDeque<OutputEntry>,
    /// Maximum number of entries to keep
    max_entries: usize,
    /// Maximum size per entry
    max_size_per_entry: usize,
    /// Current total size of all entries
    total_size: usize,
    /// Maximum total size
    max_total_size: usize,
}

impl Default for OutputHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputHistory {
    /// Create a new output history with default settings
    pub fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(DEFAULT_MAX_ENTRIES),
            max_entries: DEFAULT_MAX_ENTRIES,
            max_size_per_entry: DEFAULT_MAX_SIZE_PER_ENTRY,
            total_size: 0,
            max_total_size: DEFAULT_MAX_TOTAL_SIZE,
        }
    }

    /// Create a new output history with custom settings
    pub fn with_capacity(
        max_entries: usize,
        max_size_per_entry: usize,
        max_total_size: usize,
    ) -> Self {
        Self {
            entries: VecDeque::with_capacity(max_entries),
            max_entries,
            max_size_per_entry,
            total_size: 0,
            max_total_size,
        }
    }

    /// Push a new entry to the history
    ///
    /// The entry is added to the front (index 1 = most recent)
    /// Older entries are evicted if necessary to maintain size limits
    pub fn push(&mut self, mut entry: OutputEntry) {
        // Truncate entry if it exceeds per-entry limit
        entry.truncate(self.max_size_per_entry);
        let entry_size = entry.size();

        // Remove old entries if we exceed max entries
        while self.entries.len() >= self.max_entries {
            if let Some(old) = self.entries.pop_back() {
                self.total_size = self.total_size.saturating_sub(old.size());
            }
        }

        // Remove old entries if we exceed total size limit
        while self.total_size + entry_size > self.max_total_size && !self.entries.is_empty() {
            if let Some(old) = self.entries.pop_back() {
                self.total_size = self.total_size.saturating_sub(old.size());
            }
        }

        // Add the new entry to the front
        self.total_size += entry_size;
        self.entries.push_front(entry);
    }

    /// Get an entry by index (1-based, 1 = most recent)
    pub fn get(&self, index: usize) -> Option<&OutputEntry> {
        if index == 0 || index > self.entries.len() {
            None
        } else {
            self.entries.get(index - 1)
        }
    }

    /// Get stdout by index (1-based, 1 = most recent)
    pub fn get_stdout(&self, index: usize) -> Option<&str> {
        self.get(index).map(|e| e.stdout.as_str())
    }

    /// Get stderr by index (1-based, 1 = most recent)
    pub fn get_stderr(&self, index: usize) -> Option<&str> {
        self.get(index).map(|e| e.stderr.as_str())
    }

    /// Get the most recent stdout
    pub fn last_stdout(&self) -> Option<&str> {
        self.get_stdout(1)
    }

    /// Get the most recent stderr
    pub fn last_stderr(&self) -> Option<&str> {
        self.get_stderr(1)
    }

    /// Get the number of entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the history is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clear all entries
    pub fn clear(&mut self) {
        self.entries.clear();
        self.total_size = 0;
    }

    /// Get an iterator over all entries (most recent first)
    pub fn iter(&self) -> impl Iterator<Item = &OutputEntry> {
        self.entries.iter()
    }

    /// Get the current total size
    pub fn total_size(&self) -> usize {
        self.total_size
    }
    /// Get all entries as a vector (recent first)
    pub fn get_all_entries(&self) -> Vec<OutputEntry> {
        self.entries.iter().cloned().collect()
    }
}

/// Parse an output variable name like "$OUT[N]" or "OUT[N]"
///
/// Returns the index if the variable matches the pattern
pub fn parse_output_var(key: &str, prefix: &str) -> Option<usize> {
    let key = key.strip_prefix('$').unwrap_or(key);

    // Check for simple form: "OUT" or "ERR" (index 1)
    if key == prefix {
        return Some(1);
    }

    // Check for indexed form: "OUT[N]" or "ERR[N]"
    let pattern = format!("{}[", prefix);
    if key.starts_with(&pattern) && key.ends_with(']') {
        let inner = &key[pattern.len()..key.len() - 1];
        return inner.parse().ok();
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_entry_new() {
        let entry = OutputEntry::new(
            "ls".to_string(),
            "file1\nfile2".to_string(),
            "".to_string(),
            0,
        );
        assert_eq!(entry.command, "ls");
        assert_eq!(entry.stdout, "file1\nfile2");
        assert_eq!(entry.exit_code, 0);
    }

    #[test]
    fn test_output_entry_size() {
        let entry = OutputEntry::new(
            "cmd".to_string(), // 3 bytes
            "out".to_string(), // 3 bytes
            "err".to_string(), // 3 bytes
            0,
        );
        assert_eq!(entry.size(), 9);
    }

    #[test]
    fn test_output_entry_truncate() {
        let mut entry = OutputEntry::new("cmd".to_string(), "a".repeat(100), "b".repeat(100), 0);
        entry.truncate(50);
        assert!(entry.stdout.len() + entry.stderr.len() <= 100); // some overhead for truncation message
    }

    #[test]
    fn test_output_history_push_and_get() {
        let mut history = OutputHistory::new();

        history.push(OutputEntry::new("cmd1".into(), "out1".into(), "".into(), 0));
        history.push(OutputEntry::new("cmd2".into(), "out2".into(), "".into(), 0));
        history.push(OutputEntry::new("cmd3".into(), "out3".into(), "".into(), 0));

        assert_eq!(history.len(), 3);
        assert_eq!(history.get_stdout(1), Some("out3")); // Most recent
        assert_eq!(history.get_stdout(2), Some("out2"));
        assert_eq!(history.get_stdout(3), Some("out1")); // Oldest
        assert_eq!(history.get_stdout(4), None); // Out of bounds
    }

    #[test]
    fn test_output_history_max_entries() {
        let mut history = OutputHistory::with_capacity(3, 1024, 1024 * 1024);

        history.push(OutputEntry::new("cmd1".into(), "out1".into(), "".into(), 0));
        history.push(OutputEntry::new("cmd2".into(), "out2".into(), "".into(), 0));
        history.push(OutputEntry::new("cmd3".into(), "out3".into(), "".into(), 0));
        history.push(OutputEntry::new("cmd4".into(), "out4".into(), "".into(), 0));

        assert_eq!(history.len(), 3);
        assert_eq!(history.get_stdout(1), Some("out4")); // Most recent
        assert_eq!(history.get_stdout(3), Some("out2")); // Oldest (cmd1 was evicted)
    }

    #[test]
    fn test_output_history_clear() {
        let mut history = OutputHistory::new();
        history.push(OutputEntry::new("cmd1".into(), "out1".into(), "".into(), 0));
        history.push(OutputEntry::new("cmd2".into(), "out2".into(), "".into(), 0));

        history.clear();

        assert!(history.is_empty());
        assert_eq!(history.total_size(), 0);
    }

    #[test]
    fn test_parse_output_var() {
        // Simple form
        assert_eq!(parse_output_var("OUT", "OUT"), Some(1));
        assert_eq!(parse_output_var("$OUT", "OUT"), Some(1));
        assert_eq!(parse_output_var("ERR", "ERR"), Some(1));
        assert_eq!(parse_output_var("$ERR", "ERR"), Some(1));

        // Indexed form
        assert_eq!(parse_output_var("OUT[1]", "OUT"), Some(1));
        assert_eq!(parse_output_var("$OUT[1]", "OUT"), Some(1));
        assert_eq!(parse_output_var("OUT[5]", "OUT"), Some(5));
        assert_eq!(parse_output_var("ERR[3]", "ERR"), Some(3));

        // Invalid
        assert_eq!(parse_output_var("OUT[abc]", "OUT"), None);
        assert_eq!(parse_output_var("FOO", "OUT"), None);
        assert_eq!(parse_output_var("OUT[", "OUT"), None);
    }

    #[test]
    fn test_output_history_last_stdout() {
        let mut history = OutputHistory::new();
        assert_eq!(history.last_stdout(), None);

        history.push(OutputEntry::new(
            "cmd1".into(),
            "out1".into(),
            "err1".into(),
            0,
        ));
        assert_eq!(history.last_stdout(), Some("out1"));
        assert_eq!(history.last_stderr(), Some("err1"));
    }
}

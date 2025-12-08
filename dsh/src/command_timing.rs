//! Command timing statistics module for dsh
//!
//! Provides structures and functions for tracking command execution statistics.
//! This module is used by both the REPL (for recording) and the timing builtin command (for display).

use chrono::{DateTime, Duration, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tracing::{debug, warn};

/// Statistics for a single command
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandStats {
    /// The base command name (e.g., "git", "cargo")
    pub command: String,
    /// Total number of times the command was executed
    pub total_calls: u64,
    /// Total execution time in milliseconds
    pub total_duration_ms: u64,
    /// Maximum execution time in milliseconds
    pub max_duration_ms: u64,
    /// Minimum execution time in milliseconds
    pub min_duration_ms: u64,
    /// Number of failed executions (non-zero exit code)
    pub failures: u64,
    /// Last execution timestamp
    pub last_executed: DateTime<Utc>,
}

impl CommandStats {
    /// Create new statistics for a command
    pub fn new(command: String, duration_ms: u64, success: bool) -> Self {
        Self {
            command,
            total_calls: 1,
            total_duration_ms: duration_ms,
            max_duration_ms: duration_ms,
            min_duration_ms: duration_ms,
            failures: if success { 0 } else { 1 },
            last_executed: Utc::now(),
        }
    }

    /// Record another execution of this command
    pub fn record(&mut self, duration_ms: u64, success: bool) {
        self.total_calls += 1;
        self.total_duration_ms += duration_ms;
        self.max_duration_ms = self.max_duration_ms.max(duration_ms);
        self.min_duration_ms = self.min_duration_ms.min(duration_ms);
        if !success {
            self.failures += 1;
        }
        self.last_executed = Utc::now();
    }

    /// Calculate average execution time in milliseconds
    pub fn average_duration_ms(&self) -> u64 {
        if self.total_calls == 0 {
            0
        } else {
            self.total_duration_ms / self.total_calls
        }
    }

    /// Calculate success rate as a percentage
    pub fn success_rate(&self) -> f64 {
        if self.total_calls == 0 {
            100.0
        } else {
            ((self.total_calls - self.failures) as f64 / self.total_calls as f64) * 100.0
        }
    }
}

/// Container for all command timing statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommandTiming {
    /// Statistics indexed by command name
    pub stats: HashMap<String, CommandStats>,
    /// Timestamp of when statistics started being collected
    pub collection_started: Option<DateTime<Utc>>,
    /// Flag to track if there are unsaved changes
    #[serde(skip)]
    dirty: bool,
}

impl CommandTiming {
    /// Create a new empty CommandTiming instance
    pub fn new() -> Self {
        Self {
            stats: HashMap::new(),
            collection_started: Some(Utc::now()),
            dirty: false,
        }
    }

    /// Load timing data from a file
    pub fn load_from_file(path: &PathBuf) -> Option<Self> {
        if !path.exists() {
            return None;
        }

        match File::open(path) {
            Ok(file) => {
                let reader = BufReader::new(file);
                match serde_json::from_reader(reader) {
                    Ok(timing) => {
                        debug!("Loaded command timing from {:?}", path);
                        Some(timing)
                    }
                    Err(e) => {
                        warn!("Failed to parse timing file: {}", e);
                        None
                    }
                }
            }
            Err(e) => {
                warn!("Failed to open timing file: {}", e);
                None
            }
        }
    }

    /// Save timing data to a file
    pub fn save_to_file(&mut self, path: &PathBuf) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, self)?;
        self.dirty = false;
        debug!("Saved command timing to {:?}", path);
        Ok(())
    }

    /// Force save regardless of dirty flag
    pub fn force_save(&mut self, path: &PathBuf) -> std::io::Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, self)?;
        self.dirty = false;
        debug!("Force saved command timing to {:?}", path);
        Ok(())
    }

    /// Record a command execution
    pub fn record(&mut self, command: &str, exit_code: i32, duration: StdDuration) {
        let duration_ms = duration.as_millis() as u64;
        let success = exit_code == 0;

        if let Some(stats) = self.stats.get_mut(command) {
            stats.record(duration_ms, success);
        } else {
            self.stats.insert(
                command.to_string(),
                CommandStats::new(command.to_string(), duration_ms, success),
            );
        }
        self.dirty = true;
    }

    /// Check if there are unsaved changes
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Get the top N slowest commands by average duration
    pub fn top_slowest(&self, n: usize) -> Vec<&CommandStats> {
        let mut sorted: Vec<_> = self.stats.values().collect();
        sorted.sort_by(|a, b| b.average_duration_ms().cmp(&a.average_duration_ms()));
        sorted.into_iter().take(n).collect()
    }

    /// Get the top N most frequently called commands
    pub fn top_frequent(&self, n: usize) -> Vec<&CommandStats> {
        let mut sorted: Vec<_> = self.stats.values().collect();
        sorted.sort_by(|a, b| b.total_calls.cmp(&a.total_calls));
        sorted.into_iter().take(n).collect()
    }

    /// Get commands that failed recently (within the last N hours)
    pub fn recent_failures(&self, hours: i64) -> Vec<&CommandStats> {
        let cutoff = Utc::now() - Duration::hours(hours);
        self.stats
            .values()
            .filter(|s| s.failures > 0 && s.last_executed > cutoff)
            .collect()
    }

    /// Get statistics for a specific command
    pub fn get(&self, command: &str) -> Option<&CommandStats> {
        self.stats.get(command)
    }

    /// Clear all statistics
    pub fn clear(&mut self) {
        self.stats.clear();
        self.collection_started = Some(Utc::now());
        self.dirty = true;
    }

    /// Purge old entries (older than N days)
    pub fn purge_old_entries(&mut self, days: i64) {
        let cutoff = Utc::now() - Duration::days(days);
        let before = self.stats.len();
        self.stats.retain(|_, v| v.last_executed > cutoff);
        if self.stats.len() != before {
            self.dirty = true;
        }
    }
}

/// Extract the base command name from a full command line
pub fn extract_command_name(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Split by whitespace and get the first token
    let first_token = trimmed.split_whitespace().next()?;

    // Handle paths (e.g., /usr/bin/ls -> ls, ./script.sh -> script.sh)
    let command = first_token.rsplit('/').next().unwrap_or(first_token);

    // Ignore empty commands or commands starting with special characters
    if command.is_empty() || command.starts_with('!') || command.starts_with('?') {
        return None;
    }

    Some(command.to_string())
}

/// Get the path to the timing data file
pub fn get_timing_file_path() -> Option<PathBuf> {
    let xdg_dir = xdg::BaseDirectories::with_prefix("dsh").ok()?;
    xdg_dir.place_data_file("timing.json").ok()
}

/// Global shared timing instance for the shell
pub type SharedCommandTiming = Arc<RwLock<CommandTiming>>;

/// Create or load the shared timing instance
pub fn create_shared_timing() -> SharedCommandTiming {
    let timing = if let Some(path) = get_timing_file_path() {
        CommandTiming::load_from_file(&path).unwrap_or_default()
    } else {
        CommandTiming::new()
    };
    Arc::new(RwLock::new(timing))
}

/// Format duration in human-readable form
pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 3_600_000 {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) / 1000;
        format!("{}m {}s", mins, secs)
    } else {
        let hours = ms / 3_600_000;
        let mins = (ms % 3_600_000) / 60_000;
        format!("{}h {}m", hours, mins)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_stats_new() {
        let stats = CommandStats::new("git".to_string(), 100, true);
        assert_eq!(stats.command, "git");
        assert_eq!(stats.total_calls, 1);
        assert_eq!(stats.total_duration_ms, 100);
        assert_eq!(stats.failures, 0);
    }

    #[test]
    fn test_command_stats_record() {
        let mut stats = CommandStats::new("git".to_string(), 100, true);
        stats.record(200, false);

        assert_eq!(stats.total_calls, 2);
        assert_eq!(stats.total_duration_ms, 300);
        assert_eq!(stats.max_duration_ms, 200);
        assert_eq!(stats.min_duration_ms, 100);
        assert_eq!(stats.failures, 1);
    }

    #[test]
    fn test_average_duration() {
        let mut stats = CommandStats::new("git".to_string(), 100, true);
        stats.record(300, true);

        assert_eq!(stats.average_duration_ms(), 200);
    }

    #[test]
    fn test_success_rate() {
        let mut stats = CommandStats::new("git".to_string(), 100, true);
        stats.record(100, true);
        stats.record(100, false);
        stats.record(100, true);

        assert_eq!(stats.success_rate(), 75.0);
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(50), "50ms");
        assert_eq!(format_duration(1500), "1.5s");
        assert_eq!(format_duration(65000), "1m 5s");
        assert_eq!(format_duration(3_665_000), "1h 1m");
    }

    #[test]
    fn test_extract_command_name() {
        assert_eq!(extract_command_name("git status"), Some("git".to_string()));
        assert_eq!(
            extract_command_name("/usr/bin/ls -la"),
            Some("ls".to_string())
        );
        assert_eq!(
            extract_command_name("./script.sh"),
            Some("script.sh".to_string())
        );
        assert_eq!(
            extract_command_name("  cargo build  "),
            Some("cargo".to_string())
        );
        assert_eq!(extract_command_name(""), None);
        assert_eq!(extract_command_name("!help"), None);
        assert_eq!(extract_command_name("??generate"), None);
    }

    #[test]
    fn test_timing_record() {
        let mut timing = CommandTiming::new();
        timing.record("git", 0, StdDuration::from_millis(100));
        timing.record("git", 0, StdDuration::from_millis(200));
        timing.record("cargo", 1, StdDuration::from_millis(5000));

        assert_eq!(timing.stats.len(), 2);
        assert_eq!(timing.get("git").unwrap().total_calls, 2);
        assert_eq!(timing.get("cargo").unwrap().failures, 1);
        assert!(timing.is_dirty());
    }

    #[test]
    fn test_top_slowest() {
        let mut timing = CommandTiming::new();
        timing.record("fast", 0, StdDuration::from_millis(10));
        timing.record("slow", 0, StdDuration::from_millis(1000));
        timing.record("medium", 0, StdDuration::from_millis(100));

        let slowest = timing.top_slowest(2);
        assert_eq!(slowest.len(), 2);
        assert_eq!(slowest[0].command, "slow");
        assert_eq!(slowest[1].command, "medium");
    }

    #[test]
    fn test_top_frequent() {
        let mut timing = CommandTiming::new();
        timing.record("rare", 0, StdDuration::from_millis(100));
        timing.record("common", 0, StdDuration::from_millis(100));
        timing.record("common", 0, StdDuration::from_millis(100));
        timing.record("common", 0, StdDuration::from_millis(100));

        let frequent = timing.top_frequent(2);
        assert_eq!(frequent[0].command, "common");
        assert_eq!(frequent[0].total_calls, 3);
    }
}

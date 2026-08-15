//! Command timing builtin command
//!
//! Provides the `timing` builtin command for displaying command execution statistics.
//! This module reads timing data from the same JSON file used by the REPL.

use super::ShellProxy;
use chrono::{DateTime, Duration, Utc};
use dsh_types::{Context, ExitStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};
use tracing::{debug, warn};

#[derive(Default)]
struct TimingFileCoordinator {
    reset_epochs: HashMap<PathBuf, u64>,
}

static TIMING_FILE_COORDINATOR: LazyLock<Mutex<TimingFileCoordinator>> =
    LazyLock::new(|| Mutex::new(TimingFileCoordinator::default()));

fn timing_file_coordinator() -> MutexGuard<'static, TimingFileCoordinator> {
    TIMING_FILE_COORDINATOR
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Returns the in-process reset epoch for a timing file.
///
/// The REPL snapshots this value before a background write. `timing --clear`
/// increments it under the same mutex so an older snapshot cannot be published
/// after the clear completes.
pub fn timing_reset_epoch(path: &Path) -> u64 {
    timing_file_coordinator()
        .reset_epochs
        .get(path)
        .copied()
        .unwrap_or(0)
}

fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)?;
    temporary.as_file_mut().flush()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Atomically publishes `value` only if no in-process reset happened since the
/// caller captured `expected_reset_epoch`.
pub fn write_timing_json_if_epoch<T: Serialize>(
    path: &Path,
    value: &T,
    expected_reset_epoch: u64,
) -> std::io::Result<bool> {
    let coordinator = timing_file_coordinator();
    let current_epoch = coordinator.reset_epochs.get(path).copied().unwrap_or(0);
    if current_epoch != expected_reset_epoch {
        return Ok(false);
    }
    write_json_atomically(path, value)?;
    Ok(true)
}

fn write_timing_reset<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let mut coordinator = timing_file_coordinator();
    write_json_atomically(path, value)?;
    let epoch = coordinator
        .reset_epochs
        .entry(path.to_path_buf())
        .or_default();
    *epoch = epoch.wrapping_add(1);
    Ok(())
}

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
    /// Calculate average execution time in milliseconds
    pub fn average_duration_ms(&self) -> u64 {
        self.total_duration_ms
            .checked_div(self.total_calls)
            .unwrap_or(0)
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
}

impl CommandTiming {
    /// Create a new empty CommandTiming instance
    pub fn new() -> Self {
        Self {
            stats: HashMap::new(),
            collection_started: Some(Utc::now()),
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
    pub fn save_to_file(&self, path: &PathBuf) -> std::io::Result<()> {
        write_timing_reset(path, self)?;
        debug!("Saved command timing to {:?}", path);
        Ok(())
    }

    /// Clear all statistics
    pub fn clear(&mut self) {
        self.stats.clear();
        self.collection_started = Some(Utc::now());
    }

    /// Get the top N slowest commands by average duration
    pub fn top_slowest(&self, n: usize) -> Vec<&CommandStats> {
        let mut sorted: Vec<_> = self.stats.values().collect();
        sorted.sort_by_key(|b| std::cmp::Reverse(b.average_duration_ms()));
        sorted.into_iter().take(n).collect()
    }

    /// Get the top N most frequently called commands
    pub fn top_frequent(&self, n: usize) -> Vec<&CommandStats> {
        let mut sorted: Vec<_> = self.stats.values().collect();
        sorted.sort_by_key(|stat| std::cmp::Reverse(stat.total_calls));
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
}

/// Get the path to the timing data file
pub fn get_timing_file_path() -> Option<PathBuf> {
    let xdg_dir = xdg::BaseDirectories::with_prefix("dsh");
    xdg_dir.place_data_file("timing.json").ok()
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

/// Built-in timing command description
pub fn description() -> &'static str {
    "Show command execution statistics (timing, frequency, failures)"
}

/// Built-in timing command implementation
///
/// Usage:
///   timing                - Show summary of all command statistics
///   timing <command>      - Show statistics for a specific command
///   timing --slow         - Show top 10 slowest commands
///   timing --frequent     - Show top 10 most frequent commands
///   timing --failures     - Show recently failed commands
///   timing --clear        - Clear all timing statistics
pub fn command(ctx: &Context, argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Load existing timing data
    let timing_file = match get_timing_file_path() {
        Some(path) => path,
        None => {
            eprintln!("Error: Could not determine timing file path");
            return ExitStatus::ExitedWith(1);
        }
    };

    let mut timing = CommandTiming::load_from_file(&timing_file).unwrap_or_default();

    // Parse arguments
    let json_output = argv.iter().any(|arg| arg == "--json");
    let args: Vec<&str> = argv
        .iter()
        .skip(1)
        .map(|s| s.as_str())
        .filter(|arg| *arg != "--json")
        .collect();

    if json_output {
        let value = match args.first().copied() {
            Some("--slow") => serde_json::json!({"mode": "slow", "stats": timing.top_slowest(10)}),
            Some("--frequent") => {
                serde_json::json!({"mode": "frequent", "stats": timing.top_frequent(10)})
            }
            Some("--failures") => {
                serde_json::json!({"mode": "failures", "stats": timing.recent_failures(24)})
            }
            Some("--clear") => {
                let _ = ctx.write_stderr("timing: --clear cannot be combined with --json");
                return ExitStatus::ExitedWith(1);
            }
            Some(command) => serde_json::json!({"mode": "command", "stats": timing.get(command)}),
            None => serde_json::json!({"mode": "summary", "timing": timing}),
        };
        match serde_json::to_string(&value) {
            Ok(output) => {
                let _ = ctx.write_stdout(&output);
                return ExitStatus::ExitedWith(0);
            }
            Err(err) => {
                let _ = ctx.write_stderr(&format!("timing: JSON serialization failed: {err}"));
                return ExitStatus::ExitedWith(1);
            }
        }
    }

    match args.first() {
        None => {
            // Show summary
            print_summary(&timing);
        }
        Some(&"--slow") => {
            print_slowest(&timing);
        }
        Some(&"--frequent") => {
            print_frequent(&timing);
        }
        Some(&"--failures") => {
            print_failures(&timing);
        }
        Some(&"--clear") => {
            timing.clear();
            if let Err(e) = timing.save_to_file(&timing_file) {
                eprintln!("Error saving timing data: {}", e);
                return ExitStatus::ExitedWith(1);
            }
            println!("Command timing statistics cleared.");
        }
        Some(&"--help") | Some(&"-h") => {
            print_help();
        }
        Some(cmd) => {
            // Show statistics for a specific command
            print_command_stats(&timing, cmd);
        }
    }

    ExitStatus::ExitedWith(0)
}

fn print_summary(timing: &CommandTiming) {
    if timing.stats.is_empty() {
        println!("No command timing data collected yet.");
        println!("Execute some commands to start collecting statistics.");
        return;
    }

    println!();
    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║           Command Execution Statistics                           ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();

    if let Some(started) = timing.collection_started {
        let duration = Utc::now().signed_duration_since(started);
        let days = duration.num_days();
        println!("  Collection period: {} days", days.max(1));
    }

    let total_commands = timing.stats.len();
    let total_calls: u64 = timing.stats.values().map(|s| s.total_calls).sum();
    let total_failures: u64 = timing.stats.values().map(|s| s.failures).sum();

    println!("  Unique commands tracked: {}", total_commands);
    println!("  Total executions: {}", total_calls);
    println!(
        "  Overall success rate: {:.1}%",
        if total_calls > 0 {
            ((total_calls - total_failures) as f64 / total_calls as f64) * 100.0
        } else {
            100.0
        }
    );
    println!();

    // Show top 5 slowest
    println!("  ── Top 5 Slowest Commands ──────────────────────────────────────");
    for (i, stats) in timing.top_slowest(5).iter().enumerate() {
        println!(
            "  {}. {:20} avg: {:>10}  max: {:>10}  calls: {}",
            i + 1,
            stats.command,
            format_duration(stats.average_duration_ms()),
            format_duration(stats.max_duration_ms),
            stats.total_calls
        );
    }
    println!();

    // Show top 5 most frequent
    println!("  ── Top 5 Most Frequent Commands ────────────────────────────────");
    for (i, stats) in timing.top_frequent(5).iter().enumerate() {
        println!(
            "  {}. {:20} calls: {:>6}  avg: {:>10}",
            i + 1,
            stats.command,
            stats.total_calls,
            format_duration(stats.average_duration_ms())
        );
    }
    println!();

    // Show recent failures if any
    let failures = timing.recent_failures(24);
    if !failures.is_empty() {
        println!("  ── Recent Failures (last 24 hours) ─────────────────────────────");
        for stats in failures.iter().take(5) {
            println!(
                "     {:20} {} failures (success rate: {:.1}%)",
                stats.command,
                stats.failures,
                stats.success_rate()
            );
        }
        println!();
    }
}

fn print_slowest(timing: &CommandTiming) {
    println!();
    println!("Top 10 Slowest Commands:");
    println!("─────────────────────────────────────────────────────────────────────");
    for (i, stats) in timing.top_slowest(10).iter().enumerate() {
        println!(
            "  {}. {:25} avg: {:>10}  max: {:>10}  calls: {}",
            i + 1,
            stats.command,
            format_duration(stats.average_duration_ms()),
            format_duration(stats.max_duration_ms),
            stats.total_calls
        );
    }
    println!();
}

fn print_frequent(timing: &CommandTiming) {
    println!();
    println!("Top 10 Most Frequent Commands:");
    println!("─────────────────────────────────────────────────────────────────────");
    for (i, stats) in timing.top_frequent(10).iter().enumerate() {
        println!(
            "  {}. {:25} calls: {:>6}  avg: {:>10}  success: {:.1}%",
            i + 1,
            stats.command,
            stats.total_calls,
            format_duration(stats.average_duration_ms()),
            stats.success_rate()
        );
    }
    println!();
}

fn print_failures(timing: &CommandTiming) {
    println!();
    println!("Recently Failed Commands (last 24 hours):");
    println!("─────────────────────────────────────────────────────────────────────");
    let failures = timing.recent_failures(24);
    if failures.is_empty() {
        println!("  No failed commands in the last 24 hours. 🎉");
    } else {
        for stats in failures {
            println!(
                "  {:25} {} failures out of {} calls (success: {:.1}%)",
                stats.command,
                stats.failures,
                stats.total_calls,
                stats.success_rate()
            );
        }
    }
    println!();
}

fn print_command_stats(timing: &CommandTiming, cmd: &str) {
    match timing.get(cmd) {
        Some(stats) => {
            println!();
            println!("Statistics for '{}':", cmd);
            println!("─────────────────────────────────────────────────────────────────────");
            println!("  Total calls:       {}", stats.total_calls);
            println!(
                "  Average duration:  {}",
                format_duration(stats.average_duration_ms())
            );
            println!(
                "  Minimum duration:  {}",
                format_duration(stats.min_duration_ms)
            );
            println!(
                "  Maximum duration:  {}",
                format_duration(stats.max_duration_ms)
            );
            println!(
                "  Total time spent:  {}",
                format_duration(stats.total_duration_ms)
            );
            println!(
                "  Successful calls:  {}",
                stats.total_calls - stats.failures
            );
            println!("  Failed calls:      {}", stats.failures);
            println!("  Success rate:      {:.1}%", stats.success_rate());
            println!(
                "  Last executed:     {}",
                stats.last_executed.format("%Y-%m-%d %H:%M:%S UTC")
            );
            println!();
        }
        None => {
            println!("No statistics found for command '{}'.", cmd);
            println!("Execute the command to start collecting statistics.");
        }
    }
}

fn print_help() {
    println!("Usage: timing [OPTIONS] [COMMAND]");
    println!();
    println!("Show command execution statistics.");
    println!();
    println!("Options:");
    println!("  --slow       Show top 10 slowest commands by average execution time");
    println!("  --frequent   Show top 10 most frequently executed commands");
    println!("  --failures   Show commands that failed in the last 24 hours");
    println!("  --clear      Clear all timing statistics");
    println!("  -h, --help   Show this help message");
    println!();
    println!("Examples:");
    println!("  timing              Show summary of all statistics");
    println!("  timing git          Show statistics for 'git' command");
    println!("  timing --slow       Show slowest commands");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(50), "50ms");
        assert_eq!(format_duration(1500), "1.5s");
        assert_eq!(format_duration(65000), "1m 5s");
        assert_eq!(format_duration(3_665_000), "1h 1m");
    }

    #[test]
    fn test_timing_file_path() {
        let path = get_timing_file_path();
        assert!(path.is_some());
        let path = path.expect("path should be some");
        assert!(path.to_string_lossy().contains("timing.json"));
    }

    #[test]
    fn save_publishes_valid_json_and_advances_the_reset_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timing.json");
        let before = timing_reset_epoch(&path);

        CommandTiming::new().save_to_file(&path).unwrap();

        assert_ne!(timing_reset_epoch(&path), before);
        assert!(CommandTiming::load_from_file(&path).is_some());
    }
}

//! Session-local command block history.
//!
//! Command blocks are richer execution records than `$OUT` history. They keep
//! metadata for commands even when no output was captured.

use crate::output_history::OutputEntry;
use std::collections::VecDeque;
use std::time::SystemTime;

const DEFAULT_MAX_BLOCKS: usize = 100;
const DEFAULT_MAX_OUTPUT_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiWatchSummary {
    pub goal: Option<String>,
    pub status: String,
    pub notes: Vec<String>,
    pub suggested_commands: Vec<String>,
    pub raw_response: Option<String>,
}

impl AiWatchSummary {
    pub fn new(goal: Option<String>, status: String, response: String) -> Self {
        Self {
            goal,
            status,
            notes: response
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .take(8)
                .map(ToOwned::to_owned)
                .collect(),
            suggested_commands: extract_suggested_commands(&response),
            raw_response: Some(response),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommandBlock {
    pub id: u64,
    pub command: String,
    pub cwd: Option<String>,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timestamp: SystemTime,
    pub duration_ms: u64,
    pub output_entry_ids: Vec<u64>,
    pub watched: bool,
    pub watch_summary: Option<AiWatchSummary>,
}

impl CommandBlock {
    pub fn new(
        command: String,
        cwd: Option<String>,
        exit_code: i32,
        duration_ms: u64,
        output_entries: &[OutputEntry],
        watch_summary: Option<AiWatchSummary>,
    ) -> Self {
        let output_entry_ids = output_entries.iter().map(|entry| entry.id).collect();
        let stdout = join_outputs(output_entries, |entry| entry.stdout.as_str());
        let stderr = join_outputs(output_entries, |entry| entry.stderr.as_str());

        Self {
            id: 0,
            command,
            cwd,
            stdout,
            stderr,
            exit_code,
            timestamp: SystemTime::now(),
            duration_ms,
            output_entry_ids,
            watched: watch_summary.is_some(),
            watch_summary,
        }
    }

    pub fn output_preview(&self, max_chars: usize) -> String {
        let output = if self.stdout.is_empty() {
            self.stderr.as_str()
        } else {
            self.stdout.as_str()
        };
        preview_line(output, max_chars)
    }

    pub fn truncate_outputs(&mut self, max_size: usize) {
        truncate_pair(&mut self.stdout, &mut self.stderr, max_size);
    }
}

#[derive(Debug)]
pub struct CommandBlockHistory {
    blocks: VecDeque<CommandBlock>,
    max_blocks: usize,
    max_output_size: usize,
    next_id: u64,
}

impl Default for CommandBlockHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandBlockHistory {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_MAX_BLOCKS, DEFAULT_MAX_OUTPUT_SIZE)
    }

    pub fn with_capacity(max_blocks: usize, max_output_size: usize) -> Self {
        Self {
            blocks: VecDeque::with_capacity(max_blocks),
            max_blocks,
            max_output_size,
            next_id: 1,
        }
    }

    pub fn push(&mut self, mut block: CommandBlock) {
        if block.id == 0 {
            block.id = self.next_id;
            self.next_id = self.next_id.saturating_add(1);
        } else {
            self.next_id = self.next_id.max(block.id.saturating_add(1));
        }
        block.truncate_outputs(self.max_output_size);

        while self.blocks.len() >= self.max_blocks {
            self.blocks.pop_back();
        }
        self.blocks.push_front(block);
    }

    pub fn get(&self, index: usize) -> Option<&CommandBlock> {
        if index == 0 {
            return None;
        }
        self.blocks.get(index - 1)
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = &CommandBlock> {
        self.blocks.iter()
    }

    pub fn get_all_blocks(&self) -> Vec<CommandBlock> {
        self.blocks.iter().cloned().collect()
    }
}

fn join_outputs<F>(entries: &[OutputEntry], select: F) -> String
where
    F: Fn(&OutputEntry) -> &str,
{
    entries
        .iter()
        .map(select)
        .filter(|output| !output.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn preview_line(output: &str, max_chars: usize) -> String {
    let first = output.lines().next().unwrap_or("").trim();
    let mut preview = String::new();
    for ch in first.chars().take(max_chars) {
        preview.push(ch);
    }
    if first.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}

fn truncate_pair(stdout: &mut String, stderr: &mut String, max_size: usize) {
    let total = stdout.len() + stderr.len();
    if total <= max_size {
        return;
    }
    if total == 0 {
        return;
    }

    let stdout_ratio = stdout.len() as f64 / total as f64;
    let mut stdout_max = (max_size as f64 * stdout_ratio) as usize;
    while !stdout.is_char_boundary(stdout_max) {
        stdout_max = stdout_max.saturating_sub(1);
    }

    let mut stderr_max = max_size.saturating_sub(stdout_max);
    while !stderr.is_char_boundary(stderr_max) {
        stderr_max = stderr_max.saturating_sub(1);
    }

    if stdout.len() > stdout_max {
        stdout.truncate(stdout_max);
        stdout.push_str("\n... (truncated)");
    }
    if stderr.len() > stderr_max {
        stderr.truncate(stderr_max);
        stderr.push_str("\n... (truncated)");
    }
}

fn extract_suggested_commands(response: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut in_bash_block = false;

    for line in response.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_bash_block = trimmed.starts_with("```bash") || trimmed.starts_with("```sh");
            continue;
        }
        if in_bash_block && !trimmed.is_empty() && !trimmed.starts_with('#') {
            commands.push(trimmed.to_string());
            if commands.len() >= 3 {
                break;
            }
        }
    }

    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_history_assigns_ids_and_keeps_recent_first() {
        let mut history = CommandBlockHistory::with_capacity(2, 1024);
        history.push(CommandBlock::new("one".into(), None, 0, 10, &[], None));
        history.push(CommandBlock::new("two".into(), None, 1, 20, &[], None));
        history.push(CommandBlock::new("three".into(), None, 0, 30, &[], None));

        assert_eq!(history.len(), 2);
        assert_eq!(history.get(1).map(|block| block.id), Some(3));
        assert_eq!(
            history.get(1).map(|block| block.command.as_str()),
            Some("three")
        );
        assert_eq!(
            history.get(2).map(|block| block.command.as_str()),
            Some("two")
        );
    }

    #[test]
    fn block_collects_output_entry_ids_and_output() {
        let mut first = OutputEntry::new("echo one".into(), "one".into(), "".into(), 0);
        first.id = 10;
        let mut second = OutputEntry::new("echo two".into(), "two".into(), "warn".into(), 0);
        second.id = 11;

        let block = CommandBlock::new(
            "echo one; echo two".into(),
            Some("/tmp".into()),
            0,
            5,
            &[first, second],
            None,
        );

        assert_eq!(block.output_entry_ids, vec![10, 11]);
        assert_eq!(block.stdout, "one\ntwo");
        assert_eq!(block.stderr, "warn");
        assert_eq!(block.output_preview(20), "one");
    }

    #[test]
    fn watch_summary_extracts_bash_commands() {
        let summary = AiWatchSummary::new(
            Some("fix".into()),
            "failed".into(),
            "Try:\n```bash\ncargo test -p doge-shell\ncargo check\n```".into(),
        );

        assert_eq!(
            summary.suggested_commands,
            vec!["cargo test -p doge-shell", "cargo check"]
        );
    }
}

use crate::completion::display::Candidate;
use anyhow::Result;
use chrono::Timelike;
use dsh_frecency::{FrecencyStore, SortMethod};
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use tracing::debug;

// Pre-compiled regex for efficient whitespace splitting
static WHITESPACE_SPLIT_REGEX: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// History-based completion using frecency algorithm
#[allow(dead_code)]
pub struct HistoryCompletion {
    store: Option<FrecencyStore>,
    command_patterns: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CompletionContext {
    pub current_dir: String,
    pub previous_commands: Vec<String>,
    pub time_of_day: u8, // 0-23
}

#[allow(dead_code)]
impl HistoryCompletion {
    pub fn new() -> Self {
        Self {
            store: None,
            command_patterns: HashMap::new(),
        }
    }

    pub fn with_store(store: FrecencyStore) -> Self {
        Self {
            store: Some(store),
            command_patterns: HashMap::new(),
        }
    }

    /// Initialize with history data
    pub fn load_history(&mut self, history_path: &Path) -> Result<()> {
        let path_buf = history_path.to_path_buf();
        if let Ok(store) = dsh_frecency::read_store(&path_buf) {
            self.store = Some(store);
            self.build_command_patterns();
        }
        Ok(())
    }

    /// Build command patterns from history
    fn build_command_patterns(&mut self) {
        if let Some(ref store) = self.store {
            let items = &store.items;

            for item in items {
                let parts: Vec<&str> = WHITESPACE_SPLIT_REGEX.split(&item.item).collect();
                if let Some(command) = parts.first() {
                    let entry = self
                        .command_patterns
                        .entry(command.to_string())
                        .or_default();

                    if parts.len() > 1 {
                        let args = parts[1..].join(" ");
                        if !entry.contains(&args) {
                            entry.push(args);
                        }
                    }
                }
            }
        }
    }

    /// Get completion suggestions based on history
    pub fn suggest(&self, prefix: &str, context: &CompletionContext) -> Vec<Candidate> {
        let mut suggestions = Vec::new();

        if let Some(ref store) = self.store {
            // Get frecency-based suggestions by filtering items
            let frecency_items: Vec<_> = store
                .items
                .iter()
                .filter(|item| item.item.starts_with(prefix))
                .take(20)
                .collect();

            for item in frecency_items {
                // Use public method to get frecency
                let mut score = item.get_frecency() as u32;
                if item.item.contains(&context.current_dir) {
                    score = (score as f64 * 1.5) as u32;
                }

                suggestions.push(Candidate::History {
                    command: item.item.clone(),
                    frequency: score,
                    last_used: item.secs_since_access() as i64,
                });
            }
        }

        // Add pattern-based suggestions
        if let Some(patterns) = self.command_patterns.get(prefix) {
            for pattern in patterns {
                let full_command = format!("{prefix} {pattern}");
                suggestions.push(Candidate::History {
                    command: full_command,
                    frequency: 1,
                    last_used: 0,
                });
            }
        }

        // Sort by frequency and recency
        suggestions.sort_by(|a, b| {
            if let (
                Candidate::History {
                    frequency: freq_a,
                    last_used: time_a,
                    ..
                },
                Candidate::History {
                    frequency: freq_b,
                    last_used: time_b,
                    ..
                },
            ) = (a, b)
            {
                // Combine frequency and recency scores
                let score_a = (*freq_a as f64) + ((*time_a as f64) / 1000000.0);
                let score_b = (*freq_b as f64) + ((*time_b as f64) / 1000000.0);
                score_b
                    .partial_cmp(&score_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
            } else {
                std::cmp::Ordering::Equal
            }
        });

        suggestions.truncate(10); // Limit to top 10 suggestions
        suggestions
    }

    /// Get command completion based on partial input
    pub fn complete_command(&self, partial: &str, context: &CompletionContext) -> Vec<Candidate> {
        debug!(
            "History completion for: {} in {}",
            partial, context.current_dir
        );

        if partial.is_empty() {
            return self.get_recent_commands(context);
        }

        self.suggest(partial, context)
    }

    /// Get recently used commands
    fn get_recent_commands(&self, _context: &CompletionContext) -> Vec<Candidate> {
        if let Some(ref store) = self.store {
            let recent_items = store.sorted(&SortMethod::Recent);

            recent_items
                .into_iter()
                .take(10)
                .map(|item| Candidate::History {
                    command: item.item.clone(),
                    frequency: item.get_frecency() as u32,
                    last_used: item.secs_since_access() as i64,
                })
                .collect()
        } else {
            vec![]
        }
    }

    /// Update history with new command
    pub fn update_history(&mut self, command: &str, _context: &CompletionContext) -> Result<()> {
        if let Some(ref mut store) = self.store {
            // Use the store's update method if available, otherwise just add to items
            // For now, we'll create a simple implementation
            let mut found = false;
            for item in &mut store.items {
                if item.item == command {
                    // Update existing item
                    item.update_frecency(1.0);
                    item.update_num_accesses(1);
                    item.update_last_access(dsh_frecency::current_time_secs());
                    found = true;
                    break;
                }
            }

            if !found {
                // Add new item
                let mut new_item =
                    dsh_frecency::ItemStats::new(command, store.reference_time, store.half_life);
                new_item.update_frecency(1.0);
                new_item.update_num_accesses(1);
                new_item.update_last_access(dsh_frecency::current_time_secs());
                store.items.push(new_item);
            }

            self.build_command_patterns(); // Rebuild patterns
        }
        Ok(())
    }

    /// Get command suggestions for a specific directory
    pub fn get_directory_suggestions(&self, dir: &str, limit: usize) -> Vec<Candidate> {
        if let Some(ref store) = self.store {
            let mut dir_items: Vec<_> = store
                .items
                .iter()
                .filter(|item| item.item.contains(dir))
                .collect();

            // Sort by frecency using public method
            dir_items.sort_by(|a, b| {
                b.get_frecency()
                    .partial_cmp(&a.get_frecency())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            dir_items.truncate(limit);

            dir_items
                .into_iter()
                .map(|item| Candidate::History {
                    command: item.item.clone(),
                    frequency: item.get_frecency() as u32,
                    last_used: item.secs_since_access() as i64,
                })
                .collect()
        } else {
            vec![]
        }
    }
}

impl Default for HistoryCompletion {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl CompletionContext {
    pub fn new(current_dir: String) -> Self {
        Self {
            current_dir,
            previous_commands: vec![],
            time_of_day: chrono::Local::now().hour() as u8,
        }
    }

    pub fn with_previous_commands(mut self, commands: Vec<String>) -> Self {
        self.previous_commands = commands;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_history_completion_creation() {
        let completion = HistoryCompletion::new();
        assert!(completion.store.is_none());
        assert!(completion.command_patterns.is_empty());
    }

    #[test]
    fn test_completion_context() {
        let context = CompletionContext::new("/home/user".to_string());
        assert_eq!(context.current_dir, "/home/user");
        assert!(context.previous_commands.is_empty());
        assert!(context.time_of_day < 24);
    }

    #[test]
    fn test_empty_suggestions() {
        let completion = HistoryCompletion::new();
        let context = CompletionContext::new("/tmp".to_string());
        let suggestions = completion.suggest("test", &context);
        assert!(suggestions.is_empty());
    }
}

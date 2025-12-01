use crate::completion::display::Candidate;
use dsh_frecency::{FrecencyStore, SortMethod};
use std::collections::HashMap;
use tracing::debug;

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
}

impl HistoryCompletion {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            store: None,
            command_patterns: HashMap::new(),
        }
    }

    /// Get completion suggestions based on history
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
}

impl Default for HistoryCompletion {
    fn default() -> Self {
        Self::new()
    }
}

impl CompletionContext {
    #[allow(dead_code)]
    pub fn new(current_dir: String) -> Self {
        Self { current_dir }
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
    }

    #[test]
    fn test_empty_suggestions() {
        let completion = HistoryCompletion::new();
        let context = CompletionContext::new("/tmp".to_string());
        let suggestions = completion.suggest("test", &context);
        assert!(suggestions.is_empty());
    }
}

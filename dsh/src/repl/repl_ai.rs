use super::AiEvent;
use super::Repl;

use crate::completion::shell_token::{self, SeparatorMode};
use std::sync::Arc;
use std::time::Instant;

const AUTO_FIX_BLOCKLIST: &[&str] = &["gco"];

pub fn get_directory_listing_content(path: &std::path::Path) -> Vec<String> {
    let mut files = Vec::new();
    if let Ok(dir) = std::fs::read_dir(path) {
        let mut entries: Vec<_> = dir
            .filter_map(|e| e.ok())
            .map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                (name, is_dir)
            })
            .filter(|(name, _)| !name.starts_with('.'))
            .collect();

        entries.sort_by(|a, b| match (a.1, b.1) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.0.cmp(&b.0),
        });

        files = entries
            .into_iter()
            .take(30)
            .map(|(name, is_dir)| if is_dir { format!("{}/", name) } else { name })
            .collect();
    }
    files
}

impl<'a> Repl<'a> {
    pub(crate) fn trigger_auto_fix(&self) {
        if self.last_status != 0
            && !self.last_command_string.is_empty()
            && self.input_preferences.auto_fix
            && let Some(service) = &self.ai_service
        {
            if is_auto_fix_blocked(&self.last_command_string) {
                return;
            }
            let service = service.clone();
            let command = self.last_command_string.clone();
            let status = self.last_status;
            let output = self
                .shell
                .environment
                .read()
                .get_var("OUT")
                .unwrap_or_default();
            let tx = self.ai_tx.clone();

            tokio::spawn(async move {
                if let Ok(fixed) =
                    crate::ai_features::fix_command(service.as_ref(), &command, status, &output)
                        .await
                {
                    let _ = tx.send(AiEvent::AutoFix(fixed));
                }
            });
        }
    }

    pub(crate) fn refresh_inline_suggestion(&mut self) -> bool {
        if self.input.completion.is_some() {
            let had_suggestion = !self.suggestion_manager.candidates.is_empty();
            self.suggestion_manager.clear();
            return had_suggestion;
        }

        self.sync_input_preferences();
        let history_ref = self.shell.cmd_history.as_ref();
        let current_input = self.input.to_string();
        let cursor_pos = self.input.cursor();

        // Check history first in predict() now handles this more strictly,
        // but it still needs to return states.
        let mut candidates =
            self.suggestion_manager
                .engine
                .predict(current_input.as_str(), cursor_pos, history_ref);

        // If no candidates from history/cache, try AI with full context
        if candidates.is_empty() && self.input_preferences.ai_backfill {
            let (cwd, files) = {
                // Try to use cache or empty
                self.trigger_file_context_update();
                let cache = self.file_context_cache.read();
                (
                    Some(cache.path.to_string_lossy().to_string()),
                    cache.files.clone(),
                )
            };

            if let Some(state) = self.suggestion_manager.engine.ai_suggestion_with_context(
                &current_input,
                cursor_pos,
                history_ref,
                cwd,
                files,
                Some(self.last_status),
            ) {
                candidates.push(state);
            }
        }

        if let Some(extra) = super::completion::completion_suggestion(
            &self.input,
            current_input.as_str(),
            &self.shell.environment,
        ) {
            let duplicate = candidates
                .iter()
                .any(|state| state.full == extra.full && state.source == extra.source);
            if !duplicate {
                candidates.push(extra);
            }
        }

        self.suggestion_manager.update_candidates(candidates);
        self.suggestion_manager.active.is_some()
    }

    pub(crate) async fn force_ai_suggestion(&mut self) -> bool {
        self.completion.clear();
        self.suggestion_manager.clear();

        self.sync_input_preferences();
        let history_ref = self.shell.cmd_history.as_ref();
        let current_input = self.input.to_string();
        let cursor_pos = self.input.cursor();

        // For forced suggestion, we can trigger update and wait a bit or just use cache
        // But since we are allowed to await here, we can actually wait for the result
        // or just use spawn_blocking locally if we want fresh results.
        // For consistency, let's update cache synchronously-ish (blocking local thread is fine as it's async task)
        // actually `force_ai_suggestion` loop waits for AI.

        let (cwd, files) = {
            let cache = self.file_context_cache.clone();
            let (cwd, files) = tokio::task::spawn_blocking(move || {
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                // Reuse the logic? Or just force update cache?
                let files = get_directory_listing_content(&cwd);

                // Update cache while we are at it
                {
                    let mut w = cache.write();
                    w.path = cwd.clone();
                    w.files = Arc::new(files.clone());
                    w.updated_at = Some(Instant::now());
                }
                (Some(cwd.to_string_lossy().to_string()), Arc::new(files))
            })
            .await
            .unwrap_or_default();
            (cwd, files)
        };

        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(15);

        tracing::debug!("force_ai_suggestion: waiting for response...");
        loop {
            if let Some(state) = self.suggestion_manager.engine.ai_suggestion_with_context(
                &current_input,
                cursor_pos,
                history_ref,
                cwd.clone(),
                files.clone(),
                Some(self.last_status),
            ) {
                tracing::debug!("force_ai_suggestion: got state {:?}", state);
                let candidates = vec![state];
                self.suggestion_manager.update_candidates(candidates);
                return true;
            }

            if start.elapsed() > timeout {
                tracing::warn!("force_ai_suggestion: timeout");
                break;
            }

            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        self.suggestion_manager.active.is_some()
    }
}

fn is_auto_fix_blocked(input: &str) -> bool {
    shell_token::tokenize(input, SeparatorMode::Parser)
        .into_iter()
        .next()
        .is_some_and(|command| AUTO_FIX_BLOCKLIST.contains(&command.raw.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn auto_fix_blocklist_uses_shell_command_token() {
        assert!(is_auto_fix_blocked("gco"));
        assert!(is_auto_fix_blocked(" gco"));
        assert!(is_auto_fix_blocked("gco\tmain"));

        assert!(!is_auto_fix_blocked(r#""gco" main"#));
        assert!(!is_auto_fix_blocked("cmd | gco"));
        assert!(!is_auto_fix_blocked(""));
        assert!(!is_auto_fix_blocked("   "));
    }

    #[test]
    fn test_get_directory_listing_content() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("a_file.txt");
        let dir_path = dir.path().join("b_dir");
        let hidden_path = dir.path().join(".hidden");

        File::create(&file_path).unwrap();
        std::fs::create_dir(&dir_path).unwrap();
        File::create(&hidden_path).unwrap();

        let listing = get_directory_listing_content(dir.path());

        // Expected: "b_dir/", "a_file.txt" (sorted: directories might come first based on sort logic)
        // Sort logic: (true, false) -> Less (dir < file). So dirs come first.
        // b_dir is dir, a_file is file. b_dir should be first.

        assert_eq!(listing.len(), 2);
        assert!(listing.contains(&"b_dir/".to_string()));
        assert!(listing.contains(&"a_file.txt".to_string()));
        assert_eq!(listing[0], "b_dir/");
    }
}

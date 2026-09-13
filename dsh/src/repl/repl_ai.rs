//! AI-adjacent `Repl` methods that don't belong in the main event loop:
//! failure auto-fix (deterministic and AI), inline ghost-text suggestions,
//! the `|?`/`|!`/`??` pipe patterns (detect + run), and `!sudo` toggling.
use super::Repl;
use super::input_analysis;
use super::terminal_state;
use super::{AI_PIPE_OUTPUT_CHARS, AiEvent, AutoFixKind, AutoFixSuggestion};

use crate::ai_features;
use crate::completion::shell_token::{self, SeparatorMode};
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType};
use dsh_builtin::execute_chat_message;
use dsh_types::Context;
use dsh_types::quick_fix::{DeterministicQuickFixProvider, QuickFixProvider};
use nix::unistd::getpid;
use std::sync::Arc;
use std::time::Instant;

const AUTO_FIX_BLOCKLIST: &[&str] = &["gco"];

/// Quiet period before the AI ghost-text backfill is allowed to fire.
const AI_BACKFILL_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);

pub fn get_directory_listing_content(path: &std::path::Path) -> Vec<String> {
    crate::ai_features::directory_listing_entries(path)
}

impl<'a> Repl<'a> {
    /// Automatic hint path, called after a failed command.
    ///
    /// The noise gate covers everything automatic, AI included: an interrupt
    /// or a `grep` miss is not a failure worth spending a request on. The
    /// manual Alt-f binding calls `trigger_auto_fix` directly and bypasses all
    /// of this.
    pub(crate) fn maybe_auto_fix_on_failure(&mut self) {
        if !self.ai_ui.input_preferences.failure_hint {
            return;
        }
        let command = self.state.last_command_string.clone();
        let status = self.state.last_status;
        if !super::failure_hint::should_offer_hint(
            &command,
            status,
            self.state.last_hinted_failure.as_ref(),
        ) {
            return;
        }
        self.state.last_hinted_failure = Some((command, status));
        self.trigger_auto_fix();
    }

    /// Point at the manual AI actions, when they are opted in and available.
    ///
    /// Replaces the line `execute_shell_command` used to print after every
    /// failure; it has to fire on every path that produces no applicable fix,
    /// including a blocked or failed AI request.
    fn send_diagnose_hint(&self, command_time: Option<Instant>) {
        if self.ai_ui.input_preferences.auto_diagnose && self.services.ai.is_some() {
            let _ = self.ai_ui.ai_tx.send(AiEvent::AutoFix(AutoFixSuggestion {
                replacement: String::new(),
                title: None,
                kind: AutoFixKind::DiagnoseHintOnly,
                command_time,
            }));
        }
    }

    pub(crate) fn trigger_auto_fix(&self) {
        if self.state.last_status == 0 || self.state.last_command_string.is_empty() {
            return;
        }

        let command = self.state.last_command_string.clone();
        let status = self.state.last_status;
        // Both streams, not just whichever one happened to be non-empty: a
        // command that logs progress on stdout and the error on stderr used to
        // reach the fixer with the progress log alone.
        let output = crate::ai_features::resolve_last_failure(
            &self.shell.environment.read(),
            Some((command.as_str(), status)),
        )
        .map(|failure| failure.output)
        .unwrap_or_default();

        let command_time = self.state.last_command_time;
        if let Some(fix) = DeterministicQuickFixProvider
            .suggest(&command, status, &output)
            .into_iter()
            .next()
        {
            let _ = self.ai_ui.ai_tx.send(AiEvent::AutoFix(AutoFixSuggestion {
                replacement: fix.replacement,
                title: Some(fix.title),
                kind: AutoFixKind::QuickFix,
                command_time,
            }));
            return;
        }

        if self.ai_ui.input_preferences.auto_fix
            && let Some(service) = &self.services.ai
            && !is_auto_fix_blocked(&command)
        {
            let service = service.clone();
            let tx = self.ai_ui.ai_tx.clone();
            let diagnose_hint = self.ai_ui.input_preferences.auto_diagnose;

            tokio::spawn(async move {
                match crate::ai_features::fix_command(service.as_ref(), &command, status, &output)
                    .await
                {
                    Ok(fixed) => {
                        let _ = tx.send(AiEvent::AutoFix(AutoFixSuggestion {
                            replacement: fixed,
                            title: None,
                            kind: AutoFixKind::AiFix,
                            command_time,
                        }));
                    }
                    // The request failed (offline, rate limited). Fall back to
                    // the manual-action hint rather than saying nothing.
                    Err(err) => {
                        tracing::debug!("auto fix request failed: {err}");
                        if diagnose_hint {
                            let _ = tx.send(AiEvent::AutoFix(AutoFixSuggestion {
                                replacement: String::new(),
                                title: None,
                                kind: AutoFixKind::DiagnoseHintOnly,
                                command_time,
                            }));
                        }
                    }
                }
            });
            return;
        }

        // No deterministic fix and no automatic AI fix on the way — including
        // a blocklisted command or auto-fix being off.
        self.send_diagnose_hint(command_time);
    }

    pub(crate) fn refresh_inline_suggestion(&mut self) -> bool {
        if self.input.completion.is_some() {
            let had_suggestion = !self.ai_ui.suggestion_manager.candidates.is_empty();
            self.ai_ui.suggestion_manager.clear();
            return had_suggestion;
        }

        self.sync_input_preferences();
        let history_ref = self.shell.cmd_history.as_ref();
        let current_input = self.input.to_string();
        let cursor_pos = self.input.cursor();

        // History stays first. Integrated JSON/dynamic ghost text should beat
        // generic path lookahead, and AI remains the final fallback.
        let mut candidates = self.ai_ui.suggestion_manager.engine.predict_history(
            current_input.as_str(),
            cursor_pos,
            history_ref,
        );

        if candidates.is_empty() {
            let current_dir = self.terminal_ui.prompt.read().current_path().to_path_buf();
            self.sync_completion_jobs();
            if let Some(full) = self.completion_ui.integrated_completion.ghost_completion(
                current_input.as_str(),
                cursor_pos,
                &current_dir,
                history_ref,
            ) {
                candidates.push(crate::suggestion::SuggestionState {
                    full,
                    source: crate::suggestion::SuggestionSource::Completion,
                });
            }
        }

        if candidates.is_empty()
            && let Some(extra) = super::completion::completion_suggestion(
                &self.input,
                current_input.as_str(),
                &self.shell.environment,
            )
        {
            candidates.push(extra);
        }

        // If no deterministic candidates are available, try AI with full context.
        //
        // Debounced: this runs on every keystroke, and the refresh tick calls it
        // again once typing stops, so waiting for a quiet moment costs no
        // suggestions but stops a request per character.
        if candidates.is_empty()
            && self.ai_ui.input_preferences.ai_backfill
            && self.ai_ui.last_input_change_time.elapsed() >= AI_BACKFILL_DEBOUNCE
        {
            let (cwd, files) = {
                self.trigger_file_context_update();
                let cache = self.services.file_context.read();
                (
                    Some(cache.path.to_string_lossy().to_string()),
                    cache.files.clone(),
                )
            };

            if let Some(state) = self
                .ai_ui
                .suggestion_manager
                .engine
                .ai_suggestion_with_context(
                    &current_input,
                    cursor_pos,
                    history_ref,
                    cwd,
                    files,
                    Some(self.state.last_status),
                )
            {
                candidates.push(state);
            }
        }

        self.ai_ui.suggestion_manager.update_candidates(candidates);
        self.ai_ui.suggestion_manager.active.is_some()
    }

    pub(crate) async fn force_ai_suggestion(&mut self) -> bool {
        self.completion_ui.completion.clear();
        self.ai_ui.suggestion_manager.clear();

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
            let cache = self.services.file_context.clone();
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
            if let Some(state) = self
                .ai_ui
                .suggestion_manager
                .engine
                .ai_suggestion_with_context(
                    &current_input,
                    cursor_pos,
                    history_ref,
                    cwd.clone(),
                    files.clone(),
                    Some(self.state.last_status),
                )
            {
                tracing::debug!("force_ai_suggestion: got state {:?}", state);
                let candidates = vec![state];
                self.ai_ui.suggestion_manager.update_candidates(candidates);
                return true;
            }

            if start.elapsed() > timeout {
                tracing::warn!("force_ai_suggestion: timeout");
                break;
            }

            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        self.ai_ui.suggestion_manager.active.is_some()
    }

    pub(super) async fn toggle_sudo(&mut self) -> Result<()> {
        input_analysis::toggle_sudo(self).await
    }

    /// Get directory listing for AI context
    pub(super) fn get_directory_listing(&self) -> String {
        get_directory_listing_content(std::path::Path::new(".")).join("\n")
    }

    pub(super) async fn expand_smart_pipe(&self, query: String) -> Result<String> {
        let service = self
            .services
            .ai
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("AI client not configured"))?;
        ai_features::expand_smart_pipe(service.as_ref(), &query).await
    }

    pub(super) async fn run_generative_command(&self, query: &str) -> Result<String> {
        let service = self
            .services
            .ai
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("AI client not configured"))?;
        ai_features::run_generative_command(service.as_ref(), query).await
    }

    pub(crate) fn detect_smart_pipe(&self) -> Option<String> {
        let input = self.input.as_str();
        if let Some(idx) = input.rfind("|?") {
            let query = input[idx + 2..].trim();
            if !query.is_empty() {
                return Some(query.to_string());
            }
        }
        None
    }

    pub(crate) fn detect_generative_command(&self) -> Option<String> {
        let input = self.input.as_str().trim_start();
        if let Some(query) = input.strip_prefix("??") {
            let query = query.trim();
            if !query.is_empty() {
                return Some(query.to_string());
            }
        }
        None
    }

    /// Detect AI Output Pipe pattern: `command |! "query"`
    /// Returns (command, query) if pattern is found
    pub(crate) fn detect_ai_pipe(&self) -> Option<(String, String)> {
        let input = self.input.as_str();
        if let Some(idx) = input.rfind("|!") {
            let command = input[..idx].trim().to_string();
            let query_part = input[idx + 2..].trim();

            // Extract query from quotes or as plain text
            let query = if (query_part.starts_with('"') && query_part.ends_with('"')
                || query_part.starts_with('\'') && query_part.ends_with('\''))
                && query_part.len() > 1
            {
                query_part[1..query_part.len() - 1].to_string()
            } else {
                query_part.to_string()
            };

            if !command.is_empty() && !query.is_empty() {
                return Some((command, query));
            }
        }
        None
    }

    /// Execute command, capture output, and send to AI for analysis
    pub(super) async fn run_ai_pipe(&mut self, command: String, query: String) -> Result<()> {
        use std::process::Command;

        let mut renderer = TerminalRenderer::new();
        queue!(renderer, Print("\r\n🔄 Running command...\r\n")).ok();
        renderer.flush().ok();

        // Execute the command and capture output
        let output = Command::new("sh").arg("-c").arg(&command).output();

        let (stdout, stderr, exit_code) = match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                let exit_code = out.status.code().unwrap_or(-1);
                (stdout, stderr, exit_code)
            }
            Err(e) => {
                queue!(
                    renderer,
                    Print(format!("❌ Failed to execute command: {}\r\n", e))
                )
                .ok();
                renderer.flush().ok();
                return Ok(());
            }
        };

        // Combine stdout and stderr for analysis
        let combined_output = if stderr.is_empty() {
            stdout
        } else if stdout.is_empty() {
            stderr
        } else {
            format!("STDOUT:\n{}\n\nSTDERR:\n{}", stdout, stderr)
        };

        // Check if AI service is available
        let Some(_service) = self.services.ai.clone() else {
            queue!(
                renderer,
                Print(format!(
                    "❌ AI service is not configured. {}\r\n",
                    dsh_openai::API_KEY_SETUP_HINT
                ))
            )
            .ok();
            renderer.flush().ok();
            return Ok(());
        };

        queue!(renderer, Print("🤖 Analyzing output...\r\n")).ok();
        renderer.flush().ok();

        // Call unified AI entry point
        queue!(renderer, Print("\r")).ok();
        queue!(renderer, Clear(ClearType::CurrentLine)).ok();

        // The captured output is unbounded; keep both ends so an error at the
        // tail of a long log still reaches the model.
        let bounded_output =
            dsh_openai::turn::truncate_middle(&combined_output, AI_PIPE_OUTPUT_CHARS);
        let message = format!(
            "Shell command: `{}`\n\nOutput:\n```\n{}\n```\n\nQuery: {}",
            command, bounded_output, query
        );

        let ctx = Context::new_safe(getpid(), getpid(), true);
        // Unlike the `!` prefix path in `eval_str`, nothing on this route
        // disables raw mode before the chat call - streaming's incremental
        // `write_stdout` calls need cooked mode for their newlines to land
        // as real line breaks instead of a staircase.
        let raw_mode_pause = terminal_state::RawModePause::new();
        let lifecycle = crate::agent_lifecycle::current(self.shell);
        let _turn = lifecycle.begin_turn();
        execute_chat_message(&ctx, &mut *self.shell, &message, None);
        drop(raw_mode_pause);

        self.state.last_status = exit_code;
        self.state.last_command_string = command;

        renderer.flush().ok();
        self.print_prompt(&mut renderer);
        renderer.flush().ok();

        Ok(())
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

    use crate::ai_features::AiService;
    use crate::environment::Environment;
    use crate::shell::Shell;
    use async_trait::async_trait;
    use serde_json::Value; // Add missing imports if needed

    struct MockAiService {
        response: String,
    }

    impl MockAiService {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
            }
        }
    }

    #[async_trait]
    impl AiService for MockAiService {
        async fn send_request(
            &self,
            _messages: Vec<Value>,
            _temperature: Option<f64>,
        ) -> Result<String> {
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn test_trigger_auto_fix_success() {
        use crate::environment::Environment;

        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        // Setup mock AI service
        let service = Arc::new(MockAiService::new(r#"{"command": "ls", "args": ["-la"]}"#));
        repl.services.ai = Some(service);

        // Setup failed state
        repl.state.last_command_string = "lss -la".to_string();
        repl.state.last_status = 127;

        // Enable auto_fix
        repl.ai_ui.input_preferences.auto_fix = true;

        repl.trigger_auto_fix();

        // Wait for the background task to complete and send the result
        if let Some(AiEvent::AutoFix(fix)) = repl.event_loop.recv_ai().await {
            repl.ai_ui.auto_fix_suggestion = Some(fix);
        }

        let fix = repl
            .ai_ui
            .auto_fix_suggestion
            .take()
            .expect("auto fix expected");
        assert_eq!(fix.replacement, "ls -la");
        assert_eq!(fix.kind, crate::repl::AutoFixKind::AiFix);
    }

    #[tokio::test]
    async fn test_detect_ai_pipe_with_double_quoted_query() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        repl.input
            .reset("ls -la |! \"show largest files\"".to_string());
        let result = repl.detect_ai_pipe();
        assert!(result.is_some());
        let (command, query) = result.unwrap();
        assert_eq!(command, "ls -la");
        assert_eq!(query, "show largest files");
    }

    #[tokio::test]
    async fn test_detect_ai_pipe_with_single_quoted_query() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        repl.input
            .reset("docker ps |! 'find running containers'".to_string());
        let result = repl.detect_ai_pipe();
        assert!(result.is_some());
        let (command, query) = result.unwrap();
        assert_eq!(command, "docker ps");
        assert_eq!(query, "find running containers");
    }

    #[tokio::test]
    async fn test_detect_ai_pipe_with_unquoted_query() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        repl.input.reset("cat file.txt |! summarize".to_string());
        let result = repl.detect_ai_pipe();
        assert!(result.is_some());
        let (command, query) = result.unwrap();
        assert_eq!(command, "cat file.txt");
        assert_eq!(query, "summarize");
    }

    #[tokio::test]
    async fn test_detect_ai_pipe_empty_query() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        repl.input.reset("ls -la |! ".to_string());
        let result = repl.detect_ai_pipe();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_detect_ai_pipe_empty_command() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        repl.input.reset("|! \"query\"".to_string());
        let result = repl.detect_ai_pipe();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_detect_ai_pipe_no_pattern() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        repl.input.reset("ls -la | grep foo".to_string());
        let result = repl.detect_ai_pipe();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_detect_ai_pipe_complex_command() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        repl.input
            .reset("kubectl get pods -n default |! \"問題のあるPodを見つけて\"".to_string());
        let result = repl.detect_ai_pipe();
        assert!(result.is_some());
        let (command, query) = result.unwrap();
        assert_eq!(command, "kubectl get pods -n default");
        assert_eq!(query, "問題のあるPodを見つけて");
    }

    #[tokio::test]
    async fn test_detect_smart_pipe_valid() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        repl.input.reset("ls |? filter directories".to_string());
        let result = repl.detect_smart_pipe();
        assert_eq!(result, Some("filter directories".to_string()));
    }

    #[tokio::test]
    async fn test_detect_smart_pipe_no_query() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        repl.input.reset("ls |?".to_string());
        let result = repl.detect_smart_pipe();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_detect_smart_pipe_empty_query() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        repl.input.reset("ls |?   ".to_string());
        let result = repl.detect_smart_pipe();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_detect_smart_pipe_no_pattern() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        repl.input.reset("ls | grep foo".to_string());
        let result = repl.detect_smart_pipe();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_detect_smart_pipe_multiple_pipes() {
        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        repl.input
            .reset("cat file.txt | head -10 |? find errors".to_string());
        let result = repl.detect_smart_pipe();
        assert_eq!(result, Some("find errors".to_string()));
    }
}

use crate::completion::path::path_completion_prefix_for_shell_token;
use crate::completion::shell_token::{self, SeparatorMode};
use crate::history::History;
use dsh_openai::{ChatGptClient, ChatRequestOptions};
use parking_lot::Mutex as ParkingMutex;
use serde_json::{Value, json};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;
use tokio::sync::Notify;
use tokio::task;
use tracing::{debug, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SuggestionMode {
    Off,
    #[default]
    Ghost,
}

impl SuggestionMode {
    pub fn is_enabled(self) -> bool {
        matches!(self, SuggestionMode::Ghost)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputPreferences {
    pub suggestion_mode: SuggestionMode,
    pub ai_backfill: bool,
    pub transient_prompt: bool,
    /// When enabled, show a hint to diagnose errors after command failures
    pub auto_diagnose: bool,
    /// When enabled, automatically trigger AI fix suggestion on command failure
    pub auto_fix: bool,
    /// When enabled, send desktop notification for long running commands
    pub auto_notify_enabled: bool,
    /// Threshold in seconds for auto notification
    pub auto_notify_threshold: u64,
    /// When enabled, automatically insert pairs for brackets and quotes
    pub auto_pair: bool,
    /// When enabled, show AI command explanation after idle time
    pub ai_explanation: bool,
    /// When enabled, pin a status line to the bottom row of the terminal.
    ///
    /// Off by default: it reserves a scroll region with DECSTBM, which not
    /// every terminal handles well.
    pub status_line: bool,
    /// When enabled, show a one-line proactive hint after a command fails
    /// (deterministic quick fix, or a pointer to Alt-f/Alt-d). On by default:
    /// the automatic path costs no AI request unless `auto_fix` is also on.
    ///
    /// This gates the whole automatic post-failure path, `auto_fix` included,
    /// because every part of it surfaces as that one hint. The manual Alt-f
    /// and Alt-d bindings are unaffected.
    pub failure_hint: bool,
}

impl Default for InputPreferences {
    fn default() -> Self {
        Self {
            suggestion_mode: SuggestionMode::Ghost,
            ai_backfill: false,
            transient_prompt: true,
            auto_diagnose: false,
            auto_fix: false,
            auto_notify_enabled: false,
            auto_notify_threshold: 10,
            auto_pair: false,
            ai_explanation: false,
            status_line: false,
            failure_hint: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionSource {
    History,
    Ai,
    Completion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestionState {
    pub full: String,
    pub source: SuggestionSource,
}

#[derive(Debug, Clone)]
struct CachedSuggestion {
    prefix: String,
    state: SuggestionState,
    generated_at: Instant,
}

const HISTORY_CONTEXT_LIMIT: usize = 8;

#[derive(Debug, Clone)]
pub struct SuggestionRequest {
    pub input: String,
    pub cursor: usize,
    pub preferences: InputPreferences,
    pub history_context: Vec<String>,
    pub cwd: Option<String>,
    pub files: Arc<Vec<String>>,
    pub last_exit_code: Option<i32>,
}

impl SuggestionRequest {
    pub fn new(
        input: String,
        cursor: usize,
        preferences: InputPreferences,
        history_context: Vec<String>,
        cwd: Option<String>,
        files: Arc<Vec<String>>,
        last_exit_code: Option<i32>,
    ) -> Self {
        Self {
            input,
            cursor,
            preferences,
            history_context,
            cwd,
            files,
            last_exit_code,
        }
    }
}

pub trait SuggestionBackend: Send + Sync {
    fn predict(&self, request: SuggestionRequest) -> Option<String>;

    fn prefetch(&self, _request: SuggestionRequest) {}

    fn is_pending(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SuggestionConfig {
    pub preferences: InputPreferences,
    pub history_ttl: Duration,
    pub ai_ttl: Duration,
}

impl Default for SuggestionConfig {
    fn default() -> Self {
        Self {
            preferences: InputPreferences::default(),
            history_ttl: Duration::from_millis(350),
            ai_ttl: Duration::from_secs(2),
        }
    }
}

pub struct SuggestionEngine {
    config: SuggestionConfig,
    history_cache: Option<CachedSuggestion>,
    ai_cache: Option<CachedSuggestion>,
    ai_backend: Option<Arc<dyn SuggestionBackend + Send + Sync>>,
}

impl Default for SuggestionEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SuggestionEngine {
    pub fn new() -> Self {
        Self {
            config: SuggestionConfig::default(),
            history_cache: None,
            ai_cache: None,
            ai_backend: None,
        }
    }

    pub fn set_preferences(&mut self, prefs: InputPreferences) {
        self.config.preferences = prefs;
        if !prefs.suggestion_mode.is_enabled() {
            self.history_cache = None;
            self.ai_cache = None;
        }
    }

    pub fn set_ai_backend(&mut self, backend: Option<Arc<dyn SuggestionBackend + Send + Sync>>) {
        self.ai_backend = backend;
    }

    pub fn prefetch(
        &self,
        cwd: Option<String>,
        files: Arc<Vec<String>>,
        last_exit_code: Option<i32>,
    ) {
        if let Some(backend) = &self.ai_backend
            && self.config.preferences.ai_backfill
        {
            let request = SuggestionRequest::new(
                String::new(), // Empty input for prefetch
                0,
                self.config.preferences,
                Vec::new(),
                cwd,
                files,
                last_exit_code,
            );
            backend.prefetch(request);
        }
    }

    pub fn ai_pending(&self) -> bool {
        self.config.preferences.ai_backfill
            && self
                .ai_backend
                .as_ref()
                .map(|backend| backend.is_pending())
                .unwrap_or(false)
    }

    pub fn predict(
        &mut self,
        input: &str,
        cursor: usize,
        history: Option<&Arc<ParkingMutex<History>>>,
    ) -> Vec<SuggestionState> {
        if !self.config.preferences.suggestion_mode.is_enabled() {
            self.history_cache = None;
            self.ai_cache = None;
            return Vec::new();
        }

        if input.is_empty() {
            self.history_cache = None;
            self.ai_cache = None;
            return Vec::new();
        }

        let char_len = input.chars().count();
        if cursor > char_len {
            return Vec::new();
        }

        let mut suggestions = Vec::new();

        if let Some(state) = self.use_cache(self.history_cache.as_ref(), input) {
            suggestions.push(state.clone());
        } else if let Some(state) = self.history_suggestion(input, history) {
            self.history_cache = Some(CachedSuggestion {
                prefix: input.to_string(),
                state: state.clone(),
                generated_at: Instant::now(),
            });
            suggestions.push(state);
        } else {
            self.history_cache = None;
        }

        // Try completion suggestion (lookahead)
        if suggestions.is_empty()
            && let Some(state) = self.completion_suggestion(input)
        {
            suggestions.push(state);
        }

        // If we have a history suggestion, we skip AI to prioritize it and minimize noise/latency
        if !suggestions.is_empty() {
            self.ai_cache = None;
            return suggestions;
        }

        // Check if command is in blocklist for AI suggestions
        if self.in_blocklist(input) {
            self.ai_cache = None;
            // Still allow history suggestions, but ensure AI is skipped
            if !suggestions.is_empty() {
                return suggestions;
            }
            // If we have no history suggestions and it's blocked, return empty
            return suggestions;
        }

        if self.config.preferences.ai_backfill {
            if let Some(state) = self.use_cache(self.ai_cache.as_ref(), input) {
                suggestions.push(state.clone());
            }

            if let Some(state) = self.ai_suggestion(input, cursor, history) {
                let duplicate = suggestions
                    .iter()
                    .any(|existing| existing.full == state.full && existing.source == state.source);
                if !duplicate {
                    suggestions.push(state.clone());
                }
                self.ai_cache = Some(CachedSuggestion {
                    prefix: input.to_string(),
                    state,
                    generated_at: Instant::now(),
                });
            }
        } else {
            self.ai_cache = None;
        }

        suggestions
    }

    pub fn predict_history(
        &mut self,
        input: &str,
        cursor: usize,
        history: Option<&Arc<parking_lot::Mutex<History>>>,
    ) -> Vec<SuggestionState> {
        if !self.config.preferences.suggestion_mode.is_enabled() {
            self.history_cache = None;
            self.ai_cache = None;
            return Vec::new();
        }

        if input.is_empty() {
            self.history_cache = None;
            self.ai_cache = None;
            return Vec::new();
        }

        let char_len = input.chars().count();
        if cursor > char_len {
            return Vec::new();
        }

        let mut suggestions = Vec::new();
        if let Some(state) = self.use_cache(self.history_cache.as_ref(), input) {
            suggestions.push(state.clone());
        } else if let Some(state) = self.history_suggestion(input, history) {
            self.history_cache = Some(CachedSuggestion {
                prefix: input.to_string(),
                state: state.clone(),
                generated_at: Instant::now(),
            });
            suggestions.push(state);
        } else {
            self.history_cache = None;
        }

        if !suggestions.is_empty() {
            self.ai_cache = None;
        }

        suggestions
    }

    fn in_blocklist(&self, input: &str) -> bool {
        const AI_SUGGESTION_BLOCKLIST: &[&str] = &["gco"];

        first_command_token(input)
            .is_some_and(|command| AI_SUGGESTION_BLOCKLIST.contains(&command.raw.as_str()))
    }

    fn completion_suggestion(&self, input: &str) -> Option<SuggestionState> {
        let cursor = input.chars().count();
        let token =
            shell_token::token_at_char_cursor(input, cursor, SeparatorMode::CompletionRange)?;
        if token.raw.is_empty() || token.byte_end != input.len() {
            return None;
        }

        let is_cd = is_cd_path_context(input, &token);

        if let Ok(Some(completion)) = path_completion_prefix_for_shell_token(&token.raw, is_cd) {
            let full = format!(
                "{}{}{}",
                &input[..token.byte_start],
                completion,
                &input[token.byte_end..]
            );

            if full == input || !full.starts_with(input) {
                return None;
            }

            return Some(SuggestionState {
                full,
                source: SuggestionSource::Completion,
            });
        }
        None
    }

    fn history_suggestion(
        &self,
        input: &str,
        history: Option<&Arc<ParkingMutex<History>>>,
    ) -> Option<SuggestionState> {
        let history = history?;
        let history = history.try_lock()?;
        let entry = history.search_first(input)?.to_string();
        if entry.len() <= input.len() {
            return None;
        }
        Some(SuggestionState {
            full: entry,
            source: SuggestionSource::History,
        })
    }

    fn ai_suggestion(
        &self,
        input: &str,
        cursor: usize,
        history: Option<&Arc<ParkingMutex<History>>>,
    ) -> Option<SuggestionState> {
        self.ai_suggestion_with_context(input, cursor, history, None, Arc::new(Vec::new()), None)
    }

    pub fn ai_suggestion_with_context(
        &self,
        input: &str,
        cursor: usize,
        history: Option<&Arc<ParkingMutex<History>>>,
        cwd: Option<String>,
        files: Arc<Vec<String>>,
        last_exit_code: Option<i32>,
    ) -> Option<SuggestionState> {
        let backend = self.ai_backend.as_ref()?;
        let history_context = collect_history_context(history, input, HISTORY_CONTEXT_LIMIT);
        let request = SuggestionRequest::new(
            input.to_string(),
            cursor,
            self.config.preferences,
            history_context,
            cwd,
            files,
            last_exit_code,
        );
        let completion = backend.predict(request)?;
        if !completion.starts_with(input) {
            return None;
        }
        Some(SuggestionState {
            full: completion,
            source: SuggestionSource::Ai,
        })
    }

    fn use_cache(&self, cache: Option<&CachedSuggestion>, input: &str) -> Option<SuggestionState> {
        let cached = cache?;
        if cached.prefix == input
            && cached.state.full.starts_with(input)
            && cached.state.full.len() > input.len()
            && cached.generated_at.elapsed() <= self.ttl_for(cached.state.source)
        {
            return Some(cached.state.clone());
        }
        None
    }

    fn ttl_for(&self, source: SuggestionSource) -> Duration {
        match source {
            SuggestionSource::History => self.config.history_ttl,
            SuggestionSource::Ai => self.config.ai_ttl,
            SuggestionSource::Completion => self.config.history_ttl,
        }
    }
}

fn collect_history_context(
    history: Option<&Arc<ParkingMutex<History>>>,
    _input: &str,
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }

    let history = match history {
        Some(history) => history,
        None => return Vec::new(),
    };

    let history = match history.try_lock() {
        Some(guard) => guard,
        None => return Vec::new(),
    };

    history.get_recent_context(limit)
}

fn first_command_token(input: &str) -> Option<shell_token::ShellTokenSpan> {
    shell_token::tokenize(input, SeparatorMode::Parser)
        .into_iter()
        .next()
}

fn is_cd_path_context(input: &str, token: &shell_token::ShellTokenSpan) -> bool {
    first_command_token(input)
        .is_some_and(|command| command.raw == "cd" && token.byte_start > command.byte_end)
}

#[path = "suggestion_ai_backend.rs"]
mod ai_backend;
pub use ai_backend::AiSuggestionBackend;
#[cfg(test)]
use ai_backend::build_user_payload;

// Flat sibling rather than `suggestion/tests.rs`, because
// `test_completion_suggestion_real_fs` below completes `ls src/sugg` against
// this repository's own `dsh/src/`: adding a `suggestion/` directory next to
// `suggestion.rs` makes that prefix ambiguous and the test fails. The module
// path stays `suggestion::tests`, so `use super::*` still reaches this file.
#[cfg(test)]
#[path = "suggestion_tests.rs"]
mod tests;

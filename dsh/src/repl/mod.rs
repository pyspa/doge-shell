use crate::ai_features::{self, AiService, LiveAiService};
use crate::command_timing::{self, SharedCommandTiming};
use crate::completion::integrated::IntegratedCompletionEngine;
use crate::completion::{self, Completion};
use crate::dirs;
use crate::environment::Environment;
use crate::history::FrecencyHistory;
use crate::input::{ColorType, Input, InputConfig, display_width};
use crate::parser::{self, HighlightKind, Rule};
use crate::prompt::Prompt;
use crate::shell::{SHELL_TERMINAL, Shell};
use crate::suggestion::{InputPreferences, SuggestionBackend, SuggestionSource, SuggestionState};
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Context as _;
use anyhow::Result;
use crossterm::cursor;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, EventStream, KeyEvent, KeyModifiers,
};
use crossterm::queue;
use crossterm::style::{Print, ResetColor};
use crossterm::terminal::{self, Clear, ClearType, enable_raw_mode};
use dsh_openai::{ChatGptClient, OpenAiConfig};
use futures::StreamExt;
use nix::sys::termios::{Termios, tcgetattr};
use nix::unistd::tcsetpgrp;
use parking_lot::Mutex as ParkingMutex;
use parking_lot::RwLock;
use pest::Span as PestSpan;
use pest::iterators::Pairs;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::{Instant as TokioInstant, MissedTickBehavior, interval_at};
use tracing::{debug, warn};

const NONE: KeyModifiers = KeyModifiers::NONE;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const ALT: KeyModifiers = KeyModifiers::ALT;
const SHIFT: KeyModifiers = KeyModifiers::SHIFT;
const AI_SUGGESTION_REFRESH_MS: u64 = 150;
const MCP_FORM_SUGGESTIONS: &[&str] =
    &["mcp-add-stdio", "mcp-add-http", "mcp-add-sse", "mcp-clear"];

mod state;
use state::*;
mod cache;
use cache::*;
mod suggestion_manager;
use suggestion_manager::*;
mod handler;

pub struct Repl<'a> {
    pub shell: &'a mut Shell,
    pub(crate) input: Input,
    pub(crate) columns: usize,
    pub(crate) lines: usize,
    pub(crate) tmode: Option<Termios>,
    pub(crate) history_search: Option<String>,
    pub(crate) start_completion: bool,
    pub(crate) completion: Completion,
    pub(crate) integrated_completion: IntegratedCompletionEngine,
    pub(crate) prompt: Arc<RwLock<Prompt>>,
    // Cached prompt mark and its display width to avoid recomputation on each redraw
    pub(crate) prompt_mark_cache: String,
    pub(crate) prompt_mark_width: usize,
    pub(crate) ctrl_c_state: DoublePressState,
    pub(crate) esc_state: DoublePressState,
    pub(crate) should_exit: bool,
    pub(crate) last_command_time: Option<Instant>,
    pub(crate) last_duration: Option<Duration>,
    pub(crate) last_status: i32,
    // short-term cache for history-based completion to reduce lock/sort frequency
    pub(crate) cache: HistoryCache,
    pub(crate) suggestion_manager: SuggestionManager,
    pub(crate) input_preferences: InputPreferences,
    pub(crate) ai_pending_shown: bool,
    pub(crate) ai_service: Option<Arc<dyn AiService + Send + Sync>>,
    pub(crate) command_timing: SharedCommandTiming,
    pub(crate) last_command_string: String,
}

impl<'a> Drop for Repl<'a> {
    fn drop(&mut self) {
        let mut renderer = TerminalRenderer::new();
        queue!(renderer, DisableBracketedPaste).ok();
        renderer.flush().ok();
        self.save_history();
        // Save command timing statistics
        if let Some(path) = command_timing::get_timing_file_path()
            && let Err(e) = self.command_timing.write().save_to_file(&path)
        {
            warn!("Failed to save command timing: {}", e);
        }
    }
}

impl<'a> Repl<'a> {
    pub fn new(shell: &'a mut Shell) -> Self {
        let current = std::env::current_dir().unwrap_or_else(|e| {
            warn!(
                "Failed to get current directory: {}, using home directory",
                e
            );
            std::env::var("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| {
                    warn!("Failed to get home directory, using root");
                    std::path::PathBuf::from("/")
                })
        });
        let prompt = Prompt::new(current, "🐕 < ".to_string());

        let prompt = Arc::new(RwLock::new(prompt));
        shell
            .environment
            .write()
            .chpwd_hooks
            .push(Box::new(Arc::clone(&prompt)));
        let input_config = InputConfig::default();

        let prompt_mark_cache = prompt.read().mark.clone();
        let prompt_mark_width = display_width(&prompt_mark_cache);

        let envronment = Arc::clone(&shell.environment);
        let input_preferences = envronment.read().input_preferences();
        let mut suggestion_manager = SuggestionManager::new();
        let mut ai_service: Option<Arc<dyn AiService + Send + Sync>> = None;
        if let Some((ai_backend, client)) = Self::build_ai_backend(&envronment) {
            suggestion_manager.engine.set_ai_backend(Some(ai_backend));
            ai_service = Some(Arc::new(LiveAiService::new(client)));
        }
        suggestion_manager.set_preferences(input_preferences);
        Repl {
            shell,
            input: Input::new(input_config),
            columns: 0,
            lines: 0,
            tmode: None,
            history_search: None,
            start_completion: false,
            completion: Completion::new(),
            integrated_completion: IntegratedCompletionEngine::new(envronment),
            prompt,
            prompt_mark_cache,
            prompt_mark_width,
            ctrl_c_state: DoublePressState::new(3000), // 3 seconds for Ctrl+C
            esc_state: DoublePressState::new(400),     // 400ms for Esc (sudo toggle)
            should_exit: false,
            last_command_time: None,
            last_duration: None,
            last_status: 0,
            cache: HistoryCache::new(Duration::from_millis(300)),
            suggestion_manager,
            input_preferences,
            ai_pending_shown: false,
            ai_service,
            command_timing: command_timing::create_shared_timing(),
            last_command_string: String::new(),
        }
    }

    pub(crate) async fn perform_auto_fix(&mut self) {
        if self.last_status != 0
            && !self.last_command_string.is_empty()
            && let Some(service) = &self.ai_service
        {
            match crate::ai_features::fix_command(
                service.as_ref(),
                &self.last_command_string,
                self.last_status,
            )
            .await
            {
                Ok(fixed) => {
                    self.input.reset(fixed);
                }
                Err(e) => {
                    warn!("Auto-Fix failed: {}", e);
                }
            }
        }
    }

    pub(crate) async fn perform_smart_commit_logic(&mut self, diff: &str) {
        if let Some(service) = &self.ai_service {
            match crate::ai_features::generate_commit_message(service.as_ref(), diff).await {
                Ok(message) => {
                    let commit_cmd = format!("git commit -m \"{}\"", message);
                    self.input.reset(commit_cmd);
                }
                Err(e) => {
                    warn!("Smart Git Commit failed: {}", e);
                }
            }
        }
    }

    fn build_ai_backend(
        environment: &Arc<RwLock<Environment>>,
    ) -> Option<(Arc<dyn SuggestionBackend + Send + Sync>, ChatGptClient)> {
        let env_handle = Arc::clone(environment);
        let config = OpenAiConfig::from_getter(|key| {
            let value = {
                let guard = env_handle.read();
                guard.get_var(key)
            };
            value.or_else(|| std::env::var(key).ok())
        });

        config.api_key()?;

        match ChatGptClient::try_from_config(&config) {
            Ok(client) => {
                let backend = Arc::new(crate::suggestion::AiSuggestionBackend::new(client.clone()));
                Some((backend, client))
            }
            Err(err) => {
                warn!("Failed to initialize AI suggestion backend: {err:?}");
                None
            }
        }
    }

    fn setup(&mut self) {
        let screen_size = terminal::size().unwrap_or_else(|e| {
            warn!("Failed to get terminal size: {}, using default 80x24", e);
            (80, 24)
        });
        self.columns = screen_size.0 as usize;

        // Initialize integrated completion engine
        debug!("Initializing integrated completion engine (this may use cached JSON data)...");
        if let Err(e) = self.integrated_completion.initialize_command_completion() {
            warn!("Failed to initialize command completion: {}", e);
        } else {
            debug!("Integrated completion engine initialized successfully");
        }
        self.lines = screen_size.1 as usize;
        self.lines = screen_size.1 as usize;
        enable_raw_mode().ok();
        let mut renderer = TerminalRenderer::new();
        queue!(renderer, EnableBracketedPaste).ok();
        renderer.flush().ok();
    }

    pub(crate) async fn check_background_jobs(&mut self, output: bool) -> Result<()> {
        handler::check_background_jobs(self, output).await
    }

    pub(crate) async fn handle_event(&mut self, ev: ShellEvent) -> Result<()> {
        handler::handle_event(self, ev).await
    }

    pub(crate) async fn handle_paste_event(&mut self, text: &str) -> Result<()> {
        handler::handle_paste_event(self, text).await
    }

    pub(crate) async fn handle_key_event(&mut self, ev: &KeyEvent) -> Result<()> {
        handler::handle_key_event(self, ev).await
    }

    fn save_history(&mut self) {
        Self::save_single_history_helper(&mut self.shell.cmd_history, "command");
        Self::save_single_history_helper(&mut self.shell.path_history, "path");
    }

    fn save_single_history_helper(
        history: &mut Option<Arc<ParkingMutex<FrecencyHistory>>>,
        history_type: &str,
    ) {
        if let Some(history) = history {
            if let Some(mut history_guard) = history.try_lock() {
                // Only save if there are changes
                if let Some(ref store) = history_guard.store {
                    if store.changed {
                        if let Err(e) = history_guard.save() {
                            warn!("Failed to save {} history: {}", history_type, e);
                        } else {
                            // debug!("{} history saved successfully", history_type);
                        }
                    } else {
                        // debug!("{} history unchanged, skipping save", history_type);
                    }
                }
            } else {
                // debug!("{} history is locked, skipping save", history_type);
            }
        }
    }

    fn save_history_periodic(&mut self) {
        // Save both command and path history if they have changes
        Self::save_single_history_helper(&mut self.shell.cmd_history, "command");
        Self::save_single_history_helper(&mut self.shell.path_history, "path");
    }

    pub(crate) fn move_cursor_input_end<W: Write>(&self, out: &mut W) {
        let prompt_display_width = self.prompt_mark_width;
        // cache locally to avoid duplicate computation chains
        let input_cursor_width = self.input.cursor_display_width();
        let mut cursor_display_pos = prompt_display_width + input_cursor_width;

        // debug!(
        //     "move_cursor_input_end: prompt_mark='{}', prompt_width={}, input_cursor_width={}, final_pos={}",
        //     self.prompt_mark_cache, prompt_display_width, input_cursor_width, cursor_display_pos
        // );
        // debug!(
        //     "move_cursor_input_end: input_text='{}', input_cursor_pos={}",
        //     self.input.as_str(),
        //     self.input.cursor()
        // );

        // bound to current terminal columns if available
        if self.columns > 0 {
            cursor_display_pos = cursor_display_pos.min(self.columns.saturating_sub(1));
        } else {
            cursor_display_pos = cursor_display_pos.min(1000);
        }

        // crossterm uses 0-based column indexing
        queue!(
            out,
            ResetColor,
            cursor::MoveToColumn(cursor_display_pos as u16)
        )
        .ok();
    }

    /// Move cursor relatively on the input line given previous and new display positions
    pub(crate) fn move_cursor_relative(
        &self,
        out: &mut impl Write,
        prev_display_pos: usize,
        new_display_pos: usize,
    ) {
        if new_display_pos == prev_display_pos {
            return;
        }
        if new_display_pos > prev_display_pos {
            let delta = (new_display_pos - prev_display_pos) as u16;
            queue!(out, cursor::MoveRight(delta)).ok();
        } else {
            let delta = (prev_display_pos - new_display_pos) as u16;
            queue!(out, cursor::MoveLeft(delta)).ok();
        }
    }

    // fn move_cursor(&self, len: usize) {
    //     let mut stdout = std::io::stdout();
    //     let prompt_size = self.get_prompt().chars().count();
    //     queue!(
    //         stdout,
    //         ResetColor,
    //         cursor::MoveToColumn((prompt_size + len + 1) as u16),
    //     )
    //     .ok();
    // }

    pub(crate) fn print_prompt(&mut self, out: &mut impl Write) {
        // debug!("print_prompt called - full preprompt + mark redraw");

        // Execute pre-prompt hooks
        if let Err(e) = self.shell.exec_pre_prompt_hooks() {
            debug!("Error executing pre-prompt hooks: {}", e);
        }

        let mut prompt = self.prompt.write();

        // Draw right prompt first (same line as preprompt)
        prompt.print_right_prompt(out, self.columns, self.last_status, self.last_duration);
        // draw preprompt only here (initial or after command/bg output)
        prompt.print_preprompt(out);
        out.write_all(b"\r\n").ok();
        // update cached mark and width in case mark changed
        // update cached mark and width in case mark changed
        if self.prompt_mark_cache != prompt.mark {
            self.prompt_mark_cache = prompt.mark.clone();
            self.prompt_mark_width = display_width(&self.prompt_mark_cache);
        }
        // draw mark only (defer flushing to caller for batching)
        out.write_all(b"\r").ok();
        out.write_all(self.prompt_mark_cache.as_bytes()).ok();
        // no out.flush() here
    }

    fn sync_input_preferences(&mut self) {
        let prefs = self.shell.environment.read().input_preferences();
        if prefs != self.input_preferences {
            self.input_preferences = prefs;
            self.suggestion_manager.engine.set_preferences(prefs);
        }
    }

    fn refresh_inline_suggestion(&mut self) -> bool {
        if self.input.completion.is_some() {
            let had_suggestion = !self.suggestion_manager.candidates.is_empty();
            self.suggestion_manager.clear();
            return had_suggestion;
        }

        self.sync_input_preferences();
        let history_ref = self.shell.cmd_history.as_ref();
        let current_input = self.input.to_string();
        let cursor_pos = self.input.cursor();
        let mut candidates =
            self.suggestion_manager
                .engine
                .predict(current_input.as_str(), cursor_pos, history_ref);

        if let Some(extra) = self.completion_suggestion(current_input.as_str()) {
            let duplicate = candidates
                .iter()
                .any(|item| item.full == extra.full && item.source == extra.source);
            if !duplicate {
                candidates.push(extra);
            }
        }

        self.suggestion_manager.update_candidates(candidates)
    }

    fn completion_suggestion(&mut self, input: &str) -> Option<SuggestionState> {
        if input.is_empty() || self.input.cursor() != self.input.len() {
            return None;
        }

        if let Ok(words) = self.input.get_words()
            && let Some(full) = self.word_based_completion(input, &words)
        {
            return Some(SuggestionState {
                full,
                source: SuggestionSource::Completion,
            });
        }

        Self::mcp_form_completion(input).map(|full| SuggestionState {
            full,
            source: SuggestionSource::Completion,
        })
    }

    fn word_based_completion(
        &self,
        input: &str,
        words: &[(Rule, PestSpan<'_>, bool)],
    ) -> Option<String> {
        for (rule, span, current) in words {
            if !current {
                continue;
            }
            let word = span.as_str();
            if word.is_empty() {
                continue;
            }
            match rule {
                Rule::argv0 => {
                    if let Some(result) = self.complete_command_word(input, span, word) {
                        return Some(result);
                    }
                }
                Rule::args => {
                    if let Some(result) = Self::complete_argument_word(input, span, word) {
                        return Some(result);
                    }
                }
                _ => {}
            }
        }
        None
    }

    pub(crate) fn complete_command_word(
        &self,
        input: &str,
        span: &PestSpan<'_>,
        word: &str,
    ) -> Option<String> {
        let candidate = {
            let env = self.shell.environment.read();
            env.search(word)
        };

        if let Some(name) = candidate
            && name.len() > word.len()
        {
            return Some(Self::replace_range(input, span.start(), span.end(), &name));
        }

        if let Ok(Some(path)) = completion::path_completion_prefix(word)
            && dirs::is_dir(&path)
            && path.len() > word.len()
        {
            return Some(Self::replace_range(input, span.start(), span.end(), &path));
        }

        None
    }

    pub(crate) fn complete_argument_word(
        input: &str,
        span: &PestSpan<'_>,
        word: &str,
    ) -> Option<String> {
        let path = completion::path_completion_prefix(word).ok().flatten()?;
        if path.len() <= word.len() {
            return None;
        }
        let suffix = &path[word.len()..];
        if suffix.is_empty() {
            return None;
        }
        let mut result = input.to_string();
        result.insert_str(span.end(), suffix);
        Some(result)
    }

    pub(crate) fn mcp_form_completion(input: &str) -> Option<String> {
        let trimmed = input.trim_end();
        if trimmed.is_empty() {
            return None;
        }
        let token = Self::trailing_symbol(trimmed);
        if token.is_empty() || !token.starts_with("mcp-") {
            return None;
        }
        for candidate in MCP_FORM_SUGGESTIONS {
            if candidate.starts_with(token) && candidate.len() > token.len() {
                let suffix = &candidate[token.len()..];
                let mut output = trimmed.to_string();
                output.push_str(suffix);
                if trimmed.len() < input.len() {
                    output.push_str(&input[trimmed.len()..]);
                }
                return Some(output);
            }
        }
        None
    }

    pub(crate) fn trailing_symbol(input: &str) -> &str {
        let boundary = input
            .rfind(|c: char| c.is_whitespace() || matches!(c, '(' | ')'))
            .map(|idx| idx + 1)
            .unwrap_or(0);
        &input[boundary..]
    }

    pub(crate) fn replace_range(
        input: &str,
        start: usize,
        end: usize,
        replacement: &str,
    ) -> String {
        let mut result = String::with_capacity(input.len() + replacement.len());
        result.push_str(&input[..start]);
        result.push_str(replacement);
        result.push_str(&input[end..]);
        result
    }

    pub(crate) fn highlight_result_to_ranges(
        &self,
        highlight: parser::HighlightResult,
        input: &str,
    ) -> (Vec<(usize, usize, ColorType)>, bool) {
        let mut tokens = highlight.tokens;
        let error = highlight.error;

        // Skip sort if already sorted (common case)
        let needs_sort = tokens.windows(2).any(|w| w[0].start > w[1].start);
        if needs_sort {
            tokens.sort_by_key(|token| token.start);
        }

        let mut ranges = Vec::with_capacity(tokens.len() + error.as_ref().map(|_| 1).unwrap_or(0));
        let mut can_execute = false;
        let len = input.len();

        for token in tokens {
            if token.start >= token.end || token.end > len {
                continue;
            }
            let color = match token.kind {
                HighlightKind::Command => {
                    let word = &input[token.start..token.end];
                    if self.command_is_valid(word) {
                        can_execute = true;
                        ColorType::CommandExists
                    } else {
                        ColorType::CommandNotExists
                    }
                }
                HighlightKind::Argument | HighlightKind::Bareword => ColorType::Argument,
                HighlightKind::Variable => ColorType::Variable,
                HighlightKind::SingleQuoted => ColorType::SingleQuote,
                HighlightKind::DoubleQuoted => ColorType::DoubleQuote,
                HighlightKind::Redirect => ColorType::Redirect,
                HighlightKind::Pipe => ColorType::Pipe,
                HighlightKind::Operator => ColorType::Operator,
                HighlightKind::Background => ColorType::Background,
                HighlightKind::ProcSubstitution => ColorType::ProcSubst,
                HighlightKind::Error => ColorType::Error,
            };
            ranges.push((token.start, token.end, color));
        }

        if let Some(err) = error
            && err.start < err.end
            && err.end <= len
        {
            ranges.push((err.start, err.end, ColorType::Error));
        }

        (ranges, can_execute)
    }

    pub(crate) fn compute_color_ranges_from_pairs<'p>(
        &self,
        pairs: Pairs<'p, Rule>,
        input: &str,
    ) -> (Vec<(usize, usize, ColorType)>, bool) {
        let highlight = parser::collect_highlight_tokens_from_pairs(pairs, input.len());
        self.highlight_result_to_ranges(highlight, input)
    }

    pub(crate) fn accept_active_suggestion(&mut self) -> bool {
        self.accept_suggestion(SuggestionAcceptMode::Full)
    }

    pub(crate) fn accept_suggestion(&mut self, mode: SuggestionAcceptMode) -> bool {
        let suggestion = match self.suggestion_manager.active.clone() {
            Some(state) => state,
            None => return false,
        };

        let current = self.input.as_str().to_string();
        if !suggestion.full.starts_with(&current) || suggestion.full.len() <= current.len() {
            return false;
        }

        let suffix = &suggestion.full[current.len()..];
        if suffix.is_empty() {
            return false;
        }

        let insert_chunk = match mode {
            SuggestionAcceptMode::Full => suffix.to_string(),
            SuggestionAcceptMode::Word => match Self::next_word_chunk(suffix) {
                Some(chunk) => chunk,
                None => return false,
            },
        };

        let inserted_all = insert_chunk.len() == suffix.len();
        self.input.insert_str(&insert_chunk);

        if matches!(mode, SuggestionAcceptMode::Full) && inserted_all {
            self.learn_suggestion(&suggestion.full);
            self.suggestion_manager.clear();
        }

        true
    }

    pub(crate) fn next_word_chunk(suffix: &str) -> Option<String> {
        if suffix.is_empty() {
            return None;
        }

        let mut end = suffix.len();
        let mut in_word = false;
        for (idx, ch) in suffix.char_indices() {
            if ch.is_whitespace() {
                if in_word {
                    end = idx + ch.len_utf8();
                    break;
                }
            } else {
                in_word = true;
            }
        }

        if !in_word {
            return Some(suffix.to_string());
        }

        Some(suffix[..end.min(suffix.len())].to_string())
    }

    fn learn_suggestion(&self, suggestion: &str) {
        if let Some(history) = &self.shell.cmd_history
            && let Some(mut history) = history.try_lock()
        {
            history.add(suggestion);
        }
    }

    fn stop_history_mode(&mut self) {
        self.history_search = None;
        if let Some(ref mut history) = self.shell.cmd_history
            && let Some(mut history) = history.try_lock()
        {
            history.search_word = None;
            history.reset_index();
        }
        // If we can't get the lock, we just won't be able to stop history mode - no warning needed
    }

    pub(crate) fn set_completions(&mut self) {
        let now = Instant::now();
        let input_str = self.input.as_str().to_string();
        let is_empty = input_str.is_empty();

        // Try using cache first when TTL is valid and prefix unchanged
        if let Some(last_time) = self.cache.time
            && now.duration_since(last_time) <= self.cache.ttl
            && self.cache.prefix == input_str
        {
            if is_empty {
                if let Some(ref comps) = self.cache.sorted_recent {
                    self.completion
                        .set_completions(self.input.as_str(), comps.clone());
                    return;
                }
            } else if let Some(ref comps) = self.cache.match_sorted {
                self.completion
                    .set_completions(self.input.as_str(), comps.clone());
                return;
            }
        }

        // Fallback to computing with lock, and refresh cache if successful
        if let Some(ref mut history) = self.shell.cmd_history {
            if let Some(history) = history.try_lock() {
                // If store changed, invalidate cache
                let changed = history.store.as_ref().map(|s| s.changed).unwrap_or(false);
                if changed {
                    self.cache.sorted_recent = None;
                    self.cache.match_sorted = None;
                    self.cache.time = None;
                }

                let comps = if is_empty {
                    let list = history.sorted(&dsh_frecency::SortMethod::Recent);
                    self.cache.sorted_recent = Some(list.clone());
                    list
                } else {
                    let list = history.sort_by_match(&input_str);
                    self.cache.match_sorted = Some(list.clone());
                    list
                };

                self.cache.prefix = input_str;
                self.cache.time = Some(now);

                self.completion.set_completions(self.input.as_str(), comps);
            } else {
                // If we can't get the lock immediately, try using the cache if available, otherwise empty
                if let Some(last_time) = self.cache.time
                    && now.duration_since(last_time) <= self.cache.ttl
                {
                    if is_empty {
                        if let Some(ref comps) = self.cache.sorted_recent {
                            self.completion
                                .set_completions(self.input.as_str(), comps.clone());
                            return; // Exit early since we used the cache
                        }
                    } else if let Some(ref comps) = self.cache.match_sorted {
                        self.completion
                            .set_completions(self.input.as_str(), comps.clone());
                        return; // Exit early since we used the cache
                    }
                }
                // Set empty completions as fallback when lock is not available
                self.completion.set_completions(self.input.as_str(), vec![]);
                // No warning here since this is expected during high contention
            }
        }
    }

    fn get_completion_from_history(&mut self, input: &str) -> Option<String> {
        let now = Instant::now();
        // Try cached match-sorted list first if still fresh and prefix unchanged
        if let Some(last_time) = self.cache.time
            && now.duration_since(last_time) <= self.cache.ttl
            && self.cache.prefix.starts_with(input)
            && let Some(ref list) = self.cache.match_sorted
            && let Some(top) = list.iter().find(|it| it.item.starts_with(input))
        {
            let entry = top.item.clone();
            self.input.completion = Some(entry.clone());
            if entry.len() >= input.len() {
                return Some(entry[input.len()..].to_string());
            }
        }

        if let Some(ref mut history) = self.shell.cmd_history
            && let Some(history) = history.try_lock()
            && let Some(entry) = history.search_prefix(input)
        {
            self.input.completion = Some(entry.clone());
            if entry.len() >= input.len() {
                return Some(entry[input.len()..].to_string());
            }
        }
        // If we can't get the lock, completion just won't be available - no warning needed
        None
    }

    pub fn print_input(
        &mut self,
        out: &mut impl Write,
        reset_completion: bool,
        refresh_suggestion: bool,
    ) {
        // debug!("print_input called, reset_completion: {}", reset_completion);
        queue!(out, cursor::Hide).ok();
        let input = self.input.as_str().to_owned();
        let _prompt_display_width = self.prompt_mark_width; // cached at new()/print_prompt()
        // debug!(
        //     "Current input: '{}', prompt_display_width: {}",
        //     input, _prompt_display_width
        // );

        let mut completion: Option<String> = None;
        if input.is_empty() || reset_completion {
            self.input.completion = None;
            self.input.color_ranges = None;
            self.input.can_execute = false;
        } else {
            completion = self.get_completion_from_history(&input);

            // TODO refactor
            // Perform single parse for both words extraction and highlighting
            use pest::Parser;
            match parser::ShellParser::parse(Rule::commands, &input) {
                Ok(pairs) => {
                    // 1. Get words for completion check
                    let words = self.input.get_words_from_pairs(pairs.clone());

                    for (ref rule, ref span, current) in words {
                        let word = span.as_str();
                        if word.is_empty() {
                            continue;
                        }

                        match rule {
                            Rule::argv0 => {
                                // Completion logic for command names
                                if current && completion.is_none() {
                                    if let Some(file) = self.shell.environment.read().search(word) {
                                        if file.len() >= input.len() {
                                            completion = Some(file[input.len()..].to_string());
                                        }
                                        self.input.completion = Some(file);
                                        break;
                                    } else if let Ok(Some(dir)) =
                                        completion::path_completion_prefix(word)
                                        && dirs::is_dir(&dir)
                                    {
                                        if dir.len() >= input.len() {
                                            completion = Some(dir[input.len()..].to_string());
                                        }
                                        self.input.completion = Some(dir.to_string());
                                        break;
                                    }
                                }
                            }
                            Rule::args => {
                                // Completion logic for arguments
                                if current
                                    && completion.is_none()
                                    && let Ok(Some(path)) = completion::path_completion_prefix(word)
                                    && path.len() >= word.len()
                                {
                                    let part = path[word.len()..].to_string();
                                    completion = Some(path[word.len()..].to_string());

                                    if let Some((pre, post)) = self.input.split_current_pos() {
                                        self.input.completion = Some(pre.to_owned() + &part + post);
                                    } else {
                                        self.input.completion = Some(input.clone() + &part);
                                    }
                                    break;
                                }
                            }
                            _ => {
                                // For other rule types, leave them with default color
                            }
                        }
                    }

                    // 2. Compute color ranges using the same pairs
                    let (color_ranges, can_execute) =
                        self.compute_color_ranges_from_pairs(pairs, &input);
                    self.input.color_ranges = Some(color_ranges);
                    self.input.can_execute = can_execute;

                    // Apply visual improvements for valid paths
                    if let Some(ref mut ranges) = self.input.color_ranges {
                        for (start, end, kind) in ranges.iter_mut() {
                            // Check if Argument is a valid path
                            if matches!(kind, crate::input::ColorType::Argument) {
                                let word = &input[*start..*end];
                                // Clean up quotes if present for path check
                                let clean_word = word.trim_matches(|c| c == '\'' || c == '"');
                                let path = std::path::Path::new(clean_word);
                                if path.exists() {
                                    *kind = crate::input::ColorType::ValidPath;
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    // Parsing failed, highlight the error
                    let mut ranges = Vec::new();
                    if let Some(token) = parser::highlight_error_token(&input, err.location) {
                        ranges.push((token.start, token.end, ColorType::Error));
                    }
                    self.input.color_ranges = Some(ranges);
                    self.input.can_execute = false;
                }
            }
        }

        if completion.is_none() {
            if refresh_suggestion {
                self.refresh_inline_suggestion();
            }
        } else {
            self.suggestion_manager.clear();
        }

        let ghost_suffix = if completion.is_none() {
            self.suggestion_manager.suffix(&input)
        } else {
            None
        };

        let ai_pending_now = self.suggestion_manager.engine.ai_pending();

        // Clear the current line and redraw prompt mark + input
        queue!(out, Print("\r"), Clear(ClearType::CurrentLine)).ok();

        // Only redraw the prompt mark (not the full preprompt)
        // Use cached prompt mark without re-locking prompt
        // debug!("Redrawing prompt mark: '{}'", self.prompt_mark_cache);
        queue!(out, Print(self.prompt_mark_cache.as_str())).ok();

        // Print the input
        self.input.print(out, ghost_suffix.as_deref());

        if ai_pending_now {
            queue!(out, Print(" ⧗")).ok();
        }

        self.ai_pending_shown = ai_pending_now;

        self.move_cursor_input_end(out);

        if let Some(completion) = completion {
            self.input.print_candidates(out, completion);
            // reuse cached cursor width implicitly via move_cursor_input_end recomputation; avoid extra heavy work here
            self.move_cursor_input_end(out);
        }
        queue!(out, cursor::Show).ok();
    }

    pub async fn run_interactive(&mut self) -> Result<()> {
        let mut reader = EventStream::new();

        self.setup();

        debug!(
            "shell setpgid pid:{:?} pgid:{:?}",
            self.shell.pid, self.shell.pgid
        );
        let _ = tcsetpgrp(SHELL_TERMINAL, self.shell.pgid).context("failed tcsetpgrp");
        self.tmode = match tcgetattr(SHELL_TERMINAL) {
            Ok(tmode) => Some(tmode),
            Err(e) => {
                warn!("Failed to get terminal attributes: {}", e);
                None
            }
        };
        {
            let mut renderer = TerminalRenderer::new();
            // start repl loop
            self.print_prompt(&mut renderer);
            // ensure preprompt + mark are flushed on initial draw
            renderer.flush().ok();
        }
        self.shell.check_job_state().await?;

        let _last_save_time = Instant::now();
        let mut background_interval = interval_at(
            TokioInstant::now() + Duration::from_millis(1000),
            Duration::from_millis(1000),
        );
        background_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut ai_refresh_interval = interval_at(
            TokioInstant::now() + Duration::from_millis(AI_SUGGESTION_REFRESH_MS),
            Duration::from_millis(AI_SUGGESTION_REFRESH_MS),
        );
        ai_refresh_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = background_interval.tick() => {
                    // Save history every 30 seconds if there have been changes
                    self.save_history_periodic();
                    self.check_background_jobs(true).await?;

                    // Execute input-timeout hooks (called periodically when idle)
                    let _ = self.shell.exec_input_timeout_hooks();

                    // Refresh git status
                    let prompt = Arc::clone(&self.prompt);
                    let should = prompt.read().should_refresh();
                    if should {
                        let path = prompt.read().current_path().to_path_buf();
                        tokio::spawn(async move {
                            if let Some(status) = crate::prompt::fetch_git_status_async(&path).await {
                                prompt.write().update_git_status(Some(status));
                            }
                        });
                    }
                },
                _ = ai_refresh_interval.tick() => {
                    let mut need_redraw = false;
                    if self.input_preferences.ai_backfill
                        && self.input.completion.is_none()
                        && self.refresh_inline_suggestion()
                    {
                        need_redraw = true;
                    }

                    if self.suggestion_manager.engine.ai_pending() != self.ai_pending_shown {
                        need_redraw = true;
                    }

                    if need_redraw {
                        let mut renderer = TerminalRenderer::new();
                        self.print_input(&mut renderer, false, false);
                        renderer.flush().ok();
                    }
                }
                maybe_event = reader.next() => {
                    match maybe_event {
                        Some(Ok(event)) => {
                            // match event {
                            //     Event::Key(KeyEvent { code: KeyCode::Enter, .. }) => {
                            //         start_time = Some(Instant::now());
                            //     }
                            //     _ => {}
                            // }
                            if let Err(err) = self.handle_event(ShellEvent::Input(event)).await{
                                self.shell.print_error(format!("Error: {err:?}\r"));
                                break;
                            }
                        }
                        Some(Err(err)) => {
                            self.shell.print_error(format!("Error: {err:?}\r"));
                            break;
                        },
                        None => break,
                    }
                }
            };

            if self.start_completion {
                // show completion
                self.start_completion = false;
            }
            if self.should_exit || self.shell.exited.is_some() {
                debug!("Shell exiting normally");
                if !self.shell.wait_jobs.is_empty() {
                    // TODO show message
                }
                break;
            }
        }
        self.shell.kill_wait_jobs()?;
        Ok(())
    }

    pub fn select_history(&mut self) {
        let query = self.input.as_str();
        if let Some(ref mut history) = self.shell.cmd_history {
            if let Some(mut history) = history.try_lock() {
                let histories = history.sorted(&dsh_frecency::SortMethod::Recent);
                if let Some(val) = completion::select_item_with_skim(
                    histories
                        .into_iter()
                        .map(|history| completion::Candidate::Basic(history.item))
                        .collect(),
                    Some(query),
                ) {
                    // Replace current input with the selected history command
                    self.input.reset(val);
                }
                history.reset_index();
            } else {
                warn!(
                    "Failed to acquire command history lock for history selection - lock is busy"
                );
            }
        }
    }

    fn command_is_valid(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }

        {
            let env = self.shell.environment.read();
            if env.lookup(word).is_some() {
                return true;
            }

            if env.alias.contains_key(word) {
                return true;
            }
        }

        if dsh_builtin::get_command(word).is_some() {
            return true;
        }

        self.shell.lisp_engine.borrow().is_export(word)
    }

    async fn toggle_sudo(&mut self) -> Result<()> {
        let mut input = self.input.as_str().to_string();
        if input.starts_with("sudo ") {
            // Remove sudo
            input = input[5..].to_string();
        } else {
            // Add sudo
            input.insert_str(0, "sudo ");
        }
        self.input.reset(input);
        let mut renderer = TerminalRenderer::new();
        self.print_input(&mut renderer, true, true);
        renderer.flush().ok();
        Ok(())
    }
    async fn expand_smart_pipe(&self, query: String) -> Result<String> {
        let service = self
            .ai_service
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("AI client not configured"))?;
        ai_features::expand_smart_pipe(service.as_ref(), &query).await
    }

    async fn run_generative_command(&self, query: &str) -> Result<String> {
        let service = self
            .ai_service
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("AI client not configured"))?;
        ai_features::run_generative_command(service.as_ref(), query).await
    }

    fn detect_smart_pipe(&self) -> Option<String> {
        let input = self.input.as_str();
        if let Some(idx) = input.rfind("|?") {
            let query = input[idx + 2..].trim();
            if !query.is_empty() {
                return Some(query.to_string());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::Environment;
    use crate::shell::Shell;
    use std::thread;

    #[tokio::test]
    async fn background_interval_ticks_even_with_busy_events() {
        let mut interval = interval_at(TokioInstant::now(), Duration::from_millis(5));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut events = futures::stream::repeat(());

        let deadline = TokioInstant::now() + Duration::from_millis(50);
        let mut ticks = 0usize;

        while ticks < 3 && TokioInstant::now() < deadline {
            tokio::select! {
                _ = interval.tick() => {
                    ticks += 1;
                }
                _ = events.next() => {
                    tokio::task::yield_now().await;
                }
            }
        }

        assert!(
            ticks >= 3,
            "background interval ticks were starved; observed {ticks}"
        );
    }

    #[test]
    fn test_ctrl_c_state_single_press() {
        let mut state = DoublePressState::new(3000);

        // First press returns false
        assert!(!state.on_pressed());
        assert_eq!(state.press_count, 1);
        assert!(state.first_press_time.is_some());
    }

    #[test]
    fn test_ctrl_c_state_double_press_within_timeout() {
        let mut state = DoublePressState::new(3000);

        // First press
        assert!(!state.on_pressed());

        // Second press after short time
        thread::sleep(std::time::Duration::from_millis(100));
        assert!(state.on_pressed());
        assert_eq!(state.press_count, 2);
    }

    #[test]
    fn test_ctrl_c_state_double_press_after_timeout() {
        let mut state = DoublePressState::new(3000);

        // First press
        assert!(!state.on_pressed());

        // Press after more than 3 seconds (treated as new first press)
        thread::sleep(std::time::Duration::from_secs(4));
        assert!(!state.on_pressed());
        assert_eq!(state.press_count, 1);
    }

    #[test]
    fn test_ctrl_c_state_reset() {
        let mut state = DoublePressState::new(3000);

        // First press
        assert!(!state.on_pressed());

        // Reset
        state.reset();
        assert_eq!(state.press_count, 0);
        assert!(state.first_press_time.is_none());

        // Press after reset is treated as first press
        assert!(!state.on_pressed());
        assert_eq!(state.press_count, 1);
    }

    #[test]
    fn command_is_valid_detects_builtin_and_alias() {
        let env = Environment::new();
        {
            let mut writer = env.write();
            writer.alias.insert("ll".to_string(), "ls -al".to_string());
        }

        let mut shell = Shell::new(env.clone());
        let repl = Repl::new(&mut shell);

        assert!(
            repl.command_is_valid("cd"),
            "built-in command should be valid"
        );
        assert!(repl.command_is_valid("ll"), "alias should be valid");
        assert!(
            !repl.command_is_valid("definitely_not_a_command_42"),
            "unknown command should not be valid"
        );

        drop(repl);
    }
}

/// Helper function to render the transient prompt
/// Extracted for testability
pub(crate) fn render_transient_prompt_to<W: Write>(
    out: &mut W,
    input: &Input,
    input_width: usize,
    prompt_width: usize,
    cols: u16,
) -> Result<()> {
    use crossterm::style::Stylize;

    // Calculate how many lines the prompt+input occupies
    // Note: Preprompt is always one extra line above
    let input_lines = (prompt_width + input_width) / (cols as usize);
    let total_lines = 1 + input_lines; // +1 for preprompt

    queue!(
        out,
        cursor::Hide,
        cursor::MoveToColumn(0),
        cursor::MoveUp(total_lines as u16),
        terminal::Clear(ClearType::FromCursorDown)
    )
    .ok();

    // Print transient prompt symbol (Green ❯)
    // We use write! instead of print! to support the generic writer
    queue!(out, Print("❯".green()), Print(" ")).ok();

    // Render the input with existing syntax highlighting
    input.print(out, None);

    queue!(out, cursor::Show).ok();
    out.flush().ok();
    Ok(())
}

#[cfg(test)]
mod ai_tests {

    use crate::ai_features::AiService;
    use crate::repl::Repl;
    use crate::shell::Shell;
    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::Value; // Add missing imports if needed
    use std::sync::Arc;

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
    async fn test_perform_auto_fix_success() {
        use crate::environment::Environment;

        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        // Setup mock AI service
        let service = Arc::new(MockAiService::new("ls -la"));
        repl.ai_service = Some(service);

        // Setup failed state
        repl.last_command_string = "lss -la".to_string();
        repl.last_status = 127;

        repl.perform_auto_fix().await;

        assert_eq!(repl.input.to_string(), "ls -la");
    }

    #[tokio::test]
    async fn test_perform_smart_commit_success() {
        use crate::environment::Environment;

        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        // Setup mock AI service
        let service = Arc::new(MockAiService::new("feat: initial commit"));
        repl.ai_service = Some(service);

        let diff = "diff --git a/foo b/foo...";
        repl.perform_smart_commit_logic(diff).await;

        assert_eq!(
            repl.input.to_string(),
            "git commit -m \"feat: initial commit\""
        );
    }
}

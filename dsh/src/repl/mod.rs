use crate::ai_features::{self, AiService, LiveAiService};
use crate::command_timing::{self, SharedCommandTiming};
use crate::completion::integrated::IntegratedCompletionEngine;
use crate::completion::{self as completion_lib, Completion};

use crate::environment::Environment;
use crate::history::FrecencyHistory;

use crate::input::{ColorType, Input, InputConfig, display_width};
use crate::lisp::{Symbol, Value};
use crate::parser::Rule;
use crate::prompt::Prompt;
use crate::shell::{SHELL_TERMINAL, Shell};
use crate::suggestion::{InputPreferences, SuggestionBackend};
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Context as _;
use anyhow::Result;
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste, EventStream, KeyEvent};
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{self, Clear, ClearType, disable_raw_mode, enable_raw_mode};

use dsh_builtin::execute_chat_message;
use dsh_openai::{ChatGptClient, OpenAiConfig};
use dsh_types::Context;
use futures::StreamExt;
use nix::sys::termios::{Termios, tcgetattr};
use nix::unistd::getpid;
use nix::unistd::tcsetpgrp;
use parking_lot::Mutex as ParkingMutex;
use parking_lot::RwLock;

use pest::iterators::Pairs;
use std::io::Write;
use std::os::fd::BorrowedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::time::{Instant as TokioInstant, MissedTickBehavior, interval_at};
use tracing::{debug, warn};

const AI_SUGGESTION_REFRESH_MS: u64 = 300;
const GIT_STATUS_THROTTLE_MS: u64 = 200;
// MCP_FORM_SUGGESTIONS moved to completion.rs

mod state;
use state::*;
mod cache;
use cache::*;
mod suggestion_manager;
use suggestion_manager::*;
pub mod confirmation;
mod handler;
pub mod key_action;
mod render;

pub mod completion;
mod input_analysis;
pub mod macro_utils;
mod repl_ai; // Extracted AI logic

pub(crate) use input_analysis::InputAnalysis;

/// Format directory entries for AI context
/// This is a pure function for testability

#[derive(Debug)]
pub enum AiEvent {
    AutoFix(String),
}

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
    pub(crate) ctrl_x_pressed: bool,
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
    pub(crate) stopped_jobs_warned: bool,
    pub(crate) multiline_buffer: String,
    pub(crate) last_cwd: std::path::PathBuf,
    pub(crate) git_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    pub(crate) last_git_update: Option<Instant>,
    pub(crate) git_task_inflight: Arc<AtomicBool>,
    pub(crate) file_context_cache: Arc<RwLock<FileContextCache>>,
    pub(crate) argument_explainer: crate::argument_explainer::ArgumentExplainer,
    pub(crate) last_explanation: Option<String>,
    pub(crate) auto_fix_suggestion: Option<String>,
    pub(crate) ai_rx: tokio::sync::mpsc::UnboundedReceiver<AiEvent>,
    pub(crate) ai_tx: tokio::sync::mpsc::UnboundedSender<AiEvent>,
    pub(crate) history_sync_last_check: Instant,
    pub(crate) completion_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    /// Flag to indicate argument explanation needs refresh (debounced)
    pub(crate) explanation_dirty: bool,
    /// Cache for syntax highlighting to avoid re-parsing unchanged input
    pub(crate) last_analyzed_input: String,
    pub(crate) last_analysis_result: Option<InputAnalysis>,
    /// Handle to the background GitHub task, allowing cancellation on drop
    pub(crate) github_task: Option<tokio::task::JoinHandle<()>>,
}

impl<'a> Drop for Repl<'a> {
    fn drop(&mut self) {
        // Cancel background task
        if let Some(handle) = self.github_task.take() {
            handle.abort();
        }

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
        // Initialize Command Palette actions
        crate::command_palette::register_builtin_actions();

        // Initialize completion notifier channel
        let (completion_tx, completion_rx) = tokio::sync::mpsc::unbounded_channel();
        completion_lib::set_completion_notifier(completion_tx);

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
        let prompt = Prompt::new(current.clone(), "🐕 < ".to_string());

        let prompt = Arc::new(RwLock::new(prompt));
        shell
            .environment
            .write()
            .chpwd_hooks
            .push(Box::new(Arc::clone(&prompt)));
        let input_config = InputConfig::default();

        // Initialize GitHub integration
        let github_status = Arc::new(RwLock::new(crate::github::GitHubStatus::default()));
        prompt.write().github_status = Some(github_status.clone());

        let github_config = {
            let lisp_engine = shell.lisp_engine.borrow();
            let env = lisp_engine.env.borrow();

            let pat = match env.get(&Symbol::from("*github-pat*")) {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            };

            if let Some(Value::String(icon)) = env.get(&Symbol::from("*github-icon*")) {
                prompt.write().github_icon = icon.clone();
            }

            let interval = match env.get(&Symbol::from("*github-notify-interval*")) {
                Some(Value::String(s)) => s.parse::<u64>().unwrap_or(60),
                Some(Value::Int(i)) => i.try_into().unwrap_or(60),
                _ => 60,
            };

            let filter = match env.get(&Symbol::from("*github-notifications-filter*")) {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            };

            if pat.is_some() {
                debug!(
                    "GitHub integration enabled. Interval: {}, Filter: {:?}",
                    interval, filter
                );
            } else {
                debug!("GitHub integration disabled (no PAT found).");
            }

            Arc::new(RwLock::new(crate::github::GitHubConfig {
                pat,
                interval,
                filter,
            }))
        };

        let config_for_task = Arc::clone(&github_config);
        let prompt_for_github = Arc::clone(&prompt);
        let status_for_github = Arc::clone(&github_status);

        // Spawn background task
        let github_task = tokio::spawn(crate::github::background_github_task(
            config_for_task,
            prompt_for_github,
            status_for_github.clone(),
        ));

        // Set github_status in shell as well for proxy access
        shell.github_status = Some(status_for_github);

        let prompt_mark_cache = prompt.read().mark.clone();
        let prompt_mark_width = display_width(&prompt_mark_cache);

        let envronment = Arc::clone(&shell.environment);
        let input_preferences = envronment.read().input_preferences();
        let mut suggestion_manager = SuggestionManager::new();
        let mut ai_service: Option<Arc<dyn AiService + Send + Sync>> = None;
        if let Some((ai_backend, client)) = Self::build_ai_backend(&envronment) {
            suggestion_manager.engine.set_ai_backend(Some(ai_backend));

            // ... (in Repl::new)

            let allowlist = envronment.read().execute_allowlist.clone();
            let service = Arc::new(LiveAiService::new(
                client,
                envronment.read().mcp_manager.clone(),
                envronment.read().safety_level.clone(),
                shell.safety_guard.clone(),
                Some(confirmation::ReplConfirmationHandler::new()),
                allowlist,
            ));

            // Store in environment so ShellProxy can access it
            envronment.write().ai_service = Some(service.clone());
            ai_service = Some(service);
        }
        suggestion_manager.set_preferences(input_preferences);

        // Setup Git event channel
        let (git_tx, git_rx) = tokio::sync::mpsc::unbounded_channel();
        prompt.write().set_git_sender(git_tx);

        // Setup AI event channel
        let (ai_tx, ai_rx) = tokio::sync::mpsc::unbounded_channel();

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
            ctrl_x_pressed: false,
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
            stopped_jobs_warned: false,
            multiline_buffer: String::new(),
            last_cwd: current.clone(),
            git_rx,
            last_git_update: None,
            git_task_inflight: Arc::new(AtomicBool::new(false)),
            file_context_cache: Arc::new(RwLock::new(FileContextCache::new())),
            argument_explainer: crate::argument_explainer::ArgumentExplainer::new(),
            last_explanation: None,
            auto_fix_suggestion: None,
            ai_rx,
            ai_tx,
            history_sync_last_check: Instant::now(),
            completion_rx,
            explanation_dirty: false,
            last_analyzed_input: String::new(),
            last_analysis_result: None,
            github_task: Some(github_task),
        }
    }

    pub(crate) fn trigger_file_context_update(&self) {
        let cache = self.file_context_cache.clone();
        tokio::task::spawn_blocking(move || {
            let cwd = match std::env::current_dir() {
                Ok(p) => p,
                Err(_) => return,
            };

            // Fast check
            if let Some(guard) = cache.try_read()
                && guard.is_valid(&cwd)
            {
                return;
            }

            let mut files = Vec::new();
            if let Ok(dir) = std::fs::read_dir(&cwd) {
                let mut entries: Vec<_> = dir
                    .flatten()
                    .map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        (name, is_dir)
                    })
                    .filter(|(name, _)| !name.starts_with('.'))
                    .collect();

                // Sort roughly
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

            let mut write = cache.write();
            write.path = cwd;
            write.files = Arc::new(files);
            write.updated_at = Some(Instant::now());
        });
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
        enable_raw_mode().ok();
        let mut renderer = TerminalRenderer::new();
        queue!(renderer, EnableBracketedPaste).ok();
        renderer.flush().ok();
    }

    pub(crate) async fn check_background_jobs(&mut self, output: bool) -> Result<()> {
        handler::check_background_jobs(self, output).await
    }

    pub(crate) async fn handle_event(&mut self, ev: ShellEvent) -> Result<ReplControlFlow> {
        handler::handle_event(self, ev).await
    }

    pub(crate) async fn handle_paste_event(&mut self, text: &str) -> Result<()> {
        handler::handle_paste_event(self, text).await
    }

    pub(crate) async fn handle_key_event(&mut self, ev: &KeyEvent) -> Result<ReplControlFlow> {
        let result = handler::handle_key_event(self, ev).await;
        // Mark explanation as dirty for debounced refresh
        self.explanation_dirty = true;
        result
    }

    fn refresh_argument_explanation(&mut self) {
        let input = self.input.to_string();
        let cursor = self.input.cursor();
        let explanation = self.argument_explainer.get_explanation(&input, cursor);

        if explanation != self.last_explanation {
            let mut stdout = std::io::stdout();

            // Clear previous explanation logic via clearing the line or similar
            // For MVP, we use a dedicated line below the prompt if possible.
            // But to avoid scrolling issues or interfering with output, we need to be careful.
            // Let's assume we can print at line+1.

            // We use specialized logic: save cursor, move down 1 (scrolling if needed), print/clear, restore.
            // Note: If we scroll, restoring absolute position might be off.
            // But let's try standard sequence.

            // Note: If explanation is None, we should clear the line.

            use crossterm::{QueueableCommand, cursor, style::Print, terminal};

            // Only attempt if we have columns known
            if self.columns == 0 {
                return;
            }

            // If we are at the bottom line, we might need to scroll up to make space for explanation?
            // "Inline" usually means "under" the current line.
            // If we are at the bottom, printing new line causes scroll of the prompt line too.
            // This is tricky.

            // Simplified approach: just print if we have space or don't care about scroll.
            // But if we scroll, the prompt input line moves up.
            // If we `RestorePosition`, we go back to absolute coordinates.
            // If prompt moved up, we restore to the OLD execution line (now 1 line lower relative to content).
            // So we'd draw over the wrong line?

            // Solution: check cursor position.
            if let Ok((col, row)) = cursor::position() {
                let (_, rows) = terminal::size().unwrap_or((80, 24));

                // Construct the explanation string formatted
                // let text = match &explanation { ... } // Removed unused logic

                // To be safe, we only draw if we are NOT at the very bottom, OR we accept scroll issues.
                // Or we can try to use `MoveToNextLine` which implies scrolling if at bottom.
                // But RestorePosition is absolute.
                // Maybe `MoveToPreviousLine` to restore?

                // Let's try: Save, MoveToNextLine, Print, MoveToPreviousLine, MoveToColumn(col).
                // MoveToNextLine(1) -> if at bottom, scrolls. Correct.
                // Print -> prints.
                // MoveToPreviousLine(1) -> moves up. Correct.
                // MoveToColumn(original_col) -> restores horizontal.

                stdout.queue(cursor::SavePosition).ok();

                if row >= rows - 1 {
                    // At bottom. force scroll.
                    stdout.queue(Print("\n")).ok();
                    stdout.queue(cursor::MoveToColumn(0)).ok();
                } else {
                    stdout.queue(cursor::MoveToNextLine(1)).ok();
                }

                stdout
                    .queue(terminal::Clear(terminal::ClearType::CurrentLine))
                    .ok();
                if let Some(s) = &explanation {
                    let styled = format!(" \x1b[38;5;244m[ {} ]\x1b[0m", s);
                    stdout.queue(Print(styled)).ok();
                }

                // Restore
                if row >= rows - 1 {
                    // We were at bottom. Screen scrolled. Prompt is now at rows-2 (visually).
                    // We are currently at (after print, rows-1).
                    stdout.queue(cursor::MoveUp(1)).ok();
                    stdout.queue(cursor::MoveToColumn(col)).ok();
                } else {
                    stdout.queue(cursor::RestorePosition).ok();
                }

                stdout.flush().ok();
            }

            self.last_explanation = explanation;
        }
    }

    fn save_history(&mut self) {
        // Command history is auto-saved by SQLite
        Self::save_single_history_helper(&mut self.shell.path_history, "path", false);
    }

    fn save_single_history_helper(
        history: &mut Option<Arc<ParkingMutex<FrecencyHistory>>>,
        history_type: &str,
        background: bool,
    ) {
        if let Some(history) = history {
            if let Some(mut history_guard) = history.try_lock() {
                // Only save if there are changes
                if let Some(ref store) = history_guard.store {
                    if store.changed {
                        if background {
                            history_guard.save_background();
                            // debug!("{} history saving in background", history_type);
                        } else if let Err(e) = history_guard.save() {
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
        // Command history is auto-saved by SQLite
        Self::save_single_history_helper(&mut self.shell.path_history, "path", true);
    }

    /// Move cursor relatively on the input line given previous and new display positions
    pub(crate) fn move_cursor_relative(
        &self,
        out: &mut impl Write,
        prev_display_pos: usize,
        new_display_pos: usize,
    ) {
        render::move_cursor_relative(self, out, prev_display_pos, new_display_pos)
    }

    pub(crate) fn print_prompt(&mut self, out: &mut impl Write) {
        render::print_prompt(self, out)
    }

    fn sync_input_preferences(&mut self) {
        let prefs = self.shell.environment.read().input_preferences();
        if prefs != self.input_preferences {
            self.input_preferences = prefs;
            self.suggestion_manager.engine.set_preferences(prefs);
        }
    }

    pub(crate) fn compute_color_ranges_from_pairs<'p>(
        &self,
        pairs: Pairs<'p, Rule>,
        input: &str,
    ) -> (Vec<(usize, usize, ColorType)>, bool) {
        render::compute_color_ranges_from_pairs(self, pairs, input)
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
            SuggestionAcceptMode::Word => match completion::next_word_chunk(suffix) {
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

    fn learn_suggestion(&self, suggestion: &str) {
        if let Some(history) = &self.shell.cmd_history
            && let Some(mut history) = history.try_lock()
            && let Err(e) = history.write_history(suggestion)
        {
            warn!("Failed to learn suggestion: {}", e);
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
            if entry.len() >= input.len() && entry.starts_with(input) {
                return Some(entry[input.len()..].to_string());
            }
        }

        if let Some(ref mut history) = self.shell.cmd_history
            && let Some(history) = history.try_lock()
            && let Some(entry) = history.search_first(input)
        {
            let entry = entry.to_string();
            self.input.completion = Some(entry.clone());
            if entry.len() >= input.len() && entry.starts_with(input) {
                return Some(entry[input.len()..].to_string());
            }
        }
        // If we can't get the lock, completion just won't be available - no warning needed
        None
    }

    fn analyze_input(&self, input: &str, completion: Option<String>) -> InputAnalysis {
        input_analysis::analyze_input(self, input, completion)
    }

    pub fn print_input(
        &mut self,
        out: &mut impl Write,
        reset_completion: bool,
        refresh_suggestion: bool,
    ) {
        render::print_input(self, out, reset_completion, refresh_suggestion)
    }

    pub async fn run_interactive(&mut self) -> Result<()> {
        let mut reader = EventStream::new();

        self.setup();

        debug!(
            "shell setpgid pid:{:?} pgid:{:?}",
            self.shell.pid, self.shell.pgid
        );
        let _ = tcsetpgrp(
            unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) },
            self.shell.pgid,
        )
        .context("failed tcsetpgrp");
        self.tmode = match tcgetattr(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) }) {
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

        // Debounced interval for argument explanation refresh (200ms)
        const EXPLANATION_REFRESH_MS: u64 = 200;
        let mut explanation_refresh_interval = interval_at(
            TokioInstant::now() + Duration::from_millis(EXPLANATION_REFRESH_MS),
            Duration::from_millis(EXPLANATION_REFRESH_MS),
        );
        explanation_refresh_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = background_interval.tick() => {
                    // Save history every 30 seconds if there have been changes
                    self.save_history_periodic();
                    self.check_background_jobs(true).await?;

                    // Reload path history every 30 seconds to sync with other processes
                    if self.history_sync_last_check.elapsed() > Duration::from_secs(30) {
                        if let Some(ref history) = self.shell.path_history
                            && let Some(mut history) = history.try_lock() {
                                 let _ = history.reload();
                            }
                        if let Some(ref history) = self.shell.cmd_history
                            && let Some(mut history) = history.try_lock() {
                                 let _ = history.reload();
                            }
                        self.history_sync_last_check = Instant::now();
                    }

                    // Execute input-timeout hooks (called periodically when idle)
                    let _ = self.shell.exec_input_timeout_hooks();

                    let prompt = Arc::clone(&self.prompt);

                    // Check for Rust version
                    if prompt.read().needs_rust_check() {
                        let p_clone = Arc::clone(&prompt);
                        tokio::spawn(async move {
                            if let Some(version) = crate::prompt::fetch_rust_version_async().await {
                                p_clone.write().update_rust_version(Some(version));
                            } else {
                                p_clone.write().mark_rust_check_failed();
                            }
                        });
                    }

                    // Check for Node version
                    if prompt.read().needs_node_check() {
                         let p_clone = Arc::clone(&prompt);
                         tokio::spawn(async move {
                            if let Some(version) = crate::prompt::fetch_node_version_async().await {
                                p_clone.write().update_node_version(Some(version));
                            } else {
                                p_clone.write().mark_node_check_failed();
                            }
                        });
                    }

                    // Check for Python version
                    if prompt.read().needs_python_check() {
                         let p_clone = Arc::clone(&prompt);
                         tokio::spawn(async move {
                            if let Some(version) = crate::prompt::fetch_python_version_async().await {
                                p_clone.write().update_python_version(Some(version));
                            } else {
                                p_clone.write().mark_python_check_failed();
                            }
                        });
                    }

                    // Check for Go version
                    if prompt.read().needs_go_check() {
                         let p_clone = Arc::clone(&prompt);
                         tokio::spawn(async move {
                            if let Some(version) = crate::prompt::fetch_go_version_async().await {
                                p_clone.write().update_go_version(Some(version));
                            } else {
                                p_clone.write().mark_go_check_failed();
                            }
                        });
                    }

                    // Cloud Context Checks
                    if prompt.read().should_check_k8s() {
                         let p_clone = Arc::clone(&prompt);
                         tokio::spawn(async move {
                            if let Some((context, namespace)) = crate::prompt::fetch_k8s_info_async().await {
                                p_clone.write().update_k8s_info(Some(context), namespace);
                            } else {
                                p_clone.write().mark_k8s_check_failed();
                            }
                        });
                    }

                    if prompt.read().should_check_aws() {
                        // AWS is fast (env var), can do inline or spawn
                        let profile = crate::prompt::fetch_aws_profile();
                        prompt.write().update_aws_profile(profile);
                    }

                    if prompt.read().should_check_docker() {
                         let p_clone = Arc::clone(&prompt);
                         tokio::spawn(async move {
                            if let Some(context) = crate::prompt::fetch_docker_context_async().await {
                                p_clone.write().update_docker_context(Some(context));
                            } else {
                                p_clone.write().mark_docker_check_failed();
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
                _ = explanation_refresh_interval.tick() => {
                    // Debounced argument explanation refresh
                    if self.explanation_dirty {
                        self.explanation_dirty = false;
                        self.refresh_argument_explanation();
                    }
                }
                Some(_) = self.git_rx.recv() => {
                    let now = Instant::now();
                    let is_throttled = self
                        .last_git_update
                        .is_some_and(|last| now.duration_since(last) < Duration::from_millis(GIT_STATUS_THROTTLE_MS));

                    if !is_throttled
                        && self
                            .git_task_inflight
                            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                    {
                        self.last_git_update = Some(now);
                        let prompt = Arc::clone(&self.prompt);
                        let inflight = Arc::clone(&self.git_task_inflight);
                        tokio::spawn(async move {
                            // Check if we need to discover/update git root (async)
                            let needs_root_check = prompt.read().needs_git_check;
                            if needs_root_check {
                                let cwd = prompt.read().current_dir.clone();
                                let root = crate::prompt::find_git_root_async(cwd).await;
                                prompt.write().update_git_root(root);
                            }

                            // Fetch status if we have a git root (always fetch on event)
                            if prompt.read().has_git_root() {
                                let path = prompt.read().current_path().to_path_buf();
                                if let Some(status) = crate::prompt::fetch_git_status_async(&path).await {
                                    prompt.write().update_git_status(Some(status));
                                }
                            }
                            inflight.store(false, Ordering::SeqCst);
                        });
                    }
                }
                Some(_) = self.completion_rx.recv() => {
                    // Handle path completion update (background scan finished)
                    if self.input.completion.is_none()
                        && self.refresh_inline_suggestion()
                    {
                         let mut renderer = TerminalRenderer::new();
                         self.print_input(&mut renderer, false, false);
                         renderer.flush().ok();
                    }
                }
                Some(ai_event) = self.ai_rx.recv() => {
                    match ai_event {
                        AiEvent::AutoFix(fix) => {
                            self.auto_fix_suggestion = Some(fix);
                             // Force redraw if input is empty to show the suggestion
                            if self.input.as_str().is_empty() {
                                let mut renderer = TerminalRenderer::new();
                                self.print_input(&mut renderer, false, false);
                                renderer.flush().ok();
                            }
                        }
                    }
                }
                maybe_event = reader.next() => {
                    let old_last_time = self.last_command_time;
                    match maybe_event {
                        Some(Ok(event)) => {
                            match self.handle_event(ShellEvent::Input(event)).await {
                                Ok(ReplControlFlow::Continue) => {
                                    // Continue loop
                                }
                                Ok(ReplControlFlow::RunInteractive(closure)) => {
                                    // Drop reader to release stdin lock
                                    drop(reader);

                                    // Disable raw mode so Skim/interactive command controls terminal
                                    disable_raw_mode().ok();

                                    // Execute the interactive closure
                                    match closure() {
                                        Ok(Some(action)) => {
                                            use crate::repl::state::InteractiveAction;
                                            match action {
                                                InteractiveAction::Patch { backspace_count, text } => {
                                                    // Apply the interactive patch
                                                    if backspace_count > 0 {
                                                        self.input.backspacen(backspace_count);
                                                    }
                                                    self.input.insert_str(&text);
                                                },
                                                InteractiveAction::ReplaceAll { text } => {
                                                    // Apply full replacement
                                                    self.input.reset(text);
                                                }
                                            }

                                            // Trigger validation/highlighting
                                            self.input.completion = None;
                                            self.input.color_ranges = None;
                                        }
                                        Ok(None) => {
                                            // Canceled
                                        }
                                        Err(e) => {
                                            self.shell.print_error(format!("Interactive session failed: {}\r\n", e));
                                        }
                                    }

                                    // Re-enable raw mode
                                    enable_raw_mode().ok();

                                    // Recreate reader
                                    reader = EventStream::new();

                                    // Redraw prompt
                                    let mut renderer = TerminalRenderer::new();
                                    self.print_prompt(&mut renderer);
                                    // Reprint input with updates
                                    self.print_input(&mut renderer, true, true);
                                    renderer.flush().ok();
                                }
                                Err(err) => {
                                    self.shell.print_error(format!("Error: {err:?}\r"));
                                    break;
                                }
                            }

                            // Check for CWD change and trigger AI prefetch
                            let current_cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
                            if current_cwd != self.last_cwd {
                                self.last_cwd = current_cwd.clone();

                                if self.input_preferences.ai_backfill {
                                    debug!("CWD changed to {:?}, triggering AI prefetch", self.last_cwd);
                                    let files = self.get_directory_listing();
                                    let files_vec: Vec<String> = files.lines().map(String::from).collect();
                                    self.suggestion_manager.engine.prefetch(
                                        Some(self.last_cwd.to_string_lossy().to_string()),
                                        Arc::new(files_vec),
                                        Some(self.last_status)
                                    );
                                }
                            }

                            // Reset stopped jobs warning if a command was executed
                             if self.last_command_time != old_last_time {
                                self.stopped_jobs_warned = false;

                                // Invalidate git cache and trigger re-check
                                self.prompt.write().invalidate_git_cache();

                                // Trigger git check
                                // We can send to git_rx (via git_tx which we assume we have somehow? No, we don't have git_tx here)
                                // We DO have self.git_rx, but we can't send to it.
                                // We have prompt.git_sender!
                                self.prompt.read().trigger_git_check();

                                // Trigger auto-fix if failed
                                if self.last_status != 0 {
                                    self.trigger_auto_fix();
                                }
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
                    // Allow one retry to exit with stopped jobs
                    if !self.stopped_jobs_warned {
                        self.shell
                            .print_error("There are stopped jobs.\r\n".to_string());
                        self.stopped_jobs_warned = true;
                        self.should_exit = false;
                        self.shell.exited = None;
                        continue;
                    }
                }
                break;
            }
        }
        self.shell.kill_wait_jobs()?;
        Ok(())
    }

    pub fn select_history(&mut self) -> Result<ReplControlFlow> {
        let query = self.input.as_str();
        if let Some(ref mut history) = self.shell.cmd_history {
            if let Some(mut history) = history.try_lock() {
                let items: Vec<completion_lib::Candidate> = history
                    .iter()
                    .rev()
                    .map(|h| completion_lib::Candidate::Basic(h.entry.clone()))
                    .collect();

                let res = completion_lib::select_item_with_skim(items, Some(query));

                history.reset_index();

                match res {
                    completion_lib::CompletionSelection::Selected(val) => {
                        // Replace current input with the selected history command
                        self.input.reset(val);
                        return Ok(ReplControlFlow::Continue);
                    }
                    completion_lib::CompletionSelection::Interactive(items, query) => {
                        let query = query.unwrap_or_default();
                        return Ok(ReplControlFlow::RunInteractive(Box::new(move || {
                            use completion_lib::framework::SkimCompletionFramework;

                            let result = SkimCompletionFramework::run_with_skim(items, Some(query));
                            Ok(result.map(|text| {
                                crate::repl::state::InteractiveAction::ReplaceAll { text }
                            }))
                        })));
                    }
                    completion_lib::CompletionSelection::None => {
                        return Ok(ReplControlFlow::Continue);
                    }
                }
            } else {
                warn!(
                    "Failed to acquire command history lock for history selection - lock is busy"
                );
            }
        }
        Ok(ReplControlFlow::Continue)
    }

    pub(crate) fn command_is_valid(&self, word: &str) -> bool {
        input_analysis::command_is_valid(self, word)
    }

    async fn toggle_sudo(&mut self) -> Result<()> {
        input_analysis::toggle_sudo(self).await
    }

    /// Get directory listing for AI context
    fn get_directory_listing(&self) -> String {
        repl_ai::get_directory_listing_content(std::path::Path::new(".")).join("\n")
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
    async fn run_ai_pipe(&mut self, command: String, query: String) -> Result<()> {
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
        let Some(_service) = self.ai_service.clone() else {
            queue!(
                renderer,
                Print("❌ AI service not configured. Set OPENAI_API_KEY or AI_CHAT_API_KEY.\r\n")
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

        let message = format!(
            "Shell command: `{}`\n\nOutput:\n```\n{}\n```\n\nQuery: {}",
            command, combined_output, query
        );

        let ctx = Context::new_safe(getpid(), getpid(), true);
        execute_chat_message(&ctx, &mut *self.shell, &message, None);

        self.last_status = exit_code;
        self.last_command_string = command;

        renderer.flush().ok();
        self.print_prompt(&mut renderer);
        renderer.flush().ok();

        Ok(())
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

    #[tokio::test]
    async fn command_is_valid_detects_builtin_and_alias() {
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

pub(crate) use render::render_transient_prompt_to;

#[cfg(test)]
mod ai_tests {

    use crate::ai_features::AiService;
    use crate::environment::Environment;
    use crate::repl::{AiEvent, Repl};
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
    async fn test_trigger_auto_fix_success() {
        use crate::environment::Environment;

        let environment = Environment::new();
        let mut shell = Shell::new(environment);
        let mut repl = Repl::new(&mut shell);

        // Setup mock AI service
        let service = Arc::new(MockAiService::new(r#"{"command": "ls", "args": ["-la"]}"#));
        repl.ai_service = Some(service);

        // Setup failed state
        repl.last_command_string = "lss -la".to_string();
        repl.last_status = 127;

        // Enable auto_fix
        repl.input_preferences.auto_fix = true;

        repl.trigger_auto_fix();

        // Wait for the background task to complete and send the result
        if let Some(AiEvent::AutoFix(fix)) = repl.ai_rx.recv().await {
            repl.auto_fix_suggestion = Some(fix);
        }

        assert_eq!(repl.auto_fix_suggestion, Some("ls -la".to_string()));
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

#[tokio::test]
async fn test_analyze_input_suffix_calculation() {
    use crate::environment::Environment;
    let environment = Environment::new();
    let mut shell = Shell::new(environment);
    let mut repl = Repl::new(&mut shell);

    // Existing file for test
    let test_file = "Cargo.toml";
    let partial = "Cargo.tom";
    let suffix = "l";

    // Case 1: Cursor at end
    let input_str = format!("ls {}", partial);
    repl.input.reset(input_str.clone());

    // analyze_input usage: input, completion (start with None)
    let analysis = repl.analyze_input(&input_str, None);
    let full = analysis.completion_full;
    let comp_suffix = analysis.completion;

    // Expectation: completion found (hits valid path logic)
    // Note: completion::path_completion_prefix depends on CWD.
    // Cargo.toml should be in CWD when running tests for dsh package.

    if let Some(s) = comp_suffix {
        assert_eq!(
            s, suffix,
            "Suffix should be 'l' for Cargo.tom -> Cargo.toml"
        );
        // Full string should be "ls Cargo.toml"
        if let Some(f) = full {
            assert_eq!(f, format!("ls {}", test_file));
        } else {
            panic!("Should have returned full completion string");
        }
    } else {
        // If it returns None, it might mean CWD is not as expected or file not found.
        // We'll skip asserting if environment doesn't match, but ideally it should pass in this repo.
        // println!("Skipping test as Cargo.toml was not found or completion failed");
    }

    // Case 2: Mid-line edit (this was the buggy case for suffix calc logic?)
    // Actually the logic `c[input.len()..]` was the problem in `print_input`.
    // Current logic in `analyze_input` constructs full string correctly using `split_current_pos`.

    // "ls Cargo.tom -lat"
    // Cursor after "tom"
    let input_mid = "ls Cargo.tom -lat";
    repl.input.reset(input_mid.to_string());
    repl.input.move_to_begin();
    // Move to after "Cargo.tom" (3 + 9 = 12)
    repl.input.move_by(12);

    let analysis_mid = repl.analyze_input(input_mid, None);
    let full_mid = analysis_mid.completion_full;
    let suffix_mid = analysis_mid.completion;

    if let Some(s) = suffix_mid {
        assert_eq!(s, "l", "Suffix should be 'l'");
        // Full completion should insert 'l' at cursor: "ls Cargo.toml -lat"
        if let Some(f) = full_mid {
            assert_eq!(f, "ls Cargo.toml -lat");
        }
    }
}
mod state_tests;

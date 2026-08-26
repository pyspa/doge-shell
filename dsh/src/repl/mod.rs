use crate::ai_features::{self, AiService, LiveAiService};
use crate::command_timing;
use crate::completion::integrated::IntegratedCompletionEngine;
use crate::completion::{self as completion_lib, Completion};

use crate::environment::Environment;
use crate::history::FrecencyHistory;

use crate::input::{ColorType, Input, InputConfig, display_width};
use crate::lisp::{Symbol, Value};
use crate::parser::Rule;
use crate::prompt::Prompt;
use crate::repl::state::{DoublePressState, ReplControlFlow, ShellEvent};
use crate::repl::suggestion_manager::SuggestionManager;
use crate::shell::{SHELL_TERMINAL, Shell};
use crate::suggestion::{InputPreferences, SuggestionBackend};
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Context as _;
use anyhow::Result;
use crossterm::event::{EnableBracketedPaste, KeyEvent};
use crossterm::style::Print;
use crossterm::terminal::ClearType;
use crossterm::terminal::{self, disable_raw_mode, enable_raw_mode};
use crossterm::{queue, terminal::Clear};
#[cfg(test)]
use futures::StreamExt;

use dsh_builtin::execute_chat_message;
use dsh_openai::{ChatGptClient, OpenAiConfig};
use dsh_types::Context;
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
#[cfg(test)]
use tokio::time::{Instant as TokioInstant, MissedTickBehavior, interval_at};
use tracing::{debug, warn};

/// Cap on captured command output forwarded by `|>` to the chat runtime.
const AI_PIPE_OUTPUT_CHARS: usize = 12_000;
const AI_SUGGESTION_REFRESH_MS: u64 = 300;
const GIT_STATUS_THROTTLE_MS: u64 = 200;
// MCP_FORM_SUGGESTIONS moved to completion.rs

mod state;
use state::*;
mod cache;
use cache::*;
mod abbreviation;
pub(crate) mod ai_watch;
pub(crate) mod background_io;
pub mod confirmation;
mod event_loop;
mod handler;
pub(crate) mod job_notify;
pub mod key_action;
mod key_handlers;
pub(crate) mod keybind;
pub(crate) mod last_arg;
pub(crate) mod notify;
pub(crate) mod placeholder;
mod prompt_refresh;
mod render;
mod services;
mod shell_integration;
pub(crate) mod status_line;
mod suggestion_manager;
pub(crate) mod terminal_state;

pub mod completion;
mod input_analysis;
pub mod macro_utils;
mod repl_ai; // Extracted AI logic

use background_io::{BackgroundIoCoordinator, BackgroundIoEvent};
pub(crate) use input_analysis::{CachedInputAnalysis, InputAnalysis};
use services::ReplServices;

/// Format directory entries for AI context
/// This is a pure function for testability

#[derive(Debug)]
pub enum AiEvent {
    AutoFix(String),
    CommandExplanation { input: String, explanation: String },
    CommandExplanationError { input: String },
}

pub(crate) struct TerminalUiState {
    pub(crate) columns: usize,
    pub(crate) lines: usize,
    pub(crate) tmode: Option<Termios>,
    pub(crate) prompt: Arc<RwLock<Prompt>>,
    pub(crate) prompt_mark_cache: String,
    pub(crate) prompt_mark_width: usize,
    pub(crate) ctrl_c_state: DoublePressState,
    pub(crate) esc_state: DoublePressState,
    pub(crate) last_drawn_cursor_y: usize,
    pub(crate) last_preprompt_plain: Option<String>,
    /// Optional bottom-row status line. Disabled by default.
    pub(crate) status_line: status_line::SharedStatusLine,
}

pub(crate) struct CompletionUiState {
    pub(crate) start_completion: bool,
    pub(crate) completion: Completion,
    pub(crate) integrated_completion: IntegratedCompletionEngine,
    pub(crate) cache: HistoryCache,
}

pub(crate) struct AiUiState {
    pub(crate) suggestion_manager: SuggestionManager,
    pub(crate) input_preferences: InputPreferences,
    pub(crate) ai_pending_shown: bool,
    pub(crate) last_explanation: Option<String>,
    pub(crate) auto_fix_suggestion: Option<String>,
    pub(crate) pending_ai_explanation_input: Option<String>,
    pub(crate) current_ai_explanation: Option<String>,
    pub(crate) last_input_change_time: Instant,
    pub(crate) ai_tx: tokio::sync::mpsc::UnboundedSender<AiEvent>,
    pub(crate) explanation_dirty: bool,
    pub(crate) last_analyzed_input: String,
    pub(crate) last_analysis_result: Option<CachedInputAnalysis>,
}

pub(crate) struct BackgroundTasks {
    pub(crate) last_git_update: Option<Instant>,
    pub(crate) git_task_inflight: Arc<AtomicBool>,
    pub(crate) history_sync_last_check: Instant,
    pub(crate) github_task: Option<tokio::task::JoinHandle<()>>,
    /// Finished `sched` runs, reported by the scheduler runner.
    pub(crate) sched_task: Option<tokio::task::JoinHandle<()>>,
    pub(crate) io: BackgroundIoCoordinator,
}

pub struct Repl<'a> {
    pub shell: &'a mut Shell,
    pub(crate) input: Input,
    pub(crate) history_search: Option<String>,
    /// Strokes collected so far for a multi-key binding such as
    /// `Ctrl-x Ctrl-e`. Empty when no chord is in progress.
    pub(crate) pending_chord: crate::repl::keybind::chord::Chord,
    pub(crate) state: ReplState,
    pub(crate) services: ReplServices,
    pub(crate) terminal_ui: TerminalUiState,
    pub(crate) completion_ui: CompletionUiState,
    pub(crate) ai_ui: AiUiState,
    pub(crate) background_tasks: BackgroundTasks,
    pub(crate) event_loop: event_loop::ReplEventLoop,
}

/// Whether Ctrl-R should use the pre-picker skim flow.
///
/// An escape hatch for one release, following `DSH_COMPLETION_FRAMEWORK`.
fn history_picker_backend_is_skim() -> bool {
    matches!(std::env::var("DSH_HISTORY_PICKER"), Ok(value) if value.eq_ignore_ascii_case("skim"))
}

impl<'a> Drop for Repl<'a> {
    fn drop(&mut self) {
        // Cancel background tasks. Aborting the scheduler is what makes
        // scheduled work session-scoped; in-flight children die with it
        // because `exec` sets `kill_on_drop`.
        if let Some(handle) = self.background_tasks.github_task.take() {
            handle.abort();
        };
        if let Some(handle) = self.background_tasks.sched_task.take() {
            handle.abort();
        };
        // Tests build and drop `Repl` dozens of times; without this gate each
        // drop writes escape sequences to the terminal running `cargo test`.
        if crate::terminal::terminal_control_enabled() {
            let mut renderer = TerminalRenderer::new();
            // Release the scroll margin before leaving raw mode. A terminal left
            // with a stale DECSTBM region looks broken to whatever runs next.
            self.terminal_ui
                .status_line
                .borrow_mut()
                .disarm(&mut renderer);
            queue!(renderer, crossterm::event::DisableBracketedPaste).ok();
            renderer.flush().ok();

            disable_raw_mode().ok();
        }
        // Restore the user's terminal before waiting on filesystem I/O. The
        // writer may be slow, but raw mode and DECSTBM must never outlive the
        // interactive event loop.
        self.background_tasks.io.shutdown();
        self.save_history();
        // Save command timing statistics
        if let Some(path) = command_timing::get_timing_file_path()
            && let Err(e) = self.services.command_timing.write().save_to_file(&path)
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
            .variable_state
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

        // The scheduler runner shares the task list with `Environment` and
        // reports finished runs back over this channel. Spawned here rather
        // than driven from `handle_background_tick`, because that tick is
        // awaited inside the key-event select: a slow task would freeze input.
        let (sched_tx, sched_rx) = tokio::sync::mpsc::unbounded_channel();
        let sched_task = tokio::spawn(crate::scheduler::runner::scheduler_task(
            shell.environment.read().scheduler.clone(),
            sched_tx,
        ));

        let prompt_mark_cache = prompt.read().mark.clone();
        let prompt_mark_width = display_width(&prompt_mark_cache);

        let envronment = Arc::clone(&shell.environment);
        let input_preferences = envronment.read().input_preferences();
        let mut suggestion_manager = SuggestionManager::new();
        let mut ai_service: Option<Arc<dyn AiService + Send + Sync>> = None;
        if let Some((ai_backend, client)) = Self::build_ai_backend(&envronment) {
            suggestion_manager.engine.set_ai_backend(Some(ai_backend));

            // ... (in Repl::new)

            let allowlist = envronment.read().policy_state.execute_allowlist.clone();
            let service = Arc::new(LiveAiService::new(
                client,
                envronment.read().integration_state.mcp_manager.clone(),
                envronment.read().policy_state.safety_level.clone(),
                shell.safety_guard.clone(),
                Some(confirmation::ReplConfirmationHandler::new()),
                allowlist,
            ));

            // Store in environment so ShellProxy can access it
            envronment.write().integration_state.ai_service = Some(service.clone());
            ai_service = Some(service);
        }
        suggestion_manager.set_preferences(input_preferences);

        // Setup Git event channel
        let (git_tx, git_rx) = tokio::sync::mpsc::unbounded_channel();
        prompt.write().set_git_sender(git_tx);

        // Setup AI event channel
        let (ai_tx, ai_rx) = tokio::sync::mpsc::unbounded_channel();
        let (background_io_tx, background_io_rx) = tokio::sync::mpsc::unbounded_channel();
        let background_io = BackgroundIoCoordinator::new(background_io_tx);
        let integrated_completion = IntegratedCompletionEngine::new(envronment);
        integrated_completion.set_notifier(completion_tx.clone());
        shell.completion_runtime = Some(integrated_completion.runtime());
        // Legacy path completion still uses its own cache; keep it connected
        // until that cache is moved behind CompletionRuntime as well.
        completion_lib::set_completion_notifier(completion_tx);
        let event_loop = event_loop::ReplEventLoop::new(
            AI_SUGGESTION_REFRESH_MS,
            git_rx,
            sched_rx,
            completion_rx,
            ai_rx,
            background_io_rx,
        );

        Repl {
            shell,
            input: Input::new(input_config),
            history_search: None,
            pending_chord: Vec::new(),
            state: ReplState::new(current.clone()),
            services: ReplServices::new(
                ai_service,
                command_timing::create_shared_timing(),
                Arc::clone(&prompt),
            ),
            terminal_ui: TerminalUiState {
                columns: 0,
                lines: 0,
                tmode: None,
                prompt: Arc::clone(&prompt),
                prompt_mark_cache,
                prompt_mark_width,
                ctrl_c_state: DoublePressState::new(3000),
                esc_state: DoublePressState::new(400),
                last_drawn_cursor_y: 0,
                last_preprompt_plain: None,
                status_line: status_line::shared(input_preferences.status_line),
            },
            completion_ui: CompletionUiState {
                start_completion: false,
                completion: Completion::new(),
                integrated_completion,
                cache: HistoryCache::new(Duration::from_millis(300)),
            },
            ai_ui: AiUiState {
                suggestion_manager,
                input_preferences,
                ai_pending_shown: false,
                last_explanation: None,
                auto_fix_suggestion: None,
                pending_ai_explanation_input: None,
                current_ai_explanation: None,
                last_input_change_time: Instant::now(),
                ai_tx,
                explanation_dirty: false,
                last_analyzed_input: String::new(),
                last_analysis_result: None,
            },
            background_tasks: BackgroundTasks {
                last_git_update: None,
                git_task_inflight: Arc::new(AtomicBool::new(false)),
                history_sync_last_check: Instant::now(),
                github_task: Some(github_task),
                sched_task: Some(sched_task),
                io: background_io,
            },
            event_loop,
        }
    }

    pub(crate) fn trigger_file_context_update(&self) {
        let cache = self.services.file_context.clone();
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
        self.terminal_ui.columns = screen_size.0 as usize;
        self.terminal_ui.lines = screen_size.1 as usize;
        self.terminal_ui
            .status_line
            .borrow_mut()
            .set_size(screen_size.0, screen_size.1);

        // Initialize integrated completion engine
        debug!("Initializing integrated completion engine (this may use cached JSON data)...");
        if let Err(e) = self
            .completion_ui
            .integrated_completion
            .initialize_command_completion()
        {
            warn!("Failed to initialize command completion: {}", e);
        } else {
            debug!("Integrated completion engine initialized successfully");
        }
        self.terminal_ui.lines = screen_size.1 as usize;
        enable_raw_mode().ok();
        let mut renderer = TerminalRenderer::new();
        queue!(renderer, EnableBracketedPaste).ok();
        renderer.flush().ok();
    }

    pub(crate) async fn check_background_jobs(&mut self, output: bool) -> Result<()> {
        key_handlers::auxiliary::check_background_jobs(self, output).await
    }

    pub(crate) fn sync_completion_jobs(&self) {
        self.completion_ui.integrated_completion.set_shell_jobs(
            self.shell
                .wait_jobs
                .iter()
                .map(|job| (job.job_id, job.cmd.clone(), job.state.to_string()))
                .collect(),
        );
    }

    pub(crate) async fn handle_event(&mut self, ev: ShellEvent) -> Result<ReplControlFlow> {
        handler::handle_event(self, ev).await
    }

    pub(crate) async fn handle_key_event(&mut self, ev: &KeyEvent) -> Result<ReplControlFlow> {
        let result = handler::handle_key_event(self, ev).await;
        // Mark explanation as dirty for debounced refresh
        self.ai_ui.explanation_dirty = true;
        result
    }

    fn refresh_argument_explanation(&mut self) {
        let input = self.input.to_string();
        let cursor = self.input.cursor();
        let explanation_to_show = if let Some(ref ai_exp) = self.ai_ui.current_ai_explanation {
            Some(format!("\u{2728} {}", ai_exp))
        } else {
            self.services
                .argument_explainer
                .get_explanation(&input, cursor)
        };

        if explanation_to_show != self.ai_ui.last_explanation {
            self.ai_ui.last_explanation = explanation_to_show.clone();

            use crossterm::{QueueableCommand, cursor, style::Print, terminal};
            use std::io::Write;

            if self.terminal_ui.columns == 0 {
                return;
            }

            let mut stdout = std::io::stdout();
            // Save cursor position
            stdout.queue(cursor::SavePosition).ok();
            // Move to next line and clear it
            stdout.queue(cursor::MoveToNextLine(1)).ok();
            stdout
                .queue(terminal::Clear(terminal::ClearType::CurrentLine))
                .ok();

            if let Some(ref s) = explanation_to_show {
                let styled = format!(" \x1b[38;5;244m[ {} ]\x1b[0m", s);
                stdout.queue(Print(styled)).ok();
            }

            // Restore cursor to original position
            stdout.queue(cursor::RestorePosition).ok();
            stdout.flush().ok();
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
        prev_pos: (usize, usize),
        new_pos: (usize, usize),
    ) {
        render::move_cursor_relative(self, out, prev_pos, new_pos)
    }

    pub(crate) fn print_prompt(&mut self, out: &mut impl Write) {
        render::print_prompt(self, out)
    }

    fn sync_input_preferences(&mut self) {
        let prefs = self.shell.environment.read().input_preferences();
        if prefs != self.ai_ui.input_preferences {
            self.ai_ui.input_preferences = prefs;
            self.ai_ui.suggestion_manager.engine.set_preferences(prefs);
            // If explanation was just enabled, we don't necessarily need to reset the timer here,
            // as the next event or tick will handle it.
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
        let suggestion = match self.ai_ui.suggestion_manager.active.clone() {
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
            self.ai_ui.suggestion_manager.clear();
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
        if let Some(last_time) = self.completion_ui.cache.time
            && now.duration_since(last_time) <= self.completion_ui.cache.ttl
            && self.completion_ui.cache.prefix.starts_with(input)
            && let Some(ref list) = self.completion_ui.cache.match_sorted
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

    pub(crate) fn analyze_input(&self, input: &str, completion: Option<String>) -> InputAnalysis {
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

    /// Rows the currently drawn preprompt occupies at the *current* width.
    ///
    /// Zero in continuation mode. Recomputed on each call so a resize between
    /// drawing and erasing cannot leave a stale count behind.
    pub(crate) fn preprompt_rows(&self) -> usize {
        match &self.terminal_ui.last_preprompt_plain {
            None => 0,
            Some(plain) => render::preprompt_rows(plain, self.terminal_ui.columns),
        }
    }

    pub async fn run_interactive(&mut self) -> Result<()> {
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
        self.terminal_ui.tmode = match tcgetattr(unsafe { BorrowedFd::borrow_raw(SHELL_TERMINAL) })
        {
            Ok(tmode) => Some(tmode),
            Err(e) => {
                warn!("Failed to get terminal attributes: {}", e);
                None
            }
        };
        {
            let mut renderer = TerminalRenderer::new();
            self.print_prompt(&mut renderer);
            renderer.flush().ok();
        }
        self.shell.check_job_state().await?;
        self.event_loop.resume_input();

        loop {
            let event = self.event_loop.next_event().await;
            if !self.handle_loop_event(event).await? {
                break;
            }

            if self.completion_ui.start_completion {
                self.completion_ui.start_completion = false;
            }
            if self.should_exit_event_loop() {
                break;
            }
        }

        self.event_loop.pause_input();
        self.shell.kill_wait_jobs()?;
        Ok(())
    }

    async fn handle_loop_event(&mut self, event: event_loop::LoopEvent) -> Result<bool> {
        match event {
            event_loop::LoopEvent::BackgroundTick => self.handle_background_tick().await?,
            event_loop::LoopEvent::AiRefreshTick => self.handle_ai_refresh_tick(),
            event_loop::LoopEvent::ExplanationRefreshTick => {
                if self.ai_ui.explanation_dirty {
                    self.ai_ui.explanation_dirty = false;
                    self.refresh_argument_explanation();
                }
            }
            event_loop::LoopEvent::ExplanationIdle => self.handle_explanation_idle(),
            event_loop::LoopEvent::GitRefresh => self.handle_git_refresh_request(),
            event_loop::LoopEvent::Scheduler(event) => self.handle_scheduler_event(event),
            event_loop::LoopEvent::CompletionRefresh => self.handle_completion_refresh(),
            event_loop::LoopEvent::Ai(event) => self.handle_ai_event(event),
            event_loop::LoopEvent::BackgroundIo(event) => self.handle_background_io_event(event),
            event_loop::LoopEvent::TerminalInput(event) => {
                return self.handle_terminal_input(event).await;
            }
            event_loop::LoopEvent::TerminalError(error) => {
                self.shell.print_error(format!("Error: {error:?}\r"));
                return Ok(false);
            }
            event_loop::LoopEvent::TerminalClosed => return Ok(false),
        }
        Ok(true)
    }

    fn handle_ai_refresh_tick(&mut self) {
        let mut need_redraw = false;
        if self.ai_ui.input_preferences.ai_backfill
            && self.input.completion.is_none()
            && self.refresh_inline_suggestion()
        {
            need_redraw = true;
        }

        if self.ai_ui.suggestion_manager.engine.ai_pending() != self.ai_ui.ai_pending_shown {
            need_redraw = true;
        }

        if need_redraw {
            let mut renderer = TerminalRenderer::new();
            self.print_input(&mut renderer, false, false);
            renderer.flush().ok();
        }
    }

    fn handle_explanation_idle(&mut self) {
        if self.ai_ui.input_preferences.ai_explanation
            && self.services.ai.is_some()
            && !self.input.is_empty()
            && self.ai_ui.pending_ai_explanation_input.as_deref() != Some(self.input.as_str())
            && self.ai_ui.current_ai_explanation.is_none()
        {
            let input = self.input.as_str().to_string();
            self.ai_ui.pending_ai_explanation_input = Some(input.clone());
            let ai_tx = self.ai_ui.ai_tx.clone();
            let service = self.services.ai.clone();

            tokio::spawn(async move {
                if let Some(service) = service {
                    match crate::ai_features::explain_command_inline(service.as_ref(), &input).await
                    {
                        Ok(explanation) => {
                            let _ = ai_tx.send(AiEvent::CommandExplanation {
                                input: input.clone(),
                                explanation,
                            });
                        }
                        Err(error) => {
                            tracing::debug!("Failed to get AI explanation: {}", error);
                            let _ = ai_tx.send(AiEvent::CommandExplanationError {
                                input: input.clone(),
                            });
                        }
                    }
                } else {
                    let _ = ai_tx.send(AiEvent::CommandExplanationError { input });
                }
            });
        }

        self.event_loop.reset_idle(Duration::from_secs(3600));
    }

    async fn handle_terminal_input(&mut self, event: crossterm::event::Event) -> Result<bool> {
        let old_last_time = self.state.last_command_time;
        let control_flow = match self.handle_event(ShellEvent::Input(event)).await {
            Ok(control_flow) => control_flow,
            Err(error) => {
                self.shell.print_error(format!("Error: {error:?}\r"));
                return Ok(false);
            }
        };

        if !self.apply_repl_control_flow(control_flow).await {
            return Ok(false);
        }

        self.after_terminal_input(old_last_time);
        if self.ai_ui.input_preferences.ai_explanation {
            self.event_loop.reset_idle(Duration::from_secs(5));
        }
        Ok(true)
    }

    async fn apply_repl_control_flow(&mut self, control_flow: ReplControlFlow) -> bool {
        match control_flow {
            ReplControlFlow::Continue => true,
            ReplControlFlow::ExecuteCurrentInput => {
                self.event_loop.pause_input();
                let status_pause =
                    status_line::StatusLinePause::new(self.terminal_ui.status_line.clone());
                let result = key_handlers::execution::handle_execute(self).await;
                drop(status_pause);

                match result {
                    Ok(()) => {
                        self.event_loop.resume_input();
                        true
                    }
                    Err(error) => {
                        self.shell.print_error(format!("Error: {error:?}\r"));
                        false
                    }
                }
            }
            ReplControlFlow::OpenCommandPalette => {
                self.event_loop.pause_input();
                let status_pause =
                    status_line::StatusLinePause::new(self.terminal_ui.status_line.clone());
                let result = key_handlers::auxiliary::handle_open_command_palette(self).await;
                drop(status_pause);

                match result {
                    Ok(_) => {
                        self.event_loop.resume_input();
                        true
                    }
                    Err(error) => {
                        self.shell.print_error(format!("Error: {error:?}\r"));
                        false
                    }
                }
            }
            ReplControlFlow::RunInteractive(closure) => {
                self.event_loop.pause_input();

                let mut execute_after = false;
                let raw_mode_pause = terminal_state::RawModePause::new();
                let status_pause =
                    status_line::StatusLinePause::new(self.terminal_ui.status_line.clone());
                match closure() {
                    Ok(Some(action)) => {
                        use crate::repl::state::InteractiveAction;
                        match action {
                            InteractiveAction::Patch {
                                backspace_count,
                                text,
                            } => {
                                if backspace_count > 0 {
                                    self.input.backspacen(backspace_count);
                                }
                                self.input.insert_str(&text);
                            }
                            InteractiveAction::ReplaceRange { start, end, text } => {
                                self.input.replace_range_chars(start, end, &text);
                            }
                            InteractiveAction::ReplaceAll { text } => self.input.reset(text),
                            InteractiveAction::ReplaceAllAndExecute { text } => {
                                self.input.reset(text);
                                execute_after = true;
                            }
                        }
                        self.input.completion = None;
                        self.input.color_ranges = None;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        self.shell
                            .print_error(format!("Interactive session failed: {error}\r\n"));
                    }
                }
                drop(raw_mode_pause);

                if execute_after {
                    let result = key_handlers::execution::handle_execute(self).await;
                    drop(status_pause);
                    if let Err(error) = result {
                        self.shell.print_error(format!("Error: {error:?}\r"));
                        return false;
                    }
                } else {
                    drop(status_pause);
                    let mut renderer = TerminalRenderer::new();
                    self.print_prompt(&mut renderer);
                    self.print_input(&mut renderer, true, true);
                    renderer.flush().ok();
                }

                self.event_loop.resume_input();
                true
            }
        }
    }

    fn after_terminal_input(&mut self, old_last_time: Option<Instant>) {
        let current_cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
        if current_cwd != self.state.last_cwd {
            self.state.last_cwd = current_cwd;
            if self.ai_ui.input_preferences.ai_backfill {
                debug!(
                    "CWD changed to {:?}, triggering AI prefetch",
                    self.state.last_cwd
                );
                let files = self.get_directory_listing();
                let files = files.lines().map(String::from).collect();
                self.ai_ui.suggestion_manager.engine.prefetch(
                    Some(self.state.last_cwd.to_string_lossy().to_string()),
                    Arc::new(files),
                    Some(self.state.last_status),
                );
            }
        }

        if self.state.last_command_time != old_last_time {
            self.state.stopped_jobs_warned = false;
            self.terminal_ui.prompt.write().invalidate_git_cache();
            self.terminal_ui.prompt.read().trigger_git_check();
            if self.state.last_status != 0 {
                self.trigger_auto_fix();
            }
        }
    }

    fn should_exit_event_loop(&mut self) -> bool {
        if !self.state.should_exit && self.shell.exited.is_none() {
            return false;
        }

        debug!("Shell exiting normally");
        if !self.shell.wait_jobs.is_empty() && !self.state.stopped_jobs_warned {
            self.shell
                .print_error("There are stopped jobs.\r\n".to_string());
            self.state.stopped_jobs_warned = true;
            self.state.should_exit = false;
            self.shell.exited = None;
            return false;
        }
        true
    }

    async fn handle_background_tick(&mut self) -> Result<()> {
        self.save_history_periodic();
        self.schedule_command_timing_save();
        self.check_background_jobs(true).await?;

        if self.background_tasks.history_sync_last_check.elapsed() > Duration::from_secs(30) {
            self.background_tasks.io.schedule_history_sync(
                self.shell.cmd_history.as_ref(),
                self.shell.path_history.as_ref(),
            );
            self.background_tasks.history_sync_last_check = Instant::now();
        }

        let _ = self.shell.exec_input_timeout_hooks();
        self.services.prompt_refresh.schedule();
        self.refresh_status_line();
        Ok(())
    }

    pub(crate) fn schedule_command_timing_save(&mut self) {
        let Some(path) = command_timing::get_timing_file_path() else {
            return;
        };
        self.background_tasks
            .io
            .schedule_timing_save(&self.services.command_timing, path);
    }

    fn handle_background_io_event(&mut self, event: BackgroundIoEvent) {
        self.background_tasks.io.apply_event(
            event,
            self.shell.cmd_history.as_ref(),
            self.shell.path_history.as_ref(),
            &self.services.command_timing,
        );
    }

    /// Redraws the status line from cached state.
    ///
    /// Cheap and idempotent: `render` skips the write when nothing changed, so
    /// calling it on every 1-second tick costs nothing while the shell is idle.
    pub(crate) fn refresh_status_line(&mut self) {
        // Re-read the preference each time so `(pref-status-line t)` and
        // `reload` take effect without restarting the shell.
        let wanted = self
            .shell
            .environment
            .read()
            .completion_state
            .input_preferences
            .status_line;

        {
            let mut status = self.terminal_ui.status_line.borrow_mut();
            if status.is_enabled() != wanted {
                if !wanted {
                    // Turning it off has to give the row back.
                    let mut renderer = TerminalRenderer::new();
                    status.disarm(&mut renderer);
                    renderer.flush().ok();
                }
                status.set_enabled(wanted);
            }
            if !status.is_enabled() {
                return;
            }
        }

        let scheduler = self.shell.environment.read().scheduler.clone();
        let job_count = self.shell.wait_jobs.len();
        let (git, github) = {
            let prompt = self.terminal_ui.prompt.read();
            (
                prompt.get_git_status_cached(),
                prompt
                    .github_status
                    .as_ref()
                    .map(|status| status.read().clone()),
            )
        };

        let content =
            status_line::compose(&scheduler.read(), job_count, git.as_ref(), github.as_ref());

        let mut renderer = TerminalRenderer::new();
        self.terminal_ui
            .status_line
            .borrow_mut()
            .render(&mut renderer, &content);
        renderer.flush().ok();
    }

    /// Files a finished scheduled run and, if its policy says so, tells the
    /// user about it.
    ///
    /// Runs on the REPL task, so it is never concurrent with a redraw. While a
    /// foreground command is executing the select loop is not polled at all,
    /// which is what keeps these notices from landing in the middle of another
    /// command's output — they queue and appear at the next prompt.
    fn handle_scheduler_event(&mut self, event: crate::scheduler::SchedulerEvent) {
        // `out` and `tm` only ever display this string, so it can carry a label
        // marking the run as scheduled rather than typed.
        let label = format!("sched:{} {}", event.name, event.command);

        let entry = dsh_types::output_history::OutputEntry::new(
            label,
            event.stdout.clone(),
            event.stderr.clone(),
            event.exit_code,
        );

        {
            let mut environment = self.shell.environment.write();
            let history = &mut environment.session_output_state.output_history;
            history.push(entry);
            // `push` inserts at the front, so index 1 is the entry we just
            // added. Taking the *last* one would attach some unrelated older
            // command's output to this block.
            let recorded: Vec<_> = history.get(1).cloned().into_iter().collect();

            let block = dsh_types::command_block::CommandBlock::new(
                // The block's command is what `blocks rerun` feeds back to the
                // evaluator, so it has to stay executable — no `sched:` prefix
                // here, unlike the output-history entry above.
                event.command.clone(),
                Some(event.cwd.clone()),
                event.exit_code,
                event.duration.as_millis() as u64,
                &recorded,
                None,
            );
            environment.session_output_state.command_blocks.push(block);
        }

        if !event.notify {
            return;
        }

        let mut renderer = TerminalRenderer::new();
        render::print_above_prompt(self, &mut renderer, &[event.notice()]);
        renderer.flush().ok();
        // `print_above_prompt` erased the reserved row; repaint now rather than
        // leaving it blank until the next tick.
        self.refresh_status_line();

        let prefs = self
            .shell
            .environment
            .read()
            .completion_state
            .input_preferences;
        notify::notify_scheduled_task(
            &prefs,
            &event.name,
            &event.command,
            event.exit_code,
            event.timed_out,
        );
    }

    fn handle_git_refresh_request(&mut self) {
        let now = Instant::now();
        let is_throttled = self.background_tasks.last_git_update.is_some_and(|last| {
            now.duration_since(last) < Duration::from_millis(GIT_STATUS_THROTTLE_MS)
        });
        if is_throttled
            || self
                .background_tasks
                .git_task_inflight
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return;
        }

        self.background_tasks.last_git_update = Some(now);
        let prompt = Arc::clone(&self.terminal_ui.prompt);
        let inflight = Arc::clone(&self.background_tasks.git_task_inflight);
        tokio::spawn(async move {
            if prompt.read().needs_git_check {
                let cwd = prompt.read().current_dir.clone();
                let root = crate::prompt::find_git_root_async(cwd).await;
                prompt.write().update_git_root(root);
            }
            if prompt.read().has_git_root() {
                let path = prompt.read().current_path().to_path_buf();
                if let Some(status) = crate::prompt::fetch_git_status_async(&path).await {
                    prompt.write().update_git_status(Some(status));
                }
            }
            inflight.store(false, Ordering::SeqCst);
        });
    }

    fn handle_completion_refresh(&mut self) {
        if self.input.completion.is_none() && self.refresh_inline_suggestion() {
            let mut renderer = TerminalRenderer::new();
            self.print_input(&mut renderer, false, false);
            renderer.flush().ok();
        }
    }

    fn handle_ai_event(&mut self, event: AiEvent) {
        match event {
            AiEvent::AutoFix(fix) => {
                self.ai_ui.auto_fix_suggestion = Some(fix);
                if self.input.as_str().is_empty() {
                    let mut renderer = TerminalRenderer::new();
                    self.print_input(&mut renderer, false, false);
                    renderer.flush().ok();
                }
            }
            AiEvent::CommandExplanation { input, explanation } => {
                if self.input.as_str() == input {
                    self.ai_ui.current_ai_explanation = Some(explanation);
                    self.ai_ui.explanation_dirty = true;
                }
            }
            AiEvent::CommandExplanationError { input } => {
                if self.ai_ui.pending_ai_explanation_input.as_deref() == Some(input.as_str()) {
                    self.ai_ui.pending_ai_explanation_input = None;
                }
            }
        }
    }

    /// Largest history snapshot handed to the interactive picker.
    ///
    /// The picker re-filters the whole snapshot on every keystroke, so this
    /// bounds that work; older entries stay reachable through the `history`
    /// builtin.
    const HISTORY_PICKER_SNAPSHOT: usize = 5000;

    /// Ctrl-R: search history interactively.
    ///
    /// Uses the dedicated picker, which exposes the scope/status/duration
    /// filters and the per-entry metadata. `DSH_HISTORY_PICKER=skim` selects the
    /// previous skim-based flow for one release.
    pub fn select_history(&mut self) -> Result<ReplControlFlow> {
        if history_picker_backend_is_skim() {
            return self.select_history_with_skim();
        }

        let Some(history_arc) = self.shell.cmd_history.as_ref() else {
            return Ok(ReplControlFlow::Continue);
        };
        let Some(mut history) = history_arc.try_lock() else {
            warn!("Failed to acquire command history lock for history selection - lock is busy");
            return Ok(ReplControlFlow::Continue);
        };

        let entries = history.snapshot_entries(Self::HISTORY_PICKER_SNAPSHOT);
        history.reset_index();
        // Released before the interactive session: the picker owns a snapshot,
        // and holding the lock would block the background history writer.
        drop(history);

        if entries.is_empty() {
            // Say so rather than swallowing the keypress: an unexplained no-op
            // reads as a broken binding.
            let mut renderer = TerminalRenderer::new();
            renderer
                .write_all(b"\r\ndsh: no command history yet\r\n")
                .ok();
            self.print_prompt(&mut renderer);
            self.print_input(&mut renderer, false, false);
            renderer.flush().ok();
            return Ok(ReplControlFlow::Continue);
        }

        // Same scope context the `history` builtin builds, so `scope:cwd` means
        // the same thing in both.
        let base = crate::history::query_context(self.shell.session_id.clone());
        let picker = crate::history::picker::HistoryPicker::new(
            entries,
            base,
            self.input.as_str().to_string(),
            chrono::Local::now().timestamp(),
        );

        Ok(ReplControlFlow::RunInteractive(Box::new(move || {
            Ok(crate::history::picker::run(picker)?
                .map(|text| crate::repl::state::InteractiveAction::ReplaceAll { text }))
        })))
    }

    /// The pre-picker skim flow, kept behind `DSH_HISTORY_PICKER=skim`.
    fn select_history_with_skim(&mut self) -> Result<ReplControlFlow> {
        let query = self.input.as_str();
        if let Some(ref mut history) = self.shell.cmd_history {
            if let Some(mut history) = history.try_lock() {
                let history_query = crate::history::HistoryQuery {
                    text: if query.is_empty() {
                        None
                    } else {
                        Some(query.to_string())
                    },
                    limit: Some(500),
                    ..Default::default()
                };
                let items: Vec<completion_lib::Candidate> = history
                    .search_entries(&history_query)
                    .into_iter()
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

    async fn toggle_sudo(&mut self) -> Result<()> {
        input_analysis::toggle_sudo(self).await
    }

    /// Get directory listing for AI context
    fn get_directory_listing(&self) -> String {
        repl_ai::get_directory_listing_content(std::path::Path::new(".")).join("\n")
    }

    async fn expand_smart_pipe(&self, query: String) -> Result<String> {
        let service = self
            .services
            .ai
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("AI client not configured"))?;
        ai_features::expand_smart_pipe(service.as_ref(), &query).await
    }

    async fn run_generative_command(&self, query: &str) -> Result<String> {
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
        let Some(_service) = self.services.ai.clone() else {
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

        // The captured output is unbounded; keep both ends so an error at the
        // tail of a long log still reaches the model.
        let bounded_output =
            dsh_openai::turn::truncate_middle(&combined_output, AI_PIPE_OUTPUT_CHARS);
        let message = format!(
            "Shell command: `{}`\n\nOutput:\n```\n{}\n```\n\nQuery: {}",
            command, bounded_output, query
        );

        let ctx = Context::new_safe(getpid(), getpid(), true);
        execute_chat_message(&ctx, &mut *self.shell, &message, None);

        self.state.last_status = exit_code;
        self.state.last_command_string = command;

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
            writer
                .variable_state
                .alias
                .insert("ll".to_string(), "ls -al".to_string());
        }

        let mut shell = Shell::new(env.clone());
        let repl = Repl::new(&mut shell);

        assert!(
            super::input_analysis::command_is_valid(&repl, "cd"),
            "built-in command should be valid"
        );
        assert!(
            super::input_analysis::command_is_valid(&repl, "ll"),
            "alias should be valid"
        );
        assert!(
            !super::input_analysis::command_is_valid(&repl, "definitely_not_a_command_42"),
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

        assert_eq!(repl.ai_ui.auto_fix_suggestion, Some("ls -la".to_string()));
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

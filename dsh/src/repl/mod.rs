use crate::ai_features::{AgentPolicyHandles, AiService, LiveAiService};
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
use crossterm::queue;
use crossterm::terminal::{self, disable_raw_mode, enable_raw_mode};
#[cfg(test)]
use futures::StreamExt;

use dsh_openai::{ChatGptClient, OpenAiConfig};
use nix::sys::termios::{Termios, tcgetattr};
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
pub(crate) mod failure_hint;
mod handler;
mod history_nav;
mod init;
use init::*;
pub(crate) mod job_notify;
pub mod key_action;
mod key_handlers;
pub(crate) mod keybind;
pub(crate) mod last_arg;
mod loop_handlers;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoFixKind {
    /// Deterministic quick fix; `replacement` is ready to accept.
    QuickFix,
    /// AI-generated replacement (opt-in via `set-auto-fix-enabled`).
    AiFix,
    /// Nothing to apply; the annotation only points at Alt-f / Alt-d.
    DiagnoseHintOnly,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoFixSuggestion {
    /// Replacement command shown as ghost text. Empty for `DiagnoseHintOnly`.
    pub replacement: String,
    /// Short reason shown as a right-aligned annotation.
    pub title: Option<String>,
    pub kind: AutoFixKind,
    /// `last_command_time` when this suggestion was computed. An async AI fix
    /// that arrives after another command ran is stale and gets dropped.
    pub command_time: Option<Instant>,
}

impl AutoFixSuggestion {
    /// Whether there is a replacement the user can accept with Tab/Alt-f.
    pub fn has_fix(&self) -> bool {
        !self.replacement.is_empty()
    }
}

#[derive(Debug)]
pub enum AiEvent {
    AutoFix(AutoFixSuggestion),
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
    pub(crate) auto_fix_suggestion: Option<AutoFixSuggestion>,
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
    /// The in-session cron driver (`dsh/src/cron/runner.rs`). `None` when the
    /// cron store could not be opened at startup — cron is unavailable for
    /// this session rather than fatal to it.
    pub(crate) cron_task: Option<tokio::task::JoinHandle<()>>,
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

impl<'a> Drop for Repl<'a> {
    fn drop(&mut self) {
        // Cancel background tasks. Aborting the cron runner only stops this
        // session's *scanning* for due jobs - a run already spawned as its
        // own detached child (`dsh/src/cron/exec.rs`) keeps going, since cron
        // jobs are meant to survive the session that happened to start them.
        if let Some(handle) = self.background_tasks.github_task.take() {
            handle.abort();
        };
        if let Some(handle) = self.background_tasks.cron_task.take() {
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
}

pub(crate) use render::render_transient_prompt_to;

mod state_tests;
#[cfg(test)]
mod tests;

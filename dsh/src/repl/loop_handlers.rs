//! The interactive event loop: `run_interactive`'s select loop and every `LoopEvent` handler it dispatches to (background tick, AI refresh, git refresh, scheduler, completion refresh, terminal input).
use super::*;

impl<'a> Repl<'a> {
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
            && self.ai_service().is_some()
            && !self.input.is_empty()
            && self.ai_ui.pending_ai_explanation_input.as_deref() != Some(self.input.as_str())
            && self.ai_ui.current_ai_explanation.is_none()
        {
            let input = self.input.as_str().to_string();
            self.ai_ui.pending_ai_explanation_input = Some(input.clone());
            let ai_tx = self.ai_ui.ai_tx.clone();
            let service = self.ai_service();

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
            // A new command ran; whatever hint the previous failure produced
            // is stale now.
            self.ai_ui.auto_fix_suggestion = None;
            if self.state.last_status != 0 {
                self.maybe_auto_fix_on_failure();
            } else {
                self.state.last_hinted_failure = None;
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
        self.check_agent_notices();

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
        // Cheap no-op when Herdr isn't active (`NullReporter::is_stale`
        // always answers "no"): catches the rare case where a transient
        // `herdr` failure dropped exactly the report that would have moved
        // the reported state off `Working`/`Blocked`.
        crate::agent_lifecycle::current(self.shell).reconcile_if_stale();
        Ok(())
    }

    /// Prints any notices the agent-task watcher
    /// (`dsh/src/agent/watch.rs`) queued since the last tick - a detached
    /// task completing, failing, or needing approval. Mirrors
    /// `check_background_jobs`'s own shape for job notices, so the two read
    /// as the same kind of thing above the prompt.
    fn check_agent_notices(&mut self) {
        let notices = std::mem::take(&mut *self.shell.environment.read().agent_notices.lock());
        if notices.is_empty() {
            return;
        }

        let prefs = self
            .shell
            .environment
            .read()
            .completion_state
            .input_preferences;
        // `notify_agent_task` already checks `prefs.auto_notify_enabled`
        // itself (the same convention `notify_command_finished`'s callers
        // rely on), so this loop does not gate on it a second time.
        for line in &notices {
            crate::repl::notify::notify_agent_task(&prefs, line);
        }

        let mut renderer = TerminalRenderer::new();
        crate::repl::render::print_above_prompt(self, &mut renderer, &notices);
        let _ = renderer.flush();
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

        let cron = self.shell.environment.read().cron_health.clone();
        let agent = self.shell.environment.read().agent_health.clone();
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

        let content = status_line::compose(
            &cron.read(),
            &agent.read(),
            job_count,
            git.as_ref(),
            github.as_ref(),
        );

        let mut renderer = TerminalRenderer::new();
        self.terminal_ui
            .status_line
            .borrow_mut()
            .render(&mut renderer, &content);
        renderer.flush().ok();
    }

    /// Files a finished scheduled run and, if its policy says so, tells the
    /// user about it.
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
                // An async AI fix can outlive the failure it was computed for;
                // showing it against a newer command would be misleading.
                if fix.command_time != self.state.last_command_time {
                    return;
                }
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
}

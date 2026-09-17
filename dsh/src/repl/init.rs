//! `Repl::new` construction (background tasks, prompt/AI/completion wiring, event channels) and its one private helper, `build_ai_backend`.
use super::*;

/// Whether Ctrl-R should use the pre-picker skim flow.
///
/// An escape hatch for one release, following `DOGESH_COMPLETION_FRAMEWORK`.
pub(super) fn history_picker_backend_is_skim() -> bool {
    matches!(std::env::var("DOGESH_HISTORY_PICKER"), Ok(value) if value.eq_ignore_ascii_case("skim"))
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

        // The cron store lives on disk, independent of any one session, so a
        // session merely opens it (or, if that fails, simply runs without an
        // in-session driver — jobs still fire from an external tick).
        let cron_task = match crate::cron::store::SqliteCronStore::open(
            &dsh_builtin::config_paths::cron_state_dir(),
        ) {
            Ok(store) => Some(tokio::spawn(crate::cron::runner::cron_runner_task(
                Arc::new(store),
                shell.environment.read().cron_health.clone(),
            ))),
            Err(error) => {
                warn!("cron: session driver disabled, could not open its store: {error}");
                None
            }
        };

        // The in-session half of `agent run --detach`'s notices
        // (`dsh/src/agent/watch.rs`): a task started as detached still runs
        // and finishes without this, it just does so silently. Settings are
        // resolved once, here, not re-read every scan.
        let agent_task = if crate::agent::watch::resolve_enabled(shell) {
            let active_interval_secs = crate::agent::watch::resolve_active_interval_secs(shell);
            match crate::agent::SqliteTaskStore::open(&dsh_builtin::config_paths::agent_state_dir())
            {
                Ok(store) => {
                    // Two separate statements, deliberately: both temporaries
                    // from a single `f(a().read()..., b().read()...)` call
                    // live until the end of that whole statement, not just
                    // their own argument - holding two `RwLockReadGuard`s on
                    // the same `parking_lot::RwLock` at once, which can
                    // self-deadlock this thread if a writer queues in
                    // between (parking_lot's task-fair policy blocks a new
                    // reader once a writer is waiting, even one already held
                    // by the requesting thread).
                    let agent_health = shell.environment.read().agent_health.clone();
                    let agent_notices = shell.environment.read().agent_notices.clone();
                    Some(tokio::spawn(crate::agent::watch::agent_watch_task(
                        Arc::new(store),
                        agent_health,
                        agent_notices,
                        active_interval_secs,
                    )))
                }
                Err(error) => {
                    warn!("agent: watch task disabled, could not open its store: {error}");
                    None
                }
            }
        } else {
            None
        };

        let prompt_mark_cache = prompt.read().mark.clone();
        let prompt_mark_width = display_width(&prompt_mark_cache);

        let envronment = Arc::clone(&shell.environment);
        let input_preferences = envronment.read().input_preferences();
        let mut suggestion_manager = SuggestionManager::new();
        // Always constructed, even without a key: the holders below share
        // the `ai_client` slot and follow `reload_ai_client`, so a key set
        // later (or removed later) takes effect without a restart.
        // Availability checks must read `ai_configured()` (or the slot via
        // `get_ai_service`), never `ai_service.is_some()`.
        let (ai_backend, shared_client) = Self::build_ai_backend(&envronment);
        suggestion_manager.engine.set_ai_backend(Some(ai_backend));

        // ... (in Repl::new)

        let policy = AgentPolicyHandles {
            safety_level: envronment.read().policy_state.safety_level.clone(),
            safety_guard: shell.safety_guard.clone(),
            execute_allowlist: envronment.read().policy_state.execute_allowlist.clone(),
            agent_session_allowlist: envronment
                .read()
                .policy_state
                .agent_session_allowlist
                .clone(),
        };
        let response_language = envronment
            .read()
            .integration_state
            .response_language
            .clone();
        let chat_model = envronment.read().integration_state.chat_model.clone();
        let service = Arc::new(LiveAiService::new(
            shared_client,
            envronment.read().integration_state.mcp_manager.clone(),
            policy,
            Some(confirmation::ReplConfirmationHandler::new()),
            response_language,
            chat_model,
        ));

        // Store in environment so ShellProxy can access it
        envronment.write().integration_state.ai_service = Some(service.clone());
        let ai_service: Option<Arc<dyn AiService + Send + Sync>> = Some(service);
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
                cron_task,
                agent_task,
                io: background_io,
            },
            event_loop,
        }
    }

    fn build_ai_backend(
        environment: &Arc<RwLock<Environment>>,
    ) -> (
        Arc<dyn SuggestionBackend + Send + Sync>,
        crate::ai_features::SharedChatClient,
    ) {
        let slot = environment.read().integration_state.ai_client.clone();
        let chat_model = environment.read().integration_state.chat_model.clone();
        let backend = Arc::new(crate::suggestion::AiSuggestionBackend::new(
            slot.clone(),
            chat_model,
        ));
        (backend, crate::ai_features::SharedChatClient::new(slot))
    }
}

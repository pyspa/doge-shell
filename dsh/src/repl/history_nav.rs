//! History-related helpers pulled out of the main `impl Repl`: saving path history, learning an accepted suggestion, and the Ctrl-R history picker (`select_history` / `select_history_with_skim`).
use super::*;

impl<'a> Repl<'a> {
    pub(super) fn save_history(&mut self) {
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

    pub(super) fn save_history_periodic(&mut self) {
        // Command history is auto-saved by SQLite
        Self::save_single_history_helper(&mut self.shell.path_history, "path", true);
    }
    pub(super) fn learn_suggestion(&self, suggestion: &str) {
        if let Some(history) = &self.shell.cmd_history
            && let Some(mut history) = history.try_lock()
            && let Err(e) = history.write_history(suggestion)
        {
            warn!("Failed to learn suggestion: {}", e);
        }
    }

    pub(super) fn stop_history_mode(&mut self) {
        self.history_search = None;
        if let Some(ref mut history) = self.shell.cmd_history
            && let Some(mut history) = history.try_lock()
        {
            history.search_word = None;
            history.reset_index();
        }
        // If we can't get the lock, we just won't be able to stop history mode - no warning needed
    }

    pub(super) fn get_completion_from_history(&mut self, input: &str) -> Option<String> {
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
}

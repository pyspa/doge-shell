//! History-related helpers pulled out of the main `impl Repl`: saving path history and the Ctrl-R history picker (`select_history` / `select_history_with_skim`).
use super::*;

pub(super) struct HistoryDraft {
    pub(super) text: String,
    pub(super) cursor: usize,
    pub(super) recalled_revision: u64,
}

impl<'a> Repl<'a> {
    pub(super) fn save_history(&mut self) {
        // Wait for an in-flight directory write before the process exits.
        if let Some(history) = &self.shell.path_history
            && let Err(err) = history.lock().save()
        {
            warn!("Failed to save directory history: {err}");
        }
    }

    pub(super) fn save_history_periodic(&mut self) {
        if let Some(history) = &self.shell.path_history
            && let Some(mut history) = history.try_lock()
        {
            history.save_background();
        }
    }

    pub(super) fn stop_history_mode(&mut self) {
        self.history_draft = None;
        if let Some(ref mut history) = self.shell.cmd_history
            && let Some(mut history) = history.try_lock()
        {
            history.search_word = None;
            history.reset_index();
        }
        // The next recall resets the index whenever there is no saved draft.
    }

    pub(super) fn discard_edited_history_draft(&mut self) {
        if self
            .history_draft
            .as_ref()
            .is_some_and(|draft| draft.recalled_revision != self.input.edit_revision())
        {
            self.stop_history_mode();
        }
    }

    pub(crate) fn analyze_input(&self, input: &str) -> InputAnalysis {
        input_analysis::analyze_input(self, input)
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
    /// filters and the per-entry metadata. `DOGESH_HISTORY_PICKER=skim` selects the
    /// previous skim-based flow for one release.
    pub fn select_history(&mut self) -> Result<ReplControlFlow> {
        if history_picker_backend_is_skim() {
            return self.select_history_with_skim();
        }

        let Some(history_arc) = self.shell.cmd_history.as_ref() else {
            return Ok(ReplControlFlow::Continue);
        };
        let Some(history) = history_arc.try_lock() else {
            warn!("Failed to acquire command history lock for history selection - lock is busy");
            return Ok(ReplControlFlow::Continue);
        };

        let entries = history.snapshot_entries(Self::HISTORY_PICKER_SNAPSHOT);
        let ledger_mode = self
            .shell
            .environment
            .read()
            .variable_state
            .command_ledger_mode;
        let base = crate::history::query_context(self.shell.session_id.clone());
        let picker = crate::history::picker::HistoryPicker::new(
            entries,
            base,
            self.input.as_str().to_string(),
            chrono::Local::now().timestamp(),
        )
        .with_execution_loader(ledger_mode, || {
            history.picker_execution_metadata(Self::HISTORY_PICKER_SNAPSHOT)
        });
        // Released before the interactive session: the picker owns a snapshot,
        // and holding the lock would block the background history writer.
        drop(history);

        if picker.total() == 0 && ledger_mode == crate::history::CommandLedgerMode::Off {
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

        Ok(ReplControlFlow::RunInteractive(Box::new(move || {
            Ok(crate::history::picker::run(picker)?
                .map(|text| crate::repl::state::InteractiveAction::ReplaceAll { text }))
        })))
    }

    /// Compatibility picker, still requiring an explicit choice for a single match.
    fn select_history_with_skim(&mut self) -> Result<ReplControlFlow> {
        let Some(history) = self
            .shell
            .cmd_history
            .as_ref()
            .and_then(|history| history.try_lock())
        else {
            return Ok(ReplControlFlow::Continue);
        };
        let query = self.input.as_str().to_owned();
        let history_query = crate::history::HistoryQuery {
            text: (!query.is_empty()).then(|| query.clone()),
            limit: Some(500),
            ..Default::default()
        };
        let items: Vec<_> = history
            .search_entries(&history_query)
            .into_iter()
            .map(|entry| completion_lib::Candidate::Basic(entry.entry.clone()))
            .collect();
        drop(history);

        if items.is_empty() {
            return Ok(ReplControlFlow::Continue);
        }
        Ok(ReplControlFlow::RunInteractive(Box::new(move || {
            use completion_lib::framework::SkimCompletionFramework;

            Ok(
                SkimCompletionFramework::pick_with_skim(items, Some(query), false)
                    .map(|text| crate::repl::state::InteractiveAction::ReplaceAll { text }),
            )
        })))
    }
}

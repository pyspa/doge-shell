//! Input preferences and settings.

use super::Environment;
use crate::suggestion::{InputPreferences, SuggestionMode};

impl Environment {
    /// Register a periodic task, capturing the current child-process
    /// environment so the task sees the PATH and exports in effect now.
    pub(crate) fn sched_add(
        &mut self,
        spec: dsh_types::schedule::SchedTaskSpec,
    ) -> Result<u64, String> {
        let env = self.child_process_env();
        let scheduler = self.scheduler.clone();

        scheduler.write().add(spec, env)
    }

    pub(crate) fn sched_remove(&mut self, selector: &str) -> Result<String, String> {
        let scheduler = self.scheduler.clone();

        scheduler.write().remove(selector)
    }

    pub(crate) fn sched_set_paused(
        &mut self,
        selector: &str,
        paused: bool,
    ) -> Result<String, String> {
        let scheduler = self.scheduler.clone();

        scheduler.write().set_paused(selector, paused)
    }

    /// Task summaries as `"name every 5m -> command"`, for `(sched-list)`.
    pub(crate) fn sched_descriptions(&self) -> Vec<String> {
        let scheduler = self.scheduler.clone();
        let views = scheduler.read().views();
        views
            .into_iter()
            .map(|view| {
                format!(
                    "{} every {} -> {}{}",
                    view.name,
                    view.interval,
                    view.command,
                    if view.paused { " (paused)" } else { "" }
                )
            })
            .collect()
    }

    /// Bind a key chord, replacing any existing binding for it.
    pub(crate) fn set_key_binding(
        &mut self,
        chord: crate::repl::keybind::chord::Chord,
        action: crate::repl::keybind::BoundAction,
    ) {
        self.variable_state.keybindings.insert(chord, action);
    }

    /// Remove a binding, reporting whether one existed.
    pub(crate) fn remove_key_binding(
        &mut self,
        chord: &[crate::repl::keybind::chord::KeyStroke],
    ) -> bool {
        self.variable_state.keybindings.remove(chord)
    }

    /// Configured bindings rendered as `"key -> action"`.
    pub(crate) fn key_binding_descriptions(&self) -> Vec<String> {
        use crate::repl::keybind::BoundAction;
        self.variable_state
            .keybindings
            .entries()
            .into_iter()
            .map(|(chord, action)| {
                let target = match action {
                    BoundAction::Action(action) => {
                        crate::repl::keybind::action_name::name_of(action)
                            .unwrap_or("<unnamed>")
                            .to_string()
                    }
                    BoundAction::Lisp(name) => format!("lisp:{name}"),
                };
                format!("{chord} -> {target}")
            })
            .collect()
    }

    /// Get the current suggestion mode.
    pub fn suggestion_mode(&self) -> SuggestionMode {
        self.completion_state.input_preferences.suggestion_mode
    }

    /// Set the suggestion mode.
    pub fn set_suggestion_mode(&mut self, mode: SuggestionMode) {
        self.completion_state.input_preferences.suggestion_mode = mode;
    }

    /// Check if AI suggestions are enabled.
    pub fn suggestion_ai_enabled(&self) -> bool {
        self.completion_state.input_preferences.ai_backfill
    }

    /// Enable or disable AI suggestions.
    pub fn set_suggestion_ai_enabled(&mut self, enabled: bool) {
        self.completion_state.input_preferences.ai_backfill = enabled;
    }

    /// Enable or disable auto-fix.
    pub fn set_auto_fix_enabled(&mut self, enabled: bool) {
        self.completion_state.input_preferences.auto_fix = enabled;
    }

    /// Enable or disable auto-notify.
    pub fn set_auto_notify_enabled(&mut self, enabled: bool) {
        self.completion_state.input_preferences.auto_notify_enabled = enabled;
    }

    /// Set the auto-notify threshold.
    pub fn set_auto_notify_threshold(&mut self, threshold: u64) {
        self.completion_state
            .input_preferences
            .auto_notify_threshold = threshold;
    }

    /// Enable or disable auto-pair.
    pub fn set_auto_pair_enabled(&mut self, enabled: bool) {
        self.completion_state.input_preferences.auto_pair = enabled;
    }

    /// Check if AI command explanation is enabled.
    pub fn ai_explanation_enabled(&self) -> bool {
        self.completion_state.input_preferences.ai_explanation
    }

    /// Enable or disable AI command explanation.
    pub fn set_ai_explanation_enabled(&mut self, enabled: bool) {
        self.completion_state.input_preferences.ai_explanation = enabled;
    }

    /// Enable or disable the bottom-row status line.
    pub fn set_status_line_enabled(&mut self, enabled: bool) {
        self.completion_state.input_preferences.status_line = enabled;
    }

    /// Get the current input preferences.
    pub fn input_preferences(&self) -> InputPreferences {
        self.completion_state.input_preferences
    }
}

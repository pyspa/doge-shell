//! Input preferences and settings.

use super::Environment;
use crate::suggestion::{InputPreferences, SuggestionMode};

impl Environment {
    /// Get the current suggestion mode.
    pub fn suggestion_mode(&self) -> SuggestionMode {
        self.input_preferences.suggestion_mode
    }

    /// Set the suggestion mode.
    pub fn set_suggestion_mode(&mut self, mode: SuggestionMode) {
        self.input_preferences.suggestion_mode = mode;
    }

    /// Check if AI suggestions are enabled.
    pub fn suggestion_ai_enabled(&self) -> bool {
        self.input_preferences.ai_backfill
    }

    /// Enable or disable AI suggestions.
    pub fn set_suggestion_ai_enabled(&mut self, enabled: bool) {
        self.input_preferences.ai_backfill = enabled;
    }

    /// Enable or disable auto-fix.
    pub fn set_auto_fix_enabled(&mut self, enabled: bool) {
        self.input_preferences.auto_fix = enabled;
    }

    /// Enable or disable auto-notify.
    pub fn set_auto_notify_enabled(&mut self, enabled: bool) {
        self.input_preferences.auto_notify_enabled = enabled;
    }

    /// Set the auto-notify threshold.
    pub fn set_auto_notify_threshold(&mut self, threshold: u64) {
        self.input_preferences.auto_notify_threshold = threshold;
    }

    /// Enable or disable auto-pair.
    pub fn set_auto_pair_enabled(&mut self, enabled: bool) {
        self.input_preferences.auto_pair = enabled;
    }

    /// Get the current input preferences.
    pub fn input_preferences(&self) -> InputPreferences {
        self.input_preferences
    }
}

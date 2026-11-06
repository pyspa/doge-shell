//! Coherent read of the runtime owners used by `doctor config`.
use crate::shell::Shell;
use dsh_builtin::effective_config::{CommandLedgerMode, EffectiveConfig, SuggestionMode};
use dsh_builtin::shell_capabilities::EffectiveConfigCapability;
use dsh_types::shell_options::ShellOption;

impl EffectiveConfigCapability for Shell {
    fn effective_config(&self) -> Option<EffectiveConfig> {
        let env = self.environment.read();
        let prefs = env.input_preferences();
        Some(EffectiveConfig {
            suggestion_mode: match prefs.suggestion_mode {
                crate::suggestion::SuggestionMode::Off => SuggestionMode::Off,
                crate::suggestion::SuggestionMode::Ghost => SuggestionMode::Ghost,
            },
            transient_prompt: prefs.transient_prompt,
            auto_pair: prefs.auto_pair,
            status_line: prefs.status_line,
            failure_hint: prefs.failure_hint,
            auto_notify_enabled: prefs.auto_notify_enabled,
            auto_notify_threshold: prefs.auto_notify_threshold,
            command_ledger_mode: match env.variable_state.command_ledger_mode {
                crate::history::CommandLedgerMode::Off => CommandLedgerMode::Off,
                crate::history::CommandLedgerMode::Metadata => CommandLedgerMode::Metadata,
                crate::history::CommandLedgerMode::Output => CommandLedgerMode::Output,
            },
            pipefail: env.shell_options.enabled(ShellOption::Pipefail),
        })
    }
}

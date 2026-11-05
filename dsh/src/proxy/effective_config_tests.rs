use crate::environment::Environment;
use crate::shell::Shell;
use dsh_builtin::effective_config::{CommandLedgerMode, SuggestionMode};
use dsh_builtin::shell_capabilities::EffectiveConfigCapability;
use dsh_types::shell_options::ShellOption;

#[test]
fn effective_config_reads_defaults_then_live_runtime_changes() {
    let _lock = crate::test_env_lock();
    let home = tempfile::tempdir().unwrap();
    let _home = crate::ProcessEnvGuard::set("HOME", home.path().to_str().unwrap());
    let _config = crate::ProcessEnvGuard::set("XDG_CONFIG_HOME", home.path().to_str().unwrap());
    let environment = Environment::new();
    let shell = Shell::new(environment.clone());
    let defaults = shell.effective_config().unwrap();
    assert_eq!(defaults.suggestion_mode, SuggestionMode::Ghost);
    assert!(defaults.transient_prompt);
    assert!(!defaults.auto_pair);
    assert!(!defaults.status_line);
    assert!(defaults.failure_hint);
    assert!(!defaults.auto_notify_enabled);
    assert_eq!(defaults.auto_notify_threshold, 10);
    assert_eq!(defaults.command_ledger_mode, CommandLedgerMode::Off);
    assert!(!defaults.pipefail);
    {
        let mut env = environment.write();
        env.set_suggestion_mode(crate::suggestion::SuggestionMode::Off);
        env.completion_state.input_preferences.transient_prompt = false;
        env.set_auto_pair_enabled(true);
        env.set_status_line_enabled(true);
        env.set_failure_hint_enabled(false);
        env.set_auto_notify_enabled(true);
        env.set_auto_notify_threshold(37);
        env.variable_state.command_ledger_mode = crate::history::CommandLedgerMode::Metadata;
        env.shell_options.set(ShellOption::Pipefail, true);
    }
    let live = shell.effective_config().unwrap();
    assert_eq!(live.suggestion_mode, SuggestionMode::Off);
    assert!(!live.transient_prompt);
    assert!(live.auto_pair);
    assert!(live.status_line);
    assert!(!live.failure_hint);
    assert!(live.auto_notify_enabled);
    assert_eq!(live.auto_notify_threshold, 37);
    assert_eq!(live.command_ledger_mode, CommandLedgerMode::Metadata);
    assert!(live.pipefail);
    environment.write().variable_state.command_ledger_mode =
        crate::history::CommandLedgerMode::Output;
    assert_eq!(
        shell.effective_config().unwrap().command_ledger_mode,
        CommandLedgerMode::Output
    );
}

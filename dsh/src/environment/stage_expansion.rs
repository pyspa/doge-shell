//! In-process isolated expansion environment; never owns shell resources.
use super::*;
fn capture_output(parent: &Environment) -> HashMap<String, String> {
    if let Some(values) = &parent.expansion_output {
        return values.clone();
    }
    let history = &parent.session_output_state.output_history;
    let mut values = HashMap::new();
    for index in 1..=history.len() {
        for (name, value) in [
            ("OUT", history.get_stdout(index)),
            ("ERR", history.get_stderr(index)),
        ] {
            if let Some(value) = value {
                values.insert(format!("{name}[{index}]"), value.to_string());
                if index == 1 {
                    values.insert(name.to_string(), value.to_string());
                }
            }
        }
    }
    values
}
impl Environment {
    pub(crate) fn isolated_expansion(parent: &Environment) -> Arc<RwLock<Self>> {
        let (variable_state, shell_options, invocation) = {
            (
                VariableState {
                    alias: parent.variable_state.alias.clone(),
                    abbreviations: parent.variable_state.abbreviations.clone(),
                    command_abbreviations: parent.variable_state.command_abbreviations.clone(),
                    command_ledger_mode: parent.variable_state.command_ledger_mode,
                    paths: parent.variable_state.paths.clone(),
                    variables: parent.variable_state.variables.clone(),
                    exported_vars: parent.variable_state.exported_vars.clone(),
                    direnv_roots: parent.variable_state.direnv_roots.clone(),
                    chpwd_hooks: Vec::new(),
                    z_exclude: parent.variable_state.z_exclude.clone(),
                    keybindings: parent.variable_state.keybindings.clone(),
                },
                // `ShellOptions` is `Copy`: the child gets the parent's
                // values without sharing mutable state.
                parent.shell_options,
                parent.invocation.clone(),
            )
        };
        let (integration_state, policy_state, completion_state) = {
            (
                IntegrationState {
                    mcp_servers: parent.integration_state.mcp_servers.clone(),
                    mcp_manager: parent.integration_state.mcp_manager.clone(),
                    response_language: Arc::new(RwLock::new(
                        parent.integration_state.response_language.read().clone(),
                    )),
                    chat_model: Arc::new(RwLock::new(
                        parent.integration_state.chat_model.read().clone(),
                    )),
                    ai_client: Arc::new(RwLock::new(
                        parent.integration_state.ai_client.read().clone(),
                    )),
                    ai_service: None,
                    lifecycle: parent.integration_state.lifecycle.clone(),
                },
                PolicyState {
                    execute_allowlist: parent.policy_state.execute_allowlist.clone(),
                    shell_always_allowlist: parent.policy_state.shell_always_allowlist.clone(),
                    agent_session_allowlist: parent.policy_state.agent_session_allowlist.clone(),
                    safety_level: parent.policy_state.safety_level.clone(),
                    secret_manager: SecretManager::new(),
                },
                CompletionState {
                    input_preferences: parent.completion_state.input_preferences,
                    command_cache: RwLock::new(HashMap::new()),
                    executable_names: Arc::new(RwLock::new(Vec::new())),
                    path_generation: 0,
                },
            )
        };

        Arc::new(RwLock::new(Environment {
            last_exit_status: parent.last_exit_status,
            last_async_pid: parent.last_async_pid,
            invocation,
            variable_state,
            policy_state,
            integration_state,
            session_output_state: SessionOutputState {
                output_history: OutputHistory::new(),
                command_blocks: CommandBlockHistory::new(),
            },
            completion_state,
            dir_stack: parent.dir_stack.clone(),
            // A fresh cache: only the interactive session runs the cron
            // runner that keeps this one warm, so a subshell would otherwise
            // show stale numbers instead of simply showing none.
            cron_health: Arc::new(RwLock::new(dsh_types::cron::job::CronHealth::default())),
            startup_mode: false, // Expansion is runtime, never startup configuration.
            shell_options,
            isolated_projection: true,
            expansion_output: Some(capture_output(parent)),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolated_state_inherits_specials_and_projects_writes_locally() {
        let parent = Environment::new();
        {
            let mut env = parent.write();
            env.last_exit_status = 7;
            env.last_async_pid = Some(4242);
            env.dir_stack = vec!["one".into(), "two".into()];
            env.set_shell_var("HOME".into(), "iso-home".into());
            env.set_shell_var("IFS".into(), ":".into());
            env.set_and_export_shell_var("DOGESH_ISO_X".into(), "1".into());
            env.set_shell_var("DOGESH_ISO_EMPTY".into(), String::new());
            env.session_output_state.output_history.push(
                dsh_types::output_history::OutputEntry::new(
                    "probe".into(),
                    "out".into(),
                    "err".into(),
                    0,
                ),
            );
        }
        let a = Environment::isolated_expansion(&parent.read());
        let b = Environment::isolated_expansion(&parent.read());
        for name in [
            "?",
            "!",
            "HOME",
            "IFS",
            "OUT",
            "OUT[1]",
            "OUT[01]",
            "ERR",
            "MCP_SERVERS",
            "MCP_CONNECTED",
            "MCP_TOOLS",
            "MCP_ACTIVE_TOOLS",
            "MCP_ACTIVE_GROUPS",
        ] {
            assert_eq!(
                a.read().lookup_variable(name),
                parent.read().lookup_variable(name),
                "{name}"
            );
        }
        assert_eq!(a.read().dir_stack, parent.read().dir_stack);
        assert_eq!(
            a.read().lookup_variable("DOGESH_ISO_EMPTY").as_deref(),
            Some("")
        );
        assert_eq!(a.read().lookup_variable("DOGESH_ISO_ABSENT"), None);
        {
            let mut env = a.write();
            env.set_shell_var("$DOGESH_ISO_X".into(), "9".into());
            env.set_shell_var("PATH".into(), "iso-bin".into());
            env.set_shell_var("AI_MESSAGE_LANG".into(), "iso-language".into());
            env.set_shell_var("AI_CHAT_MODEL".into(), "iso-model".into());
        }
        assert_eq!(
            a.read().lookup_variable("DOGESH_ISO_X").as_deref(),
            Some("9")
        );
        assert_eq!(a.read().child_process_env()["DOGESH_ISO_X"], "9");
        assert_eq!(a.read().variable_state.paths, vec!["iso-bin"]);
        assert_eq!(
            a.read()
                .integration_state
                .response_language
                .read()
                .as_deref(),
            Some("iso-language")
        );
        for other in [&parent, &b] {
            assert_eq!(
                other.read().lookup_variable("DOGESH_ISO_X").as_deref(),
                Some("1")
            );
            assert_ne!(other.read().variable_state.paths, vec!["iso-bin"]);
            assert_ne!(
                other
                    .read()
                    .integration_state
                    .response_language
                    .read()
                    .as_deref(),
                Some("iso-language")
            );
            assert_ne!(
                other.read().integration_state.chat_model.read().as_deref(),
                Some("iso-model")
            );
        }
        // A second generation preserves the plain output view, including indexed reads.
        let again = Environment::isolated_expansion(&a.read());
        assert_eq!(
            again.read().lookup_variable("OUT[1]").as_deref(),
            Some("out")
        );
    }
}

//! `AgentCommandPolicy for Shell`: judges whether a durable agent task may run a command, touch a file, or call an MCP tool without asking, given the task's own grants or the operator's safety level/allowlist.
use super::*;

impl Shell {
    /// The commands the chat agent may run without asking.
    ///
    /// Only the operator's configured list. Deliberately *not* the list the
    /// user's own interactive approvals feed - see
    /// `PolicyState::shell_always_allowlist` - and not this session's "always"
    /// answers either, which are matched exactly rather than by prefix.
    fn agent_allowlist_snapshot(&self) -> Vec<String> {
        self.environment
            .read()
            .policy_state
            .execute_allowlist
            .read()
            .clone()
    }

    /// The one place the level is read from.
    pub(super) fn safety_level_snapshot(&self) -> dsh_types::safety_policy::SafetyLevel {
        *self.environment.read().policy_state.safety_level.read()
    }

    /// The tool's own name behind the namespaced one the model called.
    ///
    /// Falls back to the namespaced name when no binding matches, so an
    /// unknown call is still judged rather than skipped.
    fn agent_mcp_tool_name(&self, function_name: &str) -> String {
        self.environment
            .read()
            .integration_state
            .mcp_manager
            .read()
            .tool_name_for(function_name)
            .unwrap_or_else(|| function_name.to_string())
    }
}

impl AgentCommandPolicy for Shell {
    fn agent_runtime(
        &self,
    ) -> Option<std::sync::Arc<parking_lot::Mutex<dsh_builtin::agent::AgentRuntime>>> {
        self.agent_runtime.clone()
    }
    fn evaluate_agent_file(&mut self, path: &std::path::Path, write: bool) -> AgentCommandVerdict {
        if let Some(runtime) = &self.agent_runtime
            && self
                .safety_guard
                .task_file_allowed(&runtime.lock().task.grant, path, write)
        {
            return AgentCommandVerdict::Allowed;
        }
        AgentCommandVerdict::Confirm("outside task file grants".into())
    }
    fn evaluate_agent_command(&mut self, command: &str) -> AgentCommandVerdict {
        // `get_jobs` parses *and evaluates*: `shell::parse::parse_command` calls
        // `capture_subshell_stdout` for `$(...)`, `(...)` and `<(...)`, so
        // handing one of those to a safety check would run it before anyone
        // approved it. The `execute` tool refuses them for that reason; this
        // repeats the refusal here so the trait cannot become a way to run a
        // command by asking whether it is safe.
        if let Some(construct) = dsh_types::safety_policy::substitution_construct(command) {
            return AgentCommandVerdict::Denied(format!("{construct} cannot be evaluated safely"));
        }

        // The guard judges what dsh's grammar can see; `sh -c` runs the whole
        // line. A construct the grammar cannot consume - `{ rm -rf ~; }`, a
        // heredoc - parses as a prefix and the rest is only *warned* about
        // (`report_unparsed_tail`), so the verdict would describe a different
        // command from the one that runs. Refuse instead.
        if let Some(tail) = crate::shell::eval::unconsumed_tail(self, command) {
            return AgentCommandVerdict::Denied(format!(
                "the shell's parser cannot read all of it (`{tail}` is left over),                  so it cannot be judged before it runs"
            ));
        }

        // A compound statement parses as a command named `{` / `for` / `if`,
        // which has no rule, so everything inside it went unjudged while
        // `sh -c` ran the whole thing.
        for segment in dsh_types::safety_policy::split_command_segments(command) {
            let leading = segment.split_whitespace().next().unwrap_or_default();
            if let Some(keyword) = dsh_types::safety_policy::compound_statement_keyword(leading) {
                return AgentCommandVerdict::Denied(format!(
                    "`{keyword}` starts a compound statement, and the commands inside one \
                     cannot be judged before it runs; write them as separate commands"
                ));
            }
        }

        // Parse the whole line, so a pipeline is judged as a pipeline. This is
        // the same path the user's own input takes.
        let jobs = match crate::shell::eval::get_jobs(self, command) {
            Ok(jobs) if !jobs.is_empty() => jobs,
            Ok(_) => {
                return AgentCommandVerdict::Denied("command is empty".to_string());
            }
            Err(err) => {
                return AgentCommandVerdict::Denied(format!("command could not be parsed: {err}"));
            }
        };

        let allowlist = self.agent_allowlist_snapshot();
        let level = self.safety_level_snapshot();

        if let Some(runtime) = &self.agent_runtime {
            let runtime = runtime.lock();
            if self
                .safety_guard
                .task_command_allowed(&runtime.task.grant, command)
            {
                return AgentCommandVerdict::Allowed;
            }
            return AgentCommandVerdict::Confirm(
                "command is not in the task's exact command grants".into(),
            );
        }
        match self.safety_guard.check_jobs(&jobs, &level, &allowlist) {
            SafetyResult::Allowed => AgentCommandVerdict::Allowed,
            SafetyResult::Confirm(reason) => AgentCommandVerdict::Confirm(reason),
        }
    }

    fn request_agent_approval(&mut self, message: &str) -> Result<ApprovalDecision> {
        if let Some(runtime) = &self.agent_runtime {
            let mut runtime = runtime.lock();
            runtime.task.status = dsh_types::agent::TaskStatus::InputRequired;
            runtime.task.stop_reason = Some(message.to_string());
            runtime.save(None)?;
            return Ok(ApprovalDecision::Deny);
        }
        let lifecycle = crate::agent_lifecycle::current(self);
        let _blocked = lifecycle.begin_blocked(message);
        Ok(match crate::repl::confirmation::confirm_action(message)? {
            ConfirmationAction::Yes => ApprovalDecision::Allow,
            ConfirmationAction::AlwaysAllow => ApprovalDecision::AllowAlways,
            ConfirmationAction::No => ApprovalDecision::Deny,
        })
    }

    fn remember_agent_approval(&mut self, command: &str) {
        let environment = self.environment.read();
        let mut session = environment.policy_state.agent_session_allowlist.write();
        let entry = command.trim().to_string();
        if !entry.is_empty() && !session.contains(&entry) {
            session.push(entry);
        }
    }

    fn agent_session_approvals(&mut self) -> Vec<String> {
        self.environment
            .read()
            .policy_state
            .agent_session_allowlist
            .read()
            .clone()
    }

    fn agent_allowlist(&mut self) -> Vec<String> {
        self.agent_allowlist_snapshot()
    }

    fn evaluate_agent_tool(&mut self, name: &str, arguments: &str) -> AgentCommandVerdict {
        // The same judgement the shell-side AI service already applied to MCP
        // calls. The `!` runtime used to ask about every one of them regardless
        // of the level, so `loose` still prompted and a read-only tool was
        // treated like a destructive one.
        if let Some(runtime) = &self.agent_runtime {
            let entry = crate::safety::SafetyGuard::mcp_allowlist_entry(name, arguments);
            if self
                .safety_guard
                .task_mcp_allowed(&runtime.lock().task.grant, &entry)
            {
                return AgentCommandVerdict::Allowed;
            }
            return AgentCommandVerdict::Confirm(format!(
                "external operation needs an exact task grant: {entry}"
            ));
        }
        let mut allowlist = self.agent_allowlist_snapshot();
        allowlist.extend(self.agent_session_approvals());
        let level = self.safety_level_snapshot();
        let tool_name = self.agent_mcp_tool_name(name);

        match self
            .safety_guard
            .check_mcp_tool(name, &tool_name, arguments, &level, &allowlist)
        {
            SafetyResult::Allowed => AgentCommandVerdict::Allowed,
            SafetyResult::Confirm(reason) => AgentCommandVerdict::Confirm(reason),
        }
    }

    fn agent_tool_approval_entry(&mut self, name: &str, arguments: &str) -> String {
        crate::safety::SafetyGuard::mcp_allowlist_entry(name, arguments)
    }

    fn agent_mcp_manager(
        &mut self,
    ) -> std::sync::Arc<parking_lot::RwLock<dsh_builtin::McpManager>> {
        self.environment
            .read()
            .integration_state
            .mcp_manager
            .clone()
    }
}

#[cfg(test)]
mod agent_policy_tests {
    use super::*;
    use crate::environment::Environment;
    use crate::shell::Shell;

    fn shell() -> Shell {
        Shell::new(Environment::new())
    }

    /// The guard reads what dsh's grammar can parse; `sh -c` runs the whole
    /// line. A construct the grammar cannot consume used to be judged on its
    /// prefix and only *warned* about, so `{ rm -rf ~; }` was classified as a
    /// command called `{` - which has no rule - and ran unasked.
    #[test]
    fn a_line_the_parser_cannot_finish_is_refused() {
        let mut shell = shell();

        for command in [
            "{ rm -rf /; }",
            "for f in *; do rm -rf $f; done",
            "echo a )",
            "echo unterminated\"",
        ] {
            assert!(
                matches!(
                    shell.evaluate_agent_command(command),
                    AgentCommandVerdict::Denied(_)
                ),
                "{command} should have been refused"
            );
        }
    }

    /// The refusal must not swallow ordinary lines.
    #[test]
    fn an_ordinary_line_is_still_judged_on_its_merits() {
        let mut shell = shell();

        assert_eq!(
            shell.evaluate_agent_command("echo hello"),
            AgentCommandVerdict::Allowed
        );
        assert!(matches!(
            shell.evaluate_agent_command("sudo rm -rf /"),
            AgentCommandVerdict::Confirm(_)
        ));
        assert!(matches!(
            shell.evaluate_agent_command("true | rm -rf /"),
            AgentCommandVerdict::Confirm(_)
        ));
    }
}

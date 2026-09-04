//! Shell proxy implementation for builtin command dispatch.
//!
//! This module provides the `ShellProxy` trait implementation for `Shell`,
//! routing builtin commands to their respective handlers.

mod builtin;
mod external;

use crate::repl::confirmation::ConfirmationAction;
use crate::safety::SafetyResult;
use crate::shell::Shell;
use anyhow::{Context as _, Result};
use dsh_builtin::shell_capabilities::{AgentCommandPolicy, AgentCommandVerdict, ApprovalDecision};
use dsh_builtin::{CoreShellAction, ShellProxy};
use dsh_types::{Context, mcp::McpServerConfig};
use globmatch;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Bring a stopped job back to the foreground, as the `fg` builtin does.
///
/// Exposed so the Ctrl-Z key binding can reuse the builtin's tcsetpgrp/SIGCONT
/// handling instead of reimplementing it, without widening the whole `builtin`
/// module's visibility.
pub(crate) fn resume_job_foreground(shell: &mut Shell, ctx: &Context, job_id: usize) -> Result<()> {
    builtin::jobs::execute_fg(shell, ctx, vec!["fg".to_string(), job_id.to_string()])
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn is_same_direnv_root(requested: &Path, allowed: &Path) -> bool {
    canonical_or_original(requested) == canonical_or_original(allowed)
}

fn read_confirmation_line(input: &mut String) -> io::Result<usize> {
    let stdin = io::stdin();
    if should_read_confirmation_from_tty(stdin.is_terminal())
        && let Ok(tty) = OpenOptions::new().read(true).open("/dev/tty")
    {
        let mut reader = BufReader::new(tty);
        return reader.read_line(input);
    }

    stdin.read_line(input)
}

fn should_read_confirmation_from_tty(stdin_is_terminal: bool) -> bool {
    stdin_is_terminal
}

fn confirmation_is_yes(input: &str) -> bool {
    input.trim().to_lowercase() == "y"
}

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
    fn safety_level_snapshot(&self) -> dsh_types::safety_policy::SafetyLevel {
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

        match self.safety_guard.check_jobs(&jobs, &level, &allowlist) {
            SafetyResult::Allowed => AgentCommandVerdict::Allowed,
            SafetyResult::Confirm(reason) => AgentCommandVerdict::Confirm(reason),
        }
    }

    fn request_agent_approval(&mut self, message: &str) -> Result<ApprovalDecision> {
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

impl ShellProxy for Shell {
    fn exit_shell(&mut self) {
        self.exit();
    }

    fn get_github_status(&self) -> (usize, usize, usize) {
        if let Some(ref status) = self.github_status {
            let status = status.read();
            (
                status.review_count,
                status.mention_count,
                status.other_count,
            )
        } else {
            (0, 0, 0)
        }
    }

    fn get_git_branch(&self) -> Option<String> {
        let output = std::process::Command::new("git")
            .arg("branch")
            .arg("--show-current")
            .output()
            .ok()?;
        if output.status.success() {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if branch.is_empty() {
                None
            } else {
                Some(branch)
            }
        } else {
            None
        }
    }

    fn get_job_count(&self) -> usize {
        self.wait_jobs.len()
    }

    fn save_path_history(&mut self, path: &str) {
        // Check exclusions
        {
            let env = self.environment.read();
            for pattern in &env.variable_state.z_exclude {
                if let Ok(matcher) = globmatch::Builder::new(pattern).build("/")
                    && matcher.is_match(path.into())
                {
                    debug!("dsh: path rejected by z_exclude: {}", path);
                    return;
                }
            }
        }

        if let Some(ref mut history) = self.path_history {
            let mut history = history.lock();
            history.add(path);
            history.save_background();
        }
    }

    fn save_output_history(&mut self, entry: dsh_types::output_history::OutputEntry) {
        self.environment
            .write()
            .session_output_state
            .output_history
            .push(entry);
    }

    fn changepwd(&mut self, path: &str) -> Result<()> {
        // Save current directory as OLDPWD before changing
        if let Ok(current) = std::env::current_dir() {
            let old_pwd = current.to_string_lossy().into_owned();
            self.environment
                .write()
                .variable_state
                .variables
                .insert("OLDPWD".to_string(), old_pwd);
        }

        std::env::set_current_dir(path)?;

        // Use the canonical path we actually landed in for history and hooks
        let final_path = if let Ok(canon) = std::env::current_dir() {
            canon.to_string_lossy().into_owned()
        } else {
            path.to_string()
        };

        // Slot 0 of the directory stack always mirrors the current directory,
        // so plain `cd` replaces the top without disturbing what pushd stacked
        // underneath. Every navigation path (cd, z, bookmark, pushd, popd)
        // funnels through here, which is what keeps the stack honest.
        {
            let mut env = self.environment.write();
            if env.dir_stack.is_empty() {
                env.dir_stack.push(final_path.clone());
            } else {
                env.dir_stack[0] = final_path.clone();
            }
        }

        self.save_path_history(&final_path);
        self.exec_chpwd_hooks(&final_path)?;
        Ok(())
    }

    fn dir_stack(&self) -> Vec<String> {
        self.environment.read().dir_stack.clone()
    }

    fn dir_stack_set(&mut self, stack: Vec<String>) {
        self.environment.write().dir_stack = stack;
    }

    fn sched_add(&mut self, spec: dsh_types::schedule::SchedTaskSpec) -> Result<u64, String> {
        // Snapshot the environment now: the task runs detached later, and
        // should see the PATH and exports that were in effect when it was
        // registered rather than whatever the session drifts to.
        let (scheduler, env) = {
            let environment = self.environment.read();
            (
                environment.scheduler.clone(),
                environment.child_process_env(),
            )
        };

        scheduler.write().add(spec, env)
    }

    fn sched_remove(&mut self, selector: &str) -> Result<String, String> {
        let scheduler = self.environment.read().scheduler.clone();

        scheduler.write().remove(selector)
    }

    fn sched_set_paused(&mut self, selector: &str, paused: bool) -> Result<String, String> {
        let scheduler = self.environment.read().scheduler.clone();

        scheduler.write().set_paused(selector, paused)
    }

    fn sched_trigger(&mut self, selector: &str) -> Result<String, String> {
        let scheduler = self.environment.read().scheduler.clone();

        scheduler.write().trigger(selector)
    }

    fn sched_list(&self) -> Vec<dsh_types::schedule::SchedTaskView> {
        let scheduler = self.environment.read().scheduler.clone();

        scheduler.read().views()
    }

    fn sched_as_lisp(&self) -> Vec<String> {
        let scheduler = self.environment.read().scheduler.clone();

        scheduler.read().as_lisp()
    }

    fn sched_enabled(&self) -> bool {
        let scheduler = self.environment.read().scheduler.clone();

        scheduler.read().enabled
    }

    fn sched_set_enabled(&mut self, enabled: bool) {
        let scheduler = self.environment.read().scheduler.clone();
        scheduler.write().set_enabled(enabled);
    }

    fn insert_path(&mut self, idx: usize, path: &str) {
        self.environment
            .write()
            .variable_state
            .paths
            .insert(idx, path.to_string());
    }

    fn dispatch(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()> {
        if let Some(action) = CoreShellAction::from_command_name(cmd) {
            self.dispatch_core_action(ctx, action, argv)
        } else {
            external::execute(ctx, cmd, argv, self.environment.clone())
        }
    }

    fn dispatch_core_action(
        &mut self,
        ctx: &Context,
        action: CoreShellAction,
        argv: Vec<String>,
    ) -> Result<()> {
        match action {
            CoreShellAction::Exit => builtin::exit::execute(self, ctx, argv),
            CoreShellAction::History => builtin::history::execute(self, ctx, argv),
            CoreShellAction::Reload => builtin::reload::execute(self, ctx, argv),
            CoreShellAction::Z => builtin::z::execute(self, ctx, argv),
            CoreShellAction::BlocksTui => builtin::blocks_tui::execute(self, ctx, argv),
            CoreShellAction::BlocksPersistent => {
                builtin::blocks_persistent::execute(self, ctx, argv)
            }
            CoreShellAction::Jobs => builtin::jobs::execute_jobs(self, ctx, argv),
            CoreShellAction::Foreground => builtin::jobs::execute_fg(self, ctx, argv),
            CoreShellAction::Background => builtin::jobs::execute_bg(self, ctx, argv),
            CoreShellAction::Lisp => builtin::lisp::execute_lisp(self, ctx, argv),
            CoreShellAction::LispRun => builtin::lisp::execute_lisp_run(self, ctx, argv),
            CoreShellAction::Var => builtin::var::execute_var(self, ctx, argv),
            CoreShellAction::Read => builtin::var::execute_read(self, ctx, argv),
            CoreShellAction::AbbrCommand => builtin::abbr::execute(self, ctx, argv),
        }
    }

    fn get_var(&mut self, key: &str) -> Option<String> {
        self.environment.read().get_var(key)
    }

    fn get_lisp_var(&self, key: &str) -> Option<String> {
        let lisp_engine = self.lisp_engine.borrow();
        let env = lisp_engine.env.borrow();
        match env.get(&crate::lisp::Symbol::from(key)) {
            Some(crate::lisp::Value::String(s)) => Some(s.clone()),
            Some(crate::lisp::Value::Int(i)) => Some(i.to_string()),
            _ => None,
        }
    }

    fn set_var(&mut self, key: String, value: String) {
        self.environment.write().set_shell_var(key, value);
    }

    fn set_env_var(&mut self, key: String, value: String) {
        let masked = if self
            .environment
            .read()
            .policy_state
            .secret_manager
            .is_sensitive_key(&key)
        {
            "<redacted>"
        } else {
            value.as_str()
        };
        debug!("set env {} {}", &key, masked);
        self.environment.write().set_system_env_var(key, value);
    }

    fn is_direnv_allowed(&self, path: &std::path::Path) -> bool {
        self.environment
            .read()
            .variable_state
            .direnv_roots
            .iter()
            .any(|root| is_same_direnv_root(path, Path::new(&root.path)))
    }

    fn unset_env_var(&mut self, key: &str) {
        debug!("unset env {}", key);
        self.environment.write().unset_system_env_var(key);
    }

    fn get_alias(&mut self, name: &str) -> Option<String> {
        debug!("Getting alias for: {}", name);
        self.environment
            .read()
            .variable_state
            .alias
            .get(name)
            .cloned()
    }

    fn set_alias(&mut self, name: String, command: String) {
        debug!("Setting alias: {} = {}", name, command);
        self.environment
            .write()
            .variable_state
            .alias
            .insert(name, command);
    }

    fn list_aliases(&mut self) -> std::collections::HashMap<String, String> {
        debug!("Listing all aliases");
        self.environment.read().variable_state.alias.clone()
    }

    fn add_abbr(&mut self, name: String, expansion: String) {
        debug!("Adding abbreviation: {} = {}", name, expansion);
        self.environment
            .write()
            .variable_state
            .abbreviations
            .insert(name, expansion);
    }

    fn remove_abbr(&mut self, name: &str) -> bool {
        debug!("Removing abbreviation: {}", name);
        self.environment
            .write()
            .variable_state
            .abbreviations
            .remove(name)
            .is_some()
    }

    fn list_abbrs(&self) -> Vec<(String, String)> {
        debug!("Listing all abbreviations");
        self.environment
            .read()
            .variable_state
            .abbreviations
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn get_abbr(&self, name: &str) -> Option<String> {
        debug!("Getting abbreviation for: {}", name);
        self.environment
            .read()
            .variable_state
            .abbreviations
            .get(name)
            .cloned()
    }

    fn list_mcp_servers(&mut self) -> Vec<McpServerConfig> {
        self.environment.read().mcp_servers().to_vec()
    }

    fn list_execute_allowlist(&mut self) -> Vec<String> {
        self.environment.read().execute_allowlist().to_vec()
    }

    /// Read the level the policy actually runs at, not the readable copy.
    ///
    /// The default implementation parses the `SAFETY_LEVEL` variable, which is
    /// only ever *written* by `(safety-level ...)`. That left the chat tools'
    /// sensitive-access gate reading one value while `evaluate_agent_command`
    /// and the MCP checks read another.
    fn safety_level(&mut self) -> dsh_types::safety_policy::SafetyLevel {
        self.safety_level_snapshot()
    }

    fn run_hook(&mut self, hook_name: &str, args: Vec<String>) -> Result<()> {
        // Ensure hook name is wrapped in asterisks for Lisp convention
        let hook_var = if hook_name.starts_with('*') {
            hook_name.to_string()
        } else {
            format!("*{}*", hook_name)
        };

        // Early return: skip Lisp code generation if no hooks are registered
        if !self.lisp_engine.borrow().is_bound_nonempty_list(&hook_var) {
            return Ok(());
        }

        let args_str = args
            .iter()
            .map(|a| format!("\"{}\"", a.replace("\"", "\\\"")))
            .collect::<Vec<_>>()
            .join(" ");

        let lisp_code = format!("(map (lambda (hook) (hook {args_str})) {hook_var})");

        if let Err(e) = self.lisp_engine.borrow().run(&lisp_code) {
            // We use warn! but return Ok because hook failure shouldn't crash the command
            warn!("Failed to execute hook {}: {}", hook_name, e);
        }
        Ok(())
    }

    fn select_item(&mut self, items: Vec<String>) -> Result<Option<String>> {
        let candidates: Vec<crate::completion::Candidate> = items
            .into_iter()
            .map(|item| crate::completion::Candidate::Item(item, "".to_string()))
            .collect();

        let res = crate::completion::select_item_with_skim(candidates, None);
        match res {
            crate::completion::CompletionSelection::Selected(val) => Ok(Some(val)),
            crate::completion::CompletionSelection::Interactive(items, query) => {
                use crate::completion::framework::SkimCompletionFramework;
                let query = query.unwrap_or_default();
                Ok(SkimCompletionFramework::run_with_skim(items, Some(query)))
            }
            crate::completion::CompletionSelection::None => Ok(None),
        }
    }

    // New method implementations for export
    fn list_exported_vars(&self) -> Vec<(String, String)> {
        let env = self.environment.read();
        env.variable_state
            .exported_vars
            .iter()
            .filter_map(|key| {
                env.variable_state
                    .variables
                    .get(key)
                    .map(|value| (key.clone(), value.clone()))
            })
            .collect()
    }

    fn export_var(&mut self, key: &str) -> bool {
        let mut env = self.environment.write();
        // Exporting a name that has no value yet is allowed: it takes effect
        // when the value arrives.
        let existed = env.variable_state.variables.contains_key(key);
        env.export_shell_var(key.to_string());
        existed
    }

    fn set_and_export_var(&mut self, key: String, value: String) {
        self.environment
            .write()
            .set_and_export_shell_var(key, value);
    }

    fn get_current_dir(&self) -> Result<std::path::PathBuf> {
        std::env::current_dir().context("failed to get current directory")
    }

    fn command_history_len(&self) -> Option<usize> {
        self.cmd_history
            .as_ref()
            .and_then(|history| history.try_lock())
            .map(|history| history.iter().count())
    }

    fn executable_cache_len(&self) -> Option<usize> {
        Some(
            self.environment
                .read()
                .completion_state
                .executable_names
                .read()
                .len(),
        )
    }

    fn completion_diagnostics(&self) -> Vec<String> {
        let mut lines = self
            .completion_runtime
            .as_ref()
            .map(|runtime| runtime.diagnostics_lines())
            .unwrap_or_else(|| vec!["completion-cache runtime inactive".to_string()]);
        let environment = self.environment.read();
        let fish_mode = crate::completion::dynamic::fish_fallback_mode_label(&environment);
        let fish_path = environment
            .lookup("fish")
            .unwrap_or_else(|| "missing".to_string());
        lines.push(format!(
            "completion-cache fish-fallback mode={} fish-path={}",
            fish_mode, fish_path
        ));
        lines
    }

    fn latency_probe_lines(&self, iterations: usize) -> Vec<String> {
        crate::perf_probes::run_default_probes(iterations)
            .into_iter()
            .map(|result| {
                let total_us = result.elapsed.as_micros();
                let avg_ns = result.elapsed.as_nanos() / result.iterations.max(1) as u128;
                format!(
                    "latency {} total={}us avg={}ns iterations={}",
                    result.name, total_us, avg_ns, result.iterations
                )
            })
            .collect()
    }

    fn confirm_action(&mut self, message: &str) -> Result<bool> {
        debug!("Safety confirmation requested: {}", message);

        // Use eprint! instead of println! or print! to ensure the prompt goes to stderr.
        // This is critical if the shell output is being piped.
        eprint!("{} [y/N]: ", message);
        std::io::stderr().flush()?;

        let mut input = String::new();
        read_confirmation_line(&mut input)?;

        let confirmed = confirmation_is_yes(&input);
        debug!("Confirmation result: {}", confirmed);
        Ok(confirmed)
    }

    fn is_canceled(&self) -> bool {
        crate::process::signal::check_and_clear_sigint()
    }

    fn get_full_output_history(&self) -> Vec<dsh_types::output_history::OutputEntry> {
        self.environment
            .read()
            .session_output_state
            .output_history
            .get_all_entries()
    }

    fn clear_output_history(&mut self) -> usize {
        let mut environment = self.environment.write();
        let removed = environment.session_output_state.output_history.len();
        environment.session_output_state.output_history.clear();
        removed
    }

    fn get_command_blocks(&self) -> Vec<dsh_types::command_block::CommandBlock> {
        self.environment
            .read()
            .session_output_state
            .command_blocks
            .get_all_blocks()
    }

    fn clear_command_blocks(&mut self) -> usize {
        let mut environment = self.environment.write();
        let removed = environment.session_output_state.command_blocks.len();
        environment.session_output_state.command_blocks.clear();
        removed
    }

    fn request_eval_command(&mut self, command: String) -> Result<()> {
        self.request_eval_command(command)
    }

    fn capture_command(&mut self, _ctx: &Context, cmd: &str) -> Result<(i32, String, String)> {
        use std::process::{Command, Stdio};

        // We implement this synchronously to avoid 'Cannot start a runtime from within a runtime' panic.
        debug!("Capturing command: '{}'", cmd);

        // Use sh -c to execute the command
        let output = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("Failed to capture command: {}", cmd))?;

        let exit_code = output.status.code().unwrap_or(1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        Ok((exit_code, stdout, stderr))
    }

    fn generate_command_completion_async<'a>(
        &'a mut self,
        command_name: &'a str,
        help_text: &'a str,
    ) -> dsh_builtin::ProxyFuture<'a, String> {
        let service = self.environment.read().integration_state.ai_service.clone();
        let command_name = command_name.to_string();
        let help_text = help_text.to_string();
        Box::pin(async move {
            let service = service.ok_or_else(|| anyhow::anyhow!("AI service not available"))?;
            crate::ai_features::generate_completion_json(
                service.as_ref(),
                &command_name,
                &help_text,
            )
            .await
        })
    }

    fn ask_ai_async<'a>(
        &'a mut self,
        messages: Vec<serde_json::Value>,
    ) -> dsh_builtin::ProxyFuture<'a, String> {
        let service = self.environment.read().integration_state.ai_service.clone();
        Box::pin(async move {
            let service = service.ok_or_else(|| anyhow::anyhow!("AI service not available"))?;
            service
                .send_request_with(
                    messages,
                    crate::ai_features::AiRequestOptions::new(Some(0.7)).without_tools(),
                )
                .await
        })
    }

    fn open_editor(&mut self, content: &str, extension: &str) -> Result<String> {
        crate::utils::editor::open_editor(content, extension)
    }

    fn add_snippet(&mut self, name: String, command: String, description: Option<String>) -> bool {
        crate::snippet::SnippetManager::open_default()
            .is_some_and(|manager| manager.add(&name, &command, description.as_deref()).is_ok())
    }

    fn remove_snippet(&mut self, name: &str) -> bool {
        crate::snippet::SnippetManager::open_default()
            .is_some_and(|manager| manager.remove(name).unwrap_or(false))
    }

    fn list_snippets(&self) -> Vec<dsh_types::snippet::Snippet> {
        crate::snippet::SnippetManager::open_default()
            .map(|manager| {
                manager
                    .list()
                    .unwrap_or_default()
                    .into_iter()
                    .map(to_wire_snippet)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn get_snippet(&self, name: &str) -> Option<dsh_types::snippet::Snippet> {
        crate::snippet::SnippetManager::open_default()?
            .get(name)
            .ok()
            .flatten()
            .map(to_wire_snippet)
    }

    fn update_snippet(&mut self, name: &str, command: &str, description: Option<&str>) -> bool {
        crate::snippet::SnippetManager::open_default()
            .is_some_and(|manager| manager.update(name, command, description).unwrap_or(false))
    }

    fn record_snippet_use(&mut self, name: &str) {
        if let Some(manager) = crate::snippet::SnippetManager::open_default() {
            let _ = manager.record_use(name);
        }
    }

    fn add_bookmark(&mut self, name: String, command: String) -> bool {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    let now = chrono::Utc::now().timestamp();
                    conn.execute(
                        "INSERT INTO bookmarks (name, command, created_at, use_count) VALUES (?1, ?2, ?3, 0)",
                        rusqlite::params![name, command, now],
                    ).is_ok()
                }
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    fn remove_bookmark(&mut self, name: &str) -> bool {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    conn.execute(
                        "DELETE FROM bookmarks WHERE name = ?1",
                        rusqlite::params![name],
                    )
                    .map(|c| c > 0)
                    .unwrap_or(false)
                }
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    fn list_bookmarks(&self) -> Vec<(String, String, i64)> {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    let mut stmt = match conn.prepare(
                        "SELECT name, command, use_count FROM bookmarks ORDER BY use_count DESC, name ASC",
                    ) {
                        Ok(s) => s,
                        Err(_) => return Vec::new(),
                    };
                    let rows = stmt.query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    });
                    rows.map(|r| r.filter_map(|r| r.ok()).collect())
                        .unwrap_or_default()
                }
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        }
    }

    fn get_bookmark(&self, name: &str) -> Option<(String, i64)> {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    conn.query_row(
                        "SELECT command, use_count FROM bookmarks WHERE name = ?1",
                        rusqlite::params![name],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .ok()
                }
                Err(_) => None,
            },
            Err(_) => None,
        }
    }

    fn record_bookmark_use(&mut self, name: &str) {
        if let Ok(db_path) = crate::environment::get_data_file("dsh.db")
            && let Ok(db) = crate::db::Db::new(db_path)
        {
            let conn = db.get_connection();
            let _ = conn.execute(
                "UPDATE bookmarks SET use_count = use_count + 1 WHERE name = ?1",
                rusqlite::params![name],
            );
        }
    }

    fn get_last_command(&self) -> Option<String> {
        if let Some(ref history) = self.cmd_history {
            let history = history.lock();
            history.iter().next().map(|e| e.entry.clone())
        } else {
            None
        }
    }

    fn add_dir_alias(&mut self, name: String, path: String) -> bool {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    conn.execute(
                        "INSERT OR REPLACE INTO dir_aliases (name, path) VALUES (?1, ?2)",
                        rusqlite::params![name, path],
                    )
                    .is_ok()
                }
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    fn remove_dir_alias(&mut self, name: &str) -> bool {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    conn.execute(
                        "DELETE FROM dir_aliases WHERE name = ?1",
                        rusqlite::params![name],
                    )
                    .map(|c| c > 0)
                    .unwrap_or(false)
                }
                Err(_) => false,
            },
            Err(_) => false,
        }
    }

    fn list_dir_aliases(&self) -> Vec<(String, String)> {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    let mut stmt =
                        match conn.prepare("SELECT name, path FROM dir_aliases ORDER BY name") {
                            Ok(s) => s,
                            Err(_) => return Vec::new(),
                        };
                    let rows = stmt.query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    });
                    rows.map(|r| r.filter_map(|r| r.ok()).collect())
                        .unwrap_or_default()
                }
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        }
    }

    fn get_dir_alias(&self, name: &str) -> Option<String> {
        match crate::environment::get_data_file("dsh.db") {
            Ok(db_path) => match crate::db::Db::new(db_path) {
                Ok(db) => {
                    let conn = db.get_connection();
                    conn.query_row(
                        "SELECT path FROM dir_aliases WHERE name = ?1",
                        rusqlite::params![name],
                        |row| row.get(0),
                    )
                    .ok()
                }
                Err(_) => None,
            },
            Err(_) => None,
        }
    }
}

/// Converts the shell-side snippet record into the shape builtins see.
fn to_wire_snippet(snippet: crate::snippet::Snippet) -> dsh_types::snippet::Snippet {
    dsh_types::snippet::Snippet {
        id: snippet.id,
        name: snippet.name,
        command: snippet.command,
        description: snippet.description,
        tags: snippet.tags,
        created_at: snippet.created_at,
        last_used: snippet.last_used,
        use_count: snippet.use_count,
    }
}

// Re-export for backward compatibility
pub use builtin::jobs::parse_job_spec;
pub use builtin::reload::format_reload_error;
pub use builtin::z::parse_z_args;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn direnv_allowance_requires_exact_root_match() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = dir.path().join("repo");
        let child = allowed.join("subproject");
        fs::create_dir_all(&child).unwrap();

        assert!(is_same_direnv_root(&allowed, &allowed));
        assert!(
            !is_same_direnv_root(&child, &allowed),
            "allow-direnv should not implicitly trust nested project roots"
        );
    }

    #[test]
    fn confirmation_accepts_only_single_y() {
        assert!(confirmation_is_yes("y\n"));
        assert!(confirmation_is_yes("Y\r\n"));
        assert!(confirmation_is_yes(" y "));

        assert!(!confirmation_is_yes(""));
        assert!(!confirmation_is_yes("\n"));
        assert!(!confirmation_is_yes("n\n"));
        assert!(!confirmation_is_yes("yes\n"));
        assert!(!confirmation_is_yes("1\n"));
    }

    #[test]
    fn confirmation_reads_tty_only_when_stdin_is_terminal() {
        assert!(should_read_confirmation_from_tty(true));
        assert!(!should_read_confirmation_from_tty(false));
    }
}

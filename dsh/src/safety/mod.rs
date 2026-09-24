use crate::process::Job;
use std::collections::HashMap;

mod ai_input;
mod checks;
mod mcp;
pub use ai_input::PromptInjectionResult;
pub use mcp::McpToolCall;

#[derive(Debug, Clone, PartialEq)]
/// What the guard decided about a command or a tool call.
///
/// Two values, not three: the guard runs where a person is present to answer,
/// so its strongest verdict is a question. Refusal is a separate decision, made
/// by the agent path in `AgentCommandVerdict::Denied` for the lines it cannot
/// read before they run - a `Denied` here was constructed nowhere and left nine
/// unreachable handlers behind it.
pub enum SafetyResult {
    Allowed,
    Confirm(String),
}

/// The level lives in `dsh-types` because `dsh-builtin` needs the same one:
/// the chat tools ask the host for it through `ShellProxy::safety_level`, and a
/// second enum here meant the two halves of one policy could disagree about
/// what `loose` allows.
pub use dsh_types::safety_policy::SafetyLevel;

type SafetyCheckFn = Box<dyn Fn(&[String]) -> Option<String> + Send + Sync>;

pub struct SafetyGuard {
    checkers: HashMap<String, SafetyCheckFn>,
}

/// How strictly a raw source allowlist entry may be trusted.
///
/// A job that went through runtime expansion can change its meaning after
/// materialization (`$(printf rm) -rf target`, `$CMD -rf target`, `*`),
/// so an exact match on the raw source must never skip the concrete argv
/// check for such jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SafetyCheckContext {
    pub allow_exact_source_match: bool,
}

impl SafetyCheckContext {
    pub fn strict_source() -> Self {
        Self {
            allow_exact_source_match: true,
        }
    }

    pub fn dynamic_source() -> Self {
        Self {
            allow_exact_source_match: false,
        }
    }
}

impl SafetyGuard {
    pub fn new() -> Self {
        let mut guard = Self {
            checkers: HashMap::new(),
        };

        // Specific checkers
        guard.register_checker("rm", Self::check_rm);
        guard.register_checker("git", Self::check_git);
        guard.register_checker("chmod", Self::check_recursive);
        guard.register_checker("chown", Self::check_recursive);
        guard.register_checker("cp", Self::check_cp);
        guard.register_checker("curl", Self::check_data_exfiltration);
        guard.register_checker("wget", Self::check_data_exfiltration);

        // Sensitive file readers
        for cmd in &["cat", "less", "more", "head", "tail", "grep", "awk", "sed"] {
            guard.register_checker(cmd, Self::check_sensitive_file_access);
        }
        // `mv`, the package managers and `systemctl` used to be registered here
        // with checkers that returned `None` for every input. A registered
        // checker that always allows is indistinguishable from no checker, and
        // the empty ones made the README claim confirmations that never
        // happened. Register a checker when it can say no; `strict` is what
        // covers everything else.
        for cmd in &[
            "sh",
            "bash",
            "zsh",
            "fish",
            "ksh",
            "python",
            "python3",
            "perl",
            "ruby",
            "node",
            "pwsh",
            "powershell",
        ] {
            // The checker only sees the arguments, but which flag means "here
            // is some code" depends on the interpreter, so capture its name.
            let program = cmd.to_string();
            guard.register_checker(cmd, move |args| {
                dsh_types::safety_policy::string_eval_flag(&program, args)
                    .map(|flag| format!("`{flag}` hands {program} a string to execute. Proceed?"))
            });
        }
        guard
    }

    fn register_checker<F>(&mut self, cmd: &str, f: F)
    where
        F: Fn(&[String]) -> Option<String> + Send + Sync + 'static,
    {
        self.checkers.insert(cmd.to_string(), Box::new(f));
    }

    /// Check a list of jobs (pipeline) for dangerous patterns
    pub fn check_jobs(
        &self,
        jobs: &[Job],
        level: &SafetyLevel,
        allowlist: &[String],
    ) -> SafetyResult {
        self.check_jobs_with_context(jobs, level, allowlist, &SafetyCheckContext::strict_source())
    }

    /// Same as [`Self::check_jobs`], but `allow_exact_source_match == false`
    /// forces the concrete post-expansion argv to be judged even when the
    /// raw source line is allowlisted. Use it for materialized jobs that
    /// contained dynamic expansion.
    pub fn check_jobs_with_context(
        &self,
        jobs: &[Job],
        level: &SafetyLevel,
        allowlist: &[String],
        ctx: &SafetyCheckContext,
    ) -> SafetyResult {
        match level {
            SafetyLevel::Loose => return SafetyResult::Allowed,
            SafetyLevel::Strict => {
                // In strict mode, check if all jobs are in allowlist. A raw
                // source match is not trusted for dynamic jobs: runtime
                // expansion can resolve to a different command next time.
                if ctx.allow_exact_source_match
                    && !jobs.is_empty()
                    && jobs.iter().all(|j| allowlist.contains(&j.cmd))
                {
                    return SafetyResult::Allowed;
                }

                if let Some(first) = jobs.first() {
                    return SafetyResult::Confirm(format!(
                        "Command execution '{}' requested in Strict mode.",
                        first.cmd
                    ));
                }
                return SafetyResult::Allowed;
            }
            SafetyLevel::Normal => {}
        }

        // --- Normal Mode Checks ---

        // 1. Check for dangerous pipelines (e.g., curl | sh)
        //
        // The parser puts a whole pipeline in ONE `Job` and chains its stages as
        // processes (see `Rule::pipe_command` in `shell::parse`), so the stages
        // have to be walked through the process chain. Comparing `jobs[i]` with
        // `jobs[i - 1]` only ever looked at `;`-separated commands, which meant
        // `curl … | sh` was never once detected.
        for job in jobs {
            if ctx.allow_exact_source_match && allowlist.contains(&job.cmd) {
                continue;
            }

            let mut previous: Option<String> = None;
            let mut stage = job.process.as_deref();
            while let Some(process) = stage {
                // A synthetic Smart Pipe source is data, not a command: it
                // never becomes `previous` and is never classified itself.
                if process.is_synthetic_source() {
                    stage = process.next_process();
                    continue;
                }
                // `get_cmd` is already just the program for a parsed process, but
                // keep the assignment-prefix skip so a stage that ever arrives as
                // a full command line is classified by the command, not by `FOO=bar`.
                let cmd_name =
                    Self::get_command_name(Self::leading_command_token(process.get_cmd()));

                if let Some(prev_cmd) = previous.as_deref()
                    && Self::is_network_tool(prev_cmd)
                    && Self::is_execution_tool(&cmd_name)
                {
                    return SafetyResult::Confirm(format!(
                        "Dangerous pipeline detected: '{} | {}'. This looks like a 'curl | sh' pattern. Proceed?",
                        prev_cmd, cmd_name
                    ));
                }

                previous = Some(cmd_name);
                stage = process.next_process();
            }
        }

        // 2. Check each command line for dangerous invocations.
        for job in jobs {
            // Check allowlist
            if ctx.allow_exact_source_match && allowlist.contains(&job.cmd) {
                continue;
            }

            if let Some(reason) = self.classify_command_line(&job.cmd) {
                return SafetyResult::Confirm(reason);
            }

            // 3. Judge the materialized argv as well. A dynamic source line
            // like `$(printf rm) -rf target` does not classify as `rm` above,
            // but its concrete stages do.
            let mut stage = job.process.as_deref();
            while let Some(process) = stage {
                if let Some((program, args)) = process.command_argv()
                    && let Some(reason) = self.classify_tokens(program, args)
                {
                    return SafetyResult::Confirm(reason);
                }
                stage = process.next_process();
            }
        }

        SafetyResult::Allowed
    }

    /// Judge a whole command line the way `check_jobs` does: split it at
    /// shell operators, look through wrappers (`sudo`, `env`, `timeout`, ...)
    /// on each segment, and classify what is actually run.
    ///
    /// `line` must be real, not-yet-tokenized shell text (`job.cmd`, or the
    /// raw string an MCP tool's `command` argument carried) - never tokens
    /// rejoined with spaces. Rejoining already-split tokens and re-parsing
    /// them here would corrupt a quote or a literal `;`/`|` that was part of
    /// one argument's value (`check_command` used to do exactly that; see its
    /// doc comment).
    ///
    /// Classifying only the line's first token judged `true | rm -rf ~` as
    /// `true` and `sudo rm -rf ~` as `sudo` - neither of which has a rule, so
    /// both passed every dangerous-command check. Splitting at the operators
    /// and looking through wrappers is what puts `rm` in front of the `rm`
    /// checker. An MCP tool that executes `sudo rm -rf /` or `true; rm -rf /`
    /// used to pass through unconfirmed at the default safety level this way.
    fn classify_command_line(&self, line: &str) -> Option<String> {
        for segment in dsh_types::safety_policy::split_command_segments(line) {
            let parts = match Self::parse_command_tokens(&segment) {
                Ok(parts) => parts,
                Err(err) => {
                    return Some(format!(
                        "Command '{line}' could not be parsed for safety checks ({err}). Proceed?"
                    ));
                }
            };
            if let Some(reason) = Self::classify_tokens_static(&parts, &self.checkers) {
                return Some(reason);
            }
        }
        None
    }

    /// Judge an already-tokenized `[program, args...]` list: look through
    /// wrappers and classify what is actually run, without assuming there was
    /// ever a single string behind the tokens to re-derive.
    ///
    /// `check_command`'s callers (the Lisp `(command ...)` builtin, the
    /// AI-generated single-command path) hand over a program name and a
    /// `Vec<String>` of arguments that were never shell text - joining them
    /// with spaces and feeding the result back through a shell tokenizer, as
    /// an earlier version of this function did, can turn a literal `;` or an
    /// unmatched `'` inside one argument's value into a fabricated operator
    /// or a spurious parse failure. There is no shell operator to split on
    /// here for the same reason: nothing downstream of these two callers ever
    /// interprets the tokens as shell text (the Lisp builtin execs them
    /// directly with no shell in between).
    fn classify_tokens(&self, program: &str, args: &[String]) -> Option<String> {
        let mut parts = Vec::with_capacity(args.len() + 1);
        parts.push(program.to_string());
        parts.extend_from_slice(args);
        Self::classify_tokens_static(&parts, &self.checkers)
    }

    fn classify_tokens_static(
        parts: &[String],
        checkers: &HashMap<String, SafetyCheckFn>,
    ) -> Option<String> {
        for (program, args) in dsh_types::safety_policy::command_candidates(parts) {
            let cmd_clean = Self::get_command_name(&program);

            // 1. Check always warn list
            if Self::always_warns(&cmd_clean) {
                return Some(format!(
                    "Potentially dangerous system command '{cmd_clean}' detected. Proceed?"
                ));
            }

            // 2. Run specific checker if available
            if let Some(checker) = checkers.get(&cmd_clean)
                && let Some(msg) = checker(&args)
            {
                return Some(msg);
            }
        }
        None
    }

    /// Check a single, already-tokenized command (legacy or simpler use
    /// cases: the Lisp `(command ...)` builtin, the AI-generated
    /// single-command path).
    pub fn check_command(
        &self,
        level: &SafetyLevel,
        cmd: &str,
        args: &[String],
        allowlist: &[String],
    ) -> SafetyResult {
        // Construct full command string for allowlist check
        let full_cmd = if args.is_empty() {
            cmd.to_string()
        } else {
            format!("{} {}", cmd, args.join(" "))
        };

        if allowlist.contains(&full_cmd) {
            return SafetyResult::Allowed;
        }

        // Also allow if the command name itself is in the allowlist
        if allowlist.contains(&cmd.to_string()) {
            return SafetyResult::Allowed;
        }

        match level {
            SafetyLevel::Loose => SafetyResult::Allowed,
            SafetyLevel::Strict => {
                SafetyResult::Confirm(format!("Command '{}' will be executed. Proceed?", cmd))
            }
            SafetyLevel::Normal => {
                // Look through wrappers so `sudo rm -rf /` is classified as
                // `rm` instead of the unregistered `sudo`, without treating
                // `cmd`/`args` as if they were one shell line (see
                // `classify_tokens`'s doc comment for why not).
                match self.classify_tokens(cmd, args) {
                    Some(reason) => SafetyResult::Confirm(reason),
                    None => SafetyResult::Allowed,
                }
            }
        }
    }

    /// Check MCP tool execution
    /// Judge one MCP tool call.
    ///
    /// `function_name` is the namespaced name the model called
    /// (`mcp__<label>__<tool>`); it identifies the call in the allowlist and in
    /// what the user is asked. `tool_name` is the tool's own name on its
    /// server, and it is what recognises command-execution tools (`bash`,
    /// ...): matching `"bash"` against `mcp__ops__bash` never held, so a
    /// server's shell tool reached the user as a generic question instead of
    /// being judged as the command it was about to run. `server_trust` is the
    /// operator's explicit opt-in for the owning server, and it - not the tool
    /// name - decides whether a read-only annotation may open the Normal gate.
    /// Pass `McpServerTrust::Untrusted` with `server_label = "<unknown>"` when
    /// the binding cannot be resolved, which only closes the gate the old code
    /// never opened by name alone.
    /// Task grants are explicit host input; sensitive/state paths cannot be granted.
    pub fn task_command_allowed(&self, grant: &dsh_types::agent::TaskGrant, command: &str) -> bool {
        grant.commands.iter().any(|entry| entry == command)
    }

    pub fn task_mcp_allowed(&self, grant: &dsh_types::agent::TaskGrant, entry: &str) -> bool {
        grant.mcp_calls.iter().any(|allowed| allowed == entry)
    }

    pub fn task_file_allowed(
        &self,
        grant: &dsh_types::agent::TaskGrant,
        path: &std::path::Path,
        write: bool,
    ) -> bool {
        if dsh_types::safety_policy::is_sensitive_path(path)
            || path.starts_with(dsh_builtin::config_paths::agent_state_dir())
        {
            return false;
        }
        let roots = if write {
            &grant.write_roots
        } else {
            &grant.read_roots
        };
        roots.iter().any(|root| path.starts_with(root))
    }

    /// Whether this token is a `NAME=value` assignment rather than a command.
    ///
    /// Shared with `command_candidates`, which applies the same rule when it
    /// decides which token names the program.
    fn is_assignment_token(token: &str) -> bool {
        dsh_types::safety_policy::is_assignment_token(token)
    }

    /// The command a line actually runs, past any `NAME=value` prefix.
    fn leading_command_token(cmd: &str) -> &str {
        cmd.split_whitespace()
            .find(|token| !Self::is_assignment_token(token))
            .unwrap_or("")
    }

    fn get_command_name(cmd: &str) -> String {
        std::path::Path::new(cmd)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(cmd)
            .to_string()
    }

    fn parse_command_tokens(command: &str) -> Result<Vec<String>, String> {
        shell_words::split(command).map_err(|err| err.to_string())
    }

    /// Commands that are worth a question on their own, whatever the arguments.
    ///
    /// A predicate rather than a set, so `mkfs.ext4` is recognised as `mkfs`
    /// and so both callers - a typed command line and an MCP tool that carries
    /// one - reach the same answer.
    fn always_warns(cmd: &str) -> bool {
        dsh_types::safety_policy::is_disk_destroying_command(cmd)
            || dsh_types::safety_policy::is_system_power_command(cmd)
    }

    fn is_network_tool(cmd: &str) -> bool {
        dsh_types::safety_policy::is_network_fetch_command(cmd)
    }

    fn is_execution_tool(cmd: &str) -> bool {
        dsh_types::safety_policy::is_code_execution_command(cmd)
    }

    /// Check if modifying an environment variable is safe
    pub fn check_environment_modification(
        &self,
        key: &str,
        _value: &str,
        level: &SafetyLevel,
    ) -> SafetyResult {
        let dangerous_vars = [
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "DYLD_INSERT_LIBRARIES",
            "PYTHONPATH",
            "PERL5LIB",
            "RUBYLIB",
            "NODE_OPTIONS",
        ];

        if dangerous_vars.contains(&key) {
            match level {
                SafetyLevel::Loose => SafetyResult::Allowed,
                SafetyLevel::Strict | SafetyLevel::Normal => SafetyResult::Confirm(format!(
                    "Modification of dangerous environment variable '{}' detected. Proceed?",
                    key
                )),
            }
        } else {
            SafetyResult::Allowed
        }
    }
}

impl Default for SafetyGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;

//! Deciding whether a command the model asked for may run: the allowlist as a
//! fast path (an entry means the operator does not want to be asked), the
//! `SafetyGuard` judgement for everything else, the skill-file and
//! tool-root containment checks, and where the allowlist is read from.
use super::*;

/// What the policy and the user together decided about a command.
pub(super) enum Authorization {
    Run,
    Cancelled,
}

/// Decide whether the command may run, asking the user when the policy says so.
///
/// The old rule was "in the allowlist or refused", and the allowlist starts
/// empty, so out of the box the agent could not run a single command. Now the
/// allowlist is the fast path and everything else is judged by the shell's own
/// `SafetyGuard` at the configured safety level: harmless commands run and
/// risky ones ask.
///
/// The allowlist really is a *skip*, not an extra check: an operator who wrote
/// `(chat-execute-add "rm")` has said they do not want to be asked about `rm`,
/// and the guard's warning is suppressed for it. That is what the entry means,
/// so it is the entry - not this function - that decides how much to trust.
pub(super) fn authorize(
    command: &str,
    stages: &[CommandStage],
    execution_dir: Option<&Path>,
    proxy: &mut dyn ChatToolHost,
) -> Result<Authorization, String> {
    // A skill script is arbitrary code that the agent can also write to, so it
    // is confirmed even when the rest of the policy would wave it through -
    // including under `agent run`, where `--allow-command` does not cover it.
    let skill_script = touches_skill_file(stages, execution_dir, proxy)?;

    // The shell's runtime list plus the JSON config and the environment
    // variable; dropping this merge would have quietly disabled
    // `~/.config/dsh/openai-execute-tool.json`.
    let allowlist = load_allowed_commands(
        proxy.agent_allowlist(),
        proxy.get_var(EXECUTE_TOOL_ENV_ALLOWLIST),
    )?;

    // Configured entries match by token prefix, because a person wrote
    // `cargo test` meaning "and its arguments". A session "always" answer is
    // matched against the exact line the user was shown instead: approving
    // `rm -rf target` must not go on to approve `rm -rf target ~/documents`.
    if proxy.agent_runtime().is_some() {
        // A skill script is not covered by `--allow-command`. The grant names a
        // command line the person read; a skill script is a file the agent can
        // also write, and one that arrives with a `git clone`. Leaving this to
        // `evaluate_agent_command` was how the rule below - "confirmed even
        // when the rest of the policy would wave it through" - stopped being
        // true the moment the same line ran under `agent run`.
        if skill_script {
            proxy
                .request_agent_approval(&format!(
                    "{command}: running a skill script needs its own approval"
                ))
                .map_err(|e| e.to_string())?;
            return Err(format!(
                "agent: skill script permission required: {command}"
            ));
        }
        return match proxy.evaluate_agent_command(command) {
            AgentCommandVerdict::Allowed => Ok(Authorization::Run),
            AgentCommandVerdict::Denied(reason) => Err(reason),
            AgentCommandVerdict::Confirm(reason) => {
                proxy
                    .request_agent_approval(&format!("{command}: {reason}"))
                    .map_err(|e| e.to_string())?;
                Err(format!("agent: command permission required: {command}"))
            }
        };
    }
    let approved_exactly = proxy
        .agent_session_approvals()
        .iter()
        .any(|approved| approved == command);
    let allowlisted = approved_exactly
        || stages
            .iter()
            .all(|stage| command_is_allowlisted(&stage.program, &stage.args, &allowlist));

    let verdict = proxy.evaluate_agent_command(command);
    let redirects_a_write = writes_by_redirection(command);

    let prompt = match verdict {
        AgentCommandVerdict::Denied(reason) => {
            return Err(format!("chat: execute tool refused `{command}`: {reason}"));
        }
        _ if skill_script => {
            format!("AI wants to run a skill script: `{command}`")
        }
        // The `edit` and `str_replace` tools confirm every write; a write
        // spelled as a redirection is the same act and gets the same question.
        _ if redirects_a_write && !allowlisted => {
            format!("AI wants to run `{command}`, which writes to a file")
        }
        AgentCommandVerdict::Allowed => return Ok(Authorization::Run),
        AgentCommandVerdict::Confirm(_) if allowlisted => return Ok(Authorization::Run),
        AgentCommandVerdict::Confirm(reason) => {
            format!("AI wants to run `{command}`. {reason}")
        }
    };

    match proxy
        .request_agent_approval(&prompt)
        .map_err(|err| format!("chat: confirmation failed: {err}"))?
    {
        ApprovalDecision::Allow => Ok(Authorization::Run),
        ApprovalDecision::AllowAlways => {
            proxy.remember_agent_approval(command);
            Ok(Authorization::Run)
        }
        ApprovalDecision::Deny => Ok(Authorization::Cancelled),
    }
}

pub(super) fn program_name(program: &str) -> String {
    Path::new(program)
        .file_name()
        .and_then(|p| p.to_str())
        .unwrap_or(program)
        .to_string()
}

pub(super) fn is_path_qualified(program: &str) -> bool {
    program.contains('/') || program.contains('\\') || Path::new(program).is_absolute()
}

pub(super) fn allowlist_program_matches(entry_program: &str, program: &str) -> bool {
    let entry_name = program_name(entry_program);
    let target_name = program_name(program);

    if is_path_qualified(entry_program) {
        entry_program == program
    } else if is_path_qualified(program) {
        false
    } else {
        entry_name == target_name
    }
}

pub(super) fn allowlist_entry_matches(entry: &str, program: &str, args: &[String]) -> bool {
    let entry_tokens = match split(entry) {
        Ok(tokens) if !tokens.is_empty() => tokens,
        _ => return false,
    };

    if !allowlist_program_matches(&entry_tokens[0], program) {
        return false;
    }

    if entry_tokens.len() == 1 {
        return true;
    }

    // Prefix, not equality. `cargo test` used to authorise exactly
    // `cargo test` and nothing else, so `cargo test -p dsh-builtin` was
    // refused - while a bare `cargo` entry authorised `cargo publish`. Neither
    // extreme is what an allowlist is for.
    let expected = &entry_tokens[1..];
    args.len() >= expected.len() && args[..expected.len()] == *expected
}

pub(super) fn command_is_allowlisted(program: &str, args: &[String], allowlist: &[String]) -> bool {
    allowlist
        .iter()
        .any(|entry| allowlist_entry_matches(entry, program, args))
}

/// Does any stage of `command` run a program named by one of `entries`?
///
/// `entries` uses the same word-prefix form as `AI_CHAT_EXECUTE_ALLOWLIST`, so
/// `"git push"` covers `git push --force` while `"rm"` covers every `rm`. Every
/// stage is judged and wrappers are looked through (`command_candidates`), so
/// `sudo rm -rf x`, `timeout 5 rm x` and `echo hi | rm -rf x` all answer `rm`.
///
/// # The polarity here is the opposite of the allowlist's
///
/// For the allowlist a match means "run without asking", so a matcher that
/// grows stricter refuses more - the safe direction. For a hook's `match` a
/// match means "run this check", so the same matcher growing stricter runs the
/// check *less*: a gate weakening in silence. Anything that changes
/// `allowlist_entry_matches` has to be read with both callers in mind, which is
/// what `command_names_any_looks_through_wrappers_and_stages` pins down.
///
/// An unparseable command line answers **`true`**. A hook's matcher is all that
/// stands between a command and a check the user asked for, and a mismatched
/// quote must not be a way to skip it. `authorize` refuses such a line
/// separately, so firing costs nothing but one hook run.
pub(crate) fn command_names_any(command: &str, entries: &[String]) -> bool {
    let Some(stages) = readable_stages(command) else {
        return true;
    };
    stages
        .iter()
        .any(|stage| command_is_allowlisted(&stage.program, &stage.args, entries))
}

/// Every token of every stage, or `None` when the line cannot be read.
///
/// Same reasoning as `touches_skill_file`: the program alone misses
/// `bash <path>/run.sh`, because `bash` is not a transparent wrapper. A caller
/// asking "does this command line mention such a path" has to see the arguments
/// too. `None` is kept distinct from an empty vector so the caller decides what
/// an unreadable line means rather than inheriting "mentions nothing".
pub(crate) fn command_tokens(command: &str) -> Option<Vec<String>> {
    Some(
        readable_stages(command)?
            .into_iter()
            .flat_map(|stage| std::iter::once(stage.program).chain(stage.args))
            .collect(),
    )
}

pub(super) fn readable_stages(command: &str) -> Option<Vec<CommandStage>> {
    command_stages(command).ok()
}

/// Does any part of this command line reach a file that ships with a skill?
///
/// Every skill root counts, the project one included. A skill arrives with a
/// `git clone` and the prompt actively points the model at it, so a file under
/// `<project>/.dsh/skills` is exactly the case that must not fall through to
/// the ordinary command policy and run unasked under `loose`.
///
/// Judged over **every token of every stage**, not just the program. Only the
/// program was checked at first, and `bash <skill>/run.sh` walked straight
/// past: `bash` is not a transparent wrapper (`COMMAND_WRAPPERS` is right not
/// to list it - `bash foo.sh` runs a script, it does not pass through), so the
/// stage stayed `("bash", ["…run.sh"])` and the program alone said "no".
///
/// This deliberately also asks about reading one - `cat <skill>/SKILL.md` gets
/// a prompt. Telling execution from reading by looking at the arguments means
/// guessing what the program does with them, and guessing wrong in the
/// permissive direction is how the hole above happened. An extra question about
/// a file the agent can also write is the cheaper mistake.
///
/// `AI_CHAT_PROJECT_SKILLS=0` hides project skills from the prompt; it does not
/// make running one of their scripts safe, so this always considers both roots.
pub(super) fn touches_skill_file(
    stages: &[CommandStage],
    execution_dir: Option<&Path>,
    proxy: &mut dyn ChatToolHost,
) -> Result<bool, String> {
    let shell_dir = proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;
    // The directory the command will actually run in. `execute` takes a `cwd`
    // argument, and resolving relative tokens against the shell's directory
    // instead let `{"command": "./run.sh", "cwd": "<skill dir>"}` past.
    let base = execution_dir.unwrap_or(&shell_dir);

    let roots: Vec<PathBuf> = crate::chatgpt::skills::skill_roots(Some(base), true)
        .iter()
        .map(|root| {
            std::fs::canonicalize(&root.path)
                .unwrap_or_else(|_| crate::chatgpt::tool::normalize_path(&root.path))
        })
        .collect();
    if roots.is_empty() {
        return Ok(false);
    }

    for stage in stages {
        if std::iter::once(&stage.program)
            .chain(stage.args.iter())
            .any(|token| token_is_within(token, base, &roots))
        {
            return Ok(true);
        }
    }

    Ok(false)
}

pub(super) fn token_is_within(token: &str, base: &Path, roots: &[PathBuf]) -> bool {
    // A bare word is a PATH lookup, not a path into a skill.
    if !token.contains('/') && !Path::new(token).is_absolute() {
        return false;
    }
    // Options like `--config=x` are not paths; a real path token starting with
    // `-` would have to be written `./-foo` anyway.
    if token.starts_with('-') {
        return false;
    }

    // Resolved here rather than through `resolve_tool_path`, which is an access
    // decision: under a task it refuses any path outside the grants, so every
    // ungranted skill script came back as "not a skill script" and fell through
    // to the ordinary command policy - the exact opposite of what this is for.
    let Ok(expanded) = shellexpand::full(token) else {
        return false;
    };
    let path = Path::new(expanded.as_ref());
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let Ok(resolved) = crate::chatgpt::tool::resolve_with_existing_ancestor(&absolute) else {
        return false;
    };

    roots.iter().any(|root| resolved.starts_with(root))
}

/// Every source of "the agent may run this without asking", merged.
///
/// The environment variable used to return early and win outright, so setting
/// one entry silently discarded `config.lisp` and the JSON config - a trap,
/// because nothing said the other two had stopped applying.
pub(super) fn load_allowed_commands(
    runtime_allowed: Vec<String>,
    shell_value: Option<String>,
) -> Result<Vec<String>, String> {
    let mut allowlist = runtime_allowed;

    // Shell variable first, process environment second - the order every other
    // AI setting resolves in. Reading only `std::env` made this the one key
    // that `config.lisp` could not set without an `export`.
    if let Some(mut from_env) = read_allowlist_from_env(shell_value) {
        allowlist.append(&mut from_env);
    }

    if let Some(config_path) = resolve_allowlist_path()?
        && let Some(mut file_allowlist) = read_allowlist_from_file(&config_path)?
    {
        allowlist.append(&mut file_allowlist);
    }

    allowlist.sort();
    allowlist.dedup();
    Ok(allowlist)
}

pub(super) fn read_allowlist_from_file(path: &PathBuf) -> Result<Option<Vec<String>>, String> {
    let contents = fs::read_to_string(path).map_err(|err| {
        format!(
            "chat: failed to read execute tool config {}: {err}",
            path.display()
        )
    })?;

    if contents.trim().is_empty() {
        return Ok(Some(Vec::new()));
    }

    #[derive(Deserialize)]
    struct ExecuteAllowlist {
        #[serde(default)]
        allowed_commands: Vec<String>,
    }

    let raw: ExecuteAllowlist = serde_json::from_str(&contents)
        .map_err(|err| format!("chat: failed to parse {} as JSON: {err}", path.display()))?;

    Ok(Some(
        raw.allowed_commands
            .into_iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
    ))
}

pub(super) fn read_allowlist_from_env(shell_value: Option<String>) -> Option<Vec<String>> {
    let raw = shell_value.or_else(|| env::var(EXECUTE_TOOL_ENV_ALLOWLIST).ok())?;
    let entries: Vec<String> = raw
        .split([',', '\n'])
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect();

    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

pub(super) fn resolve_allowlist_path() -> Result<Option<PathBuf>, String> {
    if let Ok(path) = env::var(EXECUTE_TOOL_CONFIG_OVERRIDE_ENV) {
        return Ok(Some(PathBuf::from(path)));
    }

    let xdg_dirs = BaseDirectories::with_prefix(CONFIG_DIR_PREFIX);

    Ok(xdg_dirs.find_config_file(EXECUTE_TOOL_CONFIG_FILE))
}

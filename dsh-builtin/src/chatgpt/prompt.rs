//! System prompt assembly: the fixed tool-use instructions
//! (`TOOL_SYSTEM_PROMPT`), folding in skills/MCP/operator-prompt/language
//! (`build_system_prompt`), `@name` skill mentions, and the project-skill
//! trust gate (a repository's own `.dogesh/skills` must be agreed to before its
//! descriptions reach a prompt).
use super::*;

/// System prompt that explains how to use the builtin tools
pub(super) const TOOL_SYSTEM_PROMPT: &str = r#"You are DogeShell Assistant, an autonomous software engineering agent running inside doge-shell.

Rules:
1. Briefly plan before using tools.
2. When the user refers to something that already happened - "the error just now", "why did that fail" - read it with `shell_history` instead of running the command again.
3. Explore cheaply first: prefer `search` and `ls`; use `read_file` only after locating the exact target.
4. Ask `shell_context` for the project's build and test commands rather than guessing them.
5. Verify every change. After editing, read the file back. After `execute`, check exit code, stdout, and stderr.
6. If a tool fails, analyze the error before asking the user.
7. When a task took many tool calls, or you recovered from a mistake the user corrected, save the
   lesson with `skill_manage` so the next run is shorter. If an existing skill already covers close
   to this ground, `patch` it instead of creating a near-duplicate. Use `project` scope for knowledge
   tied to this repository, `user` scope otherwise. Record the reproducible steps, the assumptions,
   and the pitfall - never a transcript or a copy of file contents. Write `description` as a one-line
   "Use when ..." trigger; it is the only thing shown until the skill is read.

Tools:
- `shell_history`: what the user recently ran, with exit codes and output
- `shell_context`: project root, runtimes, defined tasks, aliases
- `search`: find files or matching text
- `ls`: inspect directories
- `read_file`: read a line-numbered window of a file; it is paged, so continue with `offset`
- `str_replace`: change part of a file by exact match; use this for edits
- `edit`: create a file, or replace an existing one in full
- `execute`: run a shell command; pipes, redirection and `&&` all work
- `skill_manage`: create, update or delete a reusable skill in the directories listed below

Respond in Markdown. Be concise and avoid unnecessary repetition.
"#;

/// The system prompt, split into what identifies the conversation and what is
/// actually sent.
///
/// The two differ by the skills list. A carried-over conversation is discarded
/// when the system prompt changes, and the list changes whenever a skill is
/// installed - or written by the agent itself. Keying continuity on the list
/// meant the model lost its context at the exact moment it had just learned
/// something, so the list is excluded from `identity` and re-rendered into
/// `text` on every turn.
pub(super) struct SystemPrompt {
    pub(super) identity: String,
    pub(super) text: String,
}

pub(super) fn build_system_prompt(
    operator_prompt: Option<String>,
    language: Option<String>,
    mcp_manager: &McpManager,
    skill_roots: &[SkillRoot],
) -> SystemPrompt {
    let skills_fragment = if skill_roots.is_empty() {
        String::new()
    } else {
        SkillsManager::with_roots(skill_roots.to_vec()).get_system_prompt_fragment()
    };

    SystemPrompt {
        identity: assemble_system_prompt(
            "",
            operator_prompt.as_deref(),
            language.as_deref(),
            mcp_manager,
        ),
        text: assemble_system_prompt(
            &skills_fragment,
            operator_prompt.as_deref(),
            language.as_deref(),
            mcp_manager,
        ),
    }
}

pub(super) fn assemble_system_prompt(
    skills_fragment: &str,
    operator_prompt: Option<&str>,
    language: Option<&str>,
    mcp_manager: &McpManager,
) -> String {
    let mut base = TOOL_SYSTEM_PROMPT.to_string();

    if !skills_fragment.is_empty() {
        base.push_str(skills_fragment);
    }

    if let Some(fragment) = mcp_manager.system_prompt_fragment() {
        base.push_str("\n\nMCP access:");
        base.push('\n');
        base.push_str(&fragment);
    }

    if let Some(extra) = operator_prompt.and_then(|p| {
        let trimmed = p.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }) {
        base.push_str("\n\nAdditional operator instructions:\n");
        base.push_str(&extra);
    }

    dsh_openai::apply_language(&base, language)
}

/// Keep `pinned_messages[0]` - the system message - in one place.
///
/// Both the persistent-task checkpoint and a carried-over interactive session
/// restore a `ConversationManager` that was serialized with an older prompt.
/// Indexing directly also panicked on a checkpoint whose pinned list was empty.
pub(super) fn set_system_prompt(manager: &mut ConversationManager, text: &str) {
    if let Some(slot) = manager.pinned_messages.first_mut() {
        *slot = json!({ "role": "system", "content": text });
    }
}

/// Load any skills the user named with `@`, and hand back the rest of the line.
///
/// The directories are only scanned when the message actually starts with `@`,
/// so an ordinary turn pays nothing for this.
pub(super) fn resolve_skill_mentions<'a>(
    user_input: &'a str,
    skill_roots: &[SkillRoot],
) -> (Vec<String>, &'a str) {
    if !user_input.trim_start().starts_with('@') || skill_roots.is_empty() {
        return (Vec::new(), user_input);
    }

    let skills = SkillsManager::with_roots(skill_roots.to_vec()).load_skills();
    let known: std::collections::BTreeSet<&str> =
        skills.iter().map(|skill| skill.name.as_str()).collect();
    let (names, rest) = skills::split_leading_mentions(user_input, &|name| known.contains(name));

    let loaded = names
        .iter()
        .filter_map(|name| {
            let skill = skills.iter().find(|skill| &skill.name == name)?;
            skills::usage::note_read(skill.dir(), skill.scope);
            skills::render_mention(skill)
        })
        .collect();

    // An `@name` that resolves to nothing is left in `rest`, so the model still
    // sees exactly what the user typed.
    (loaded, rest)
}

/// Drop the project skill root unless the user has agreed to this repository.
///
/// The descriptions of `<project>/.dogesh/skills` go into the system prompt, and
/// the agent reading that prompt has `execute`. `.dogesh/hooks.json` is not read
/// for the same reason; this holds skills to the same bar.
///
/// Under a persistent task nothing is asked: an unattended run must not stall
/// on a question, and the entry point that runs without a person watching is
/// the one that should be *more* careful, not equally trusting. An untrusted
/// project is simply not read there.
pub(super) fn gate_project_skills(
    roots: &mut Vec<skills::SkillRoot>,
    proxy: &mut dyn ChatToolHost,
) {
    // Asked per root and dropped per root. Trust is recorded against a root
    // path, so declining one shared directory must not also throw away a
    // directory the user has already agreed to.
    for decision in skills::describe_project_roots(roots) {
        if !trusts_project_root(&decision, proxy) {
            roots.retain(|root| root.path != decision.root);
        }
    }
}

/// Does the user agree to this one project skills root?
pub(super) fn trusts_project_root(
    decision: &skills::ProjectSkillDecision,
    proxy: &mut dyn ChatToolHost,
) -> bool {
    if skills::trust::is_remembered(&decision.root, &decision.digest) {
        return true;
    }

    let session_key = skills::trust::session_key(&decision.root, &decision.digest);
    if proxy.agent_session_approvals().contains(&session_key) {
        return true;
    }

    if proxy.agent_runtime().is_some() {
        tracing::debug!(
            "skipping untrusted project skills at {}",
            decision.root.display()
        );
        return false;
    }

    let shown: Vec<&str> = decision.names.iter().take(8).map(String::as_str).collect();
    let more = decision.names.len().saturating_sub(shown.len());
    let suffix = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    let message = format!(
        "This repository ships {} skill(s) in `{}` ({}{}). Their descriptions go into every AI prompt here. Read them? y = this session, a = remember this repository",
        decision.names.len(),
        crate::config_paths::display_path(&decision.root),
        shown.join(", "),
        suffix
    );

    match proxy.request_agent_approval(&message) {
        Ok(crate::shell_capabilities::ApprovalDecision::Allow) => {
            proxy.remember_agent_approval(&session_key);
            true
        }
        Ok(crate::shell_capabilities::ApprovalDecision::AllowAlways) => {
            proxy.remember_agent_approval(&session_key);
            skills::trust::remember(&decision.root, &decision.digest);
            true
        }
        Ok(crate::shell_capabilities::ApprovalDecision::Deny) => false,
        Err(err) => {
            // Fail closed: an unanswerable question is not consent.
            tracing::debug!("could not ask about project skills: {err}");
            false
        }
    }
}

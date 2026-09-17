//! Safety gates a file tool runs before it touches disk: `.gitignore` (`reject_gitignored_path`/`_read_path`), a skill's frontmatter shape (`reject_broken_skill_md`), staged-review bypass (`reject_skill_path_while_staging_always`),
//! and the interactive confirm/"always" prompt shared with `execute` and MCP calls (`confirm_agent_action`).
use super::*;

pub(crate) fn safety_level(proxy: &mut dyn ShellProxy) -> SafetyLevel {
    proxy.safety_level()
}
pub(crate) fn reject_gitignored_path(
    path: &Path,
    base_dir: &Path,
    user_path: &str,
) -> Result<(), String> {
    match gitignore::is_gitignored(path, base_dir) {
        Ok(false) => Ok(()),
        Ok(true) => Err(format!(
            "chat: tool path `{user_path}` is ignored by .gitignore"
        )),
        Err(err) => Err(format!("chat: failed to apply .gitignore policy: {err}")),
    }
}
/// The same rule for the tools that only read, with skills exempted.
///
/// A repository that ignores `.dogesh/` would otherwise have its project skills
/// advertised in the prompt and then refused by every `read_file`. The
/// exemption is read-only on purpose: the justification is "the prompt already
/// pointed the model here", which says nothing about writing. `skill_manage` is
/// the way to change a skill, and it validates the name, the path and the
/// symlinks that plain `edit` would not.
///
/// The skill roots are resolved only on the branch that would reject, because
/// `ls` calls this once per directory entry and the resolution walks ancestors.
pub(crate) fn reject_gitignored_read_path(
    path: &Path,
    base_dir: &Path,
    user_path: &str,
) -> Result<(), String> {
    match gitignore::is_gitignored(path, base_dir) {
        Ok(false) => Ok(()),
        Ok(true) if crate::chatgpt::skills::is_within_skill_root(path, base_dir) => Ok(()),
        Ok(true) => Err(format!(
            "chat: tool path `{user_path}` is ignored by .gitignore"
        )),
        Err(err) => Err(format!("chat: failed to apply .gitignore policy: {err}")),
    }
}
/// The same content check `skill_manage` runs, applied to the other two
/// tools that can reach the exact same file.
///
/// `skill_manage` validates the name, the path, the symlinks *and* (since the
/// content lint) the shape of what it writes - but `edit` and `str_replace`
/// advertise "absolute for skills" in their own schemas and were never routed
/// through any of that. Without this, deleting a skill's `description:` line
/// through `str_replace` reopened exactly the bug the content lint closed,
/// just through a different tool.
///
/// Only ever inspects the file the loader actually parses for frontmatter -
/// a folder skill's `SKILL.md`, or a bare `*.md` file that is the whole
/// skill. A bundled `references/*` file is unaffected: `lint_bundled` is
/// advisory even from `skill_manage` itself, so there is nothing to gate here.
/// Structural, not existence-based, so creating a brand-new skill this way is
/// caught the same as editing one that is already on disk.
pub(crate) fn reject_broken_skill_md(
    path: &Path,
    current_dir: &Path,
    contents: &str,
) -> Result<(), String> {
    let Some((skill_dir, _scope)) = crate::chatgpt::skills::containing_skill(path, current_dir)
    else {
        return Ok(());
    };

    let is_folder_skill_md = path.parent() == Some(skill_dir.as_path())
        && path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md");
    let is_bare_md_skill =
        path == skill_dir && path.extension().and_then(|ext| ext.to_str()) == Some("md");
    if !is_folder_skill_md && !is_bare_md_skill {
        return Ok(());
    }

    let name = if is_folder_skill_md {
        skill_dir.file_name()
    } else {
        skill_dir.file_stem()
    };
    let Some(name) = name.and_then(|name| name.to_str()) else {
        return Ok(());
    };

    let findings = crate::chatgpt::skills::lint::lint_skill_md(name, contents);
    if let Some(reason) = crate::chatgpt::skills::lint::has_rejection(&findings) {
        return Err(format!("chat: {reason}"));
    }
    Ok(())
}
/// Close the one way `edit`/`str_replace` could bypass staged review for a
/// path `skill_manage` would have queued instead of writing.
///
/// Only `SkillStaging::Always` needs this. `SkillStaging::Task` redirects an
/// agent task with no write grant for the target - and `edit`/`str_replace`
/// already stall there today through the very same `confirm_agent_action`
/// grant check `skill_manage` uses, so nothing is bypassed; it is simply not
/// unblocked the way `skill_manage`'s own write now is. `Always` is
/// different: it means a person asked to review *every* skill write, and
/// `edit`/`str_replace` writing the same file through their own ordinary
/// interactive confirmation would skip that review entirely.
pub(crate) fn reject_skill_path_while_staging_always(
    path: &Path,
    current_dir: &Path,
    proxy: &mut dyn ChatToolHost,
) -> Result<(), String> {
    if crate::chatgpt::resolve_skill_staging(proxy) != crate::chatgpt::SkillStaging::Always {
        return Ok(());
    }
    if crate::chatgpt::skills::containing_skill(path, current_dir).is_none() {
        return Ok(());
    }
    Err(
        "chat: this path belongs to a skill; use `skill_manage` so the change is queued for review like every other skill write"
            .to_string(),
    )
}
pub(crate) fn confirm_sensitive_access(
    proxy: &mut dyn ChatToolHost,
    action: &str,
    path_label: &str,
    resolved: &Path,
    reason: &str,
) -> Result<bool, String> {
    if !safety_level(proxy).requires_confirmation_for_sensitive_access() {
        return Ok(true);
    }

    confirm_agent_action(
        proxy,
        &sensitive_approval_key(action, resolved),
        &format!("AI wants to {action} sensitive content `{path_label}` ({reason})"),
    )
}
/// Ask the user about an action the agent wants to take, offering "always".
///
/// The three-way answer already existed for `execute` and for MCP calls; the
/// file tools were left on `ShellProxy::confirm_action`, whose bool cannot say
/// "always". A twenty-step edit was twenty prompts, which is how a safety gate
/// turns into a key people hold down.
///
/// `approval_key` is what an "always" answer remembers, matched exactly and
/// stored beside the command lines and `mcp:` entries in the same session list.
/// It is deliberately coarser than the message: the question names the change,
/// the key names the file, so approving one edit of a file does not have to be
/// re-answered for the next one.
pub(crate) fn confirm_agent_action(
    proxy: &mut dyn ChatToolHost,
    approval_key: &str,
    message: &str,
) -> Result<bool, String> {
    confirm_agent_action_with_preview(proxy, approval_key, message, None)
}

/// The same gate, showing what the change actually is before it asks.
///
/// A question naming only the file is one a person cannot answer, so the
/// rational response to a run of them is to stop reading and press "always" -
/// which is how this gate stops being one. `preview` is what makes the answer
/// informed.
///
/// It is written to stderr on the branch that is about to ask, and only there:
/// never into a task's `stop_reason` (it would bloat the stored record and the
/// incident text `cron logs` prints), and never when the session already
/// carries an "always" for this key.
pub(crate) fn confirm_agent_action_with_preview(
    proxy: &mut dyn ChatToolHost,
    approval_key: &str,
    message: &str,
    preview: Option<&str>,
) -> Result<bool, String> {
    if let Some(runtime) = proxy.agent_runtime() {
        if let Some(path) = approval_key.strip_prefix("write:")
            && agent_write_granted(proxy, Path::new(path))
        {
            return Ok(true);
        }
        // An unattended task is never stopped for a missing grant: the
        // refusal is recorded (so a stuck task can name its resume command)
        // and returned as a tool-result error the turn works around. Only a
        // task that repeats the same refused operation (the three-strikes
        // guard in `AgentRuntime::after_tool`), hits an unknown outcome, or
        // exhausts a budget still ends up waiting on a person.
        let hint = format!("{message} [approval_key: {approval_key}]");
        runtime.lock().note_denial(&hint);
        return Err(format!("agent: permission required: {hint}"));
    }
    if proxy
        .agent_session_approvals()
        .iter()
        .any(|approved| approved == approval_key)
    {
        return Ok(true);
    }

    if let Some(preview) = preview.filter(|preview| !preview.trim().is_empty()) {
        eprint!("{preview}");
    }

    match proxy
        .request_agent_approval(message)
        .map_err(|err: anyhow::Error| format!("chat: confirmation failed: {err}"))?
    {
        ApprovalDecision::Allow => Ok(true),
        ApprovalDecision::AllowAlways => {
            proxy.remember_agent_approval(approval_key);
            Ok(true)
        }
        ApprovalDecision::Deny => Ok(false),
    }
}
/// Whether an agent task's `--write` grant already covers `path`, without
/// touching the task's status.
///
/// Shared by `confirm_agent_action` (which falls through to
/// `InputRequired` when this is `false`) and `skill_manage`'s staging check
/// (which falls through to staging a proposal instead), so the two can never
/// disagree about what a task's grant covers. `false` when there is no agent
/// task at all - callers that only make sense under one check that
/// themselves.
pub(crate) fn agent_write_granted(proxy: &mut dyn ChatToolHost, path: &Path) -> bool {
    proxy.evaluate_agent_file(path, true) == crate::shell_capabilities::AgentCommandVerdict::Allowed
}
/// What "always" remembers for a file the agent wants to change.
///
/// One key for `edit` and `str_replace` alike: the user is deciding about the
/// file, not about which tool happens to write it.
pub(crate) fn write_approval_key(resolved: &Path) -> String {
    format!("write:{}", resolved.display())
}
fn sensitive_approval_key(action: &str, resolved: &Path) -> String {
    format!("sensitive:{action}:{}", resolved.display())
}
pub(crate) fn sensitive_path_reason(path: &Path) -> Option<&'static str> {
    safety_policy::is_sensitive_path(path).then_some("sensitive path")
}

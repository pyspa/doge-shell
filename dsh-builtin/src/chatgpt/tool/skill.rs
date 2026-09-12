//! Writing skills, so the agent can keep what it learned.
//!
//! Reading is already covered: `read_file` and `ls` reach both skill roots, and
//! the system prompt lists what is there. This tool exists for the other half -
//! and only for that half, so the model has one obvious place to record a lesson
//! rather than an absolute path it has to construct.
//!
//! It is a separate tool from `edit` for four reasons `edit` cannot cover:
//! choosing a scope by name instead of by path, validating the shape of a skill
//! before anything is written, generating frontmatter the reader can actually
//! parse, and deleting.
//!
//! Every action asks the user first. A skill is instructions that a later run
//! will follow, so writing one is closer to editing configuration than to
//! editing a working file.

use crate::chatgpt::skills::{self, MAX_DESCRIPTION_CHARS, SkillScope, pending};
use crate::shell_capabilities::ChatToolHost;
use regex::Regex;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub(crate) const NAME: &str = "skill_manage";

/// Directory-name rules, which are also the name the prompt will show.
static SKILL_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9]([a-z0-9-]{0,62}[a-z0-9])?$").expect("static regex"));

/// Ceilings on what one `delete` may remove without a second look.
const MAX_DELETE_ENTRIES: usize = 64;
const MAX_DELETE_BYTES: u64 = 1024 * 1024;

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Create, update or delete a reusable skill - a short SKILL.md a later run reads instead of rediscovering the same steps. Use after a task that took many steps, or after a correction. Always asks the user first.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "write_file", "patch", "delete"],
                        "description": "delete removes the whole skill unless `file` is given."
                    },
                    "name": {
                        "type": "string",
                        "description": "Lowercase letters, digits and hyphens, e.g. `rust-bisect`."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["user", "project"],
                        "description": "`project` for this repository, `user` (default) for anything portable."
                    },
                    "description": {
                        "type": "string",
                        "description": "Required for `create`. One line saying *when* to use the skill - this is all the prompt shows."
                    },
                    "body": {
                        "type": "string",
                        "description": "Markdown body for `create`. Frontmatter is generated."
                    },
                    "file": {
                        "type": "string",
                        "description": "Path inside the skill, e.g. `references/api.md`. Default `SKILL.md`."
                    },
                    "contents": {
                        "type": "string",
                        "description": "Full contents for `write_file`."
                    },
                    "old_string": {
                        "type": "string",
                        "description": "For `patch`. Must appear exactly once."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "For `patch`."
                    }
                },
                "required": ["action", "name"],
                "additionalProperties": false
            }
        }
    })
}

/// What the caller asked for, after the arguments have been checked.
pub(crate) struct Request {
    pub(crate) action: String,
    pub(crate) name: String,
    pub(crate) scope: SkillScope,
    pub(crate) skill_dir: PathBuf,
    /// Resolved target, canonical as far as it exists.
    pub(crate) target: PathBuf,
    /// Whether `file` was given, which is what separates "delete one file" from
    /// "delete the skill".
    pub(crate) explicit_file: bool,
    pub(crate) relative_file: String,
}

pub(crate) fn run(arguments: &str, proxy: &mut dyn ChatToolHost) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for {NAME} tool: {err}"))?;

    let request = validate(&parsed, proxy)?;

    match request.action.as_str() {
        "create" => create(&parsed, &request, proxy),
        "write_file" => write_file(&parsed, &request, proxy),
        "patch" => patch(&parsed, &request, proxy),
        "delete" => delete(&request, proxy),
        other => Err(format!("chat: {NAME} does not support action `{other}`")),
    }
}

/// Everything that can be checked before the user is asked anything.
fn validate(parsed: &Value, proxy: &mut dyn ChatToolHost) -> Result<Request, String> {
    let current_dir = proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;
    validate_in(parsed, &current_dir)
}

/// The same checks as `validate`, against an explicit directory rather than
/// a live proxy.
///
/// Split out so `skill approve` can run the identical name/scope/path/
/// symlink checks against a staged proposal without needing a
/// `ChatToolHost` - approval happens from the `skill` builtin, which only
/// has a `ShellProxy`. Nothing here asks the user anything; ordering matters
/// only in that a question about a change that was going to be refused
/// anyway trains people to answer without reading, and this function is
/// exactly the part that runs before any question is asked.
pub(crate) fn validate_in(parsed: &Value, current_dir: &Path) -> Result<Request, String> {
    let action = parsed
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("chat: {NAME} requires `action`"))?
        .trim()
        .to_string();

    let name = parsed
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("chat: {NAME} requires `name`"))?
        .trim()
        .to_string();

    if !SKILL_NAME.is_match(&name) || name.contains("--") {
        return Err(format!(
            "chat: `{name}` is not a usable skill name. Use lowercase letters, digits and single hyphens, e.g. `rust-bisect`."
        ));
    }

    let scope = match parsed.get("scope").and_then(Value::as_str) {
        None | Some("user") => SkillScope::User,
        Some("project") => SkillScope::Project,
        Some(other) => return Err(format!("chat: `{other}` is not a skill scope")),
    };

    let root = match scope {
        SkillScope::User => crate::config_paths::skills_dir(),
        SkillScope::Project => skills::project_skills_root(current_dir).ok_or_else(|| {
            format!(
                "chat: there is no project here, so `{}` has nowhere to live. Use scope `user`.",
                skills::PROJECT_SKILLS_DIR
            )
        })?,
    };

    let relative_file = parsed
        .get("file")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let explicit_file = relative_file.is_some();
    let relative_file = relative_file.unwrap_or("SKILL.md").to_string();

    let candidate = Path::new(&relative_file);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "chat: `{relative_file}` must be a path inside the skill directory"
        ));
    }

    let skill_dir = root.join(&name);
    // Both sides go through the same resolver so the comparison holds whether or
    // not the directories exist yet, and so a symlinked skill directory
    // canonicalises out of the root and is refused.
    let resolved_root = super::resolve_with_existing_ancestor(&root)?;
    let resolved_dir = super::resolve_with_existing_ancestor(&skill_dir)?;
    let target = super::resolve_with_existing_ancestor(&skill_dir.join(&relative_file))?;

    if !resolved_dir.starts_with(&resolved_root) || !target.starts_with(&resolved_dir) {
        return Err(format!(
            "chat: `{relative_file}` resolves outside the skill directory"
        ));
    }

    Ok(Request {
        action,
        name,
        scope,
        skill_dir: resolved_dir,
        target,
        explicit_file,
        relative_file,
    })
}

/// The `create`-specific guards against a name collision: the skill
/// directory must not already exist, and no sibling `<name>.md` file-skill
/// may already answer to the same name.
///
/// Shared between `create()` (checked before anything is written) and
/// `skill approve` (checked again at approval time): a `create` proposal's
/// staleness check alone - the digest of `request.target`, which does not
/// exist either way for a brand-new skill - cannot see a *sibling*
/// collision that appeared after the proposal was staged, only a change to
/// the exact file it names.
pub(crate) fn reject_create_collision(request: &Request) -> Result<(), String> {
    if request.skill_dir.exists() {
        return Err(format!(
            "chat: the skill `{}` already exists. Use `patch` or `write_file` to change it.",
            request.name
        ));
    }
    // A `foo/` beside a `foo.md` is two skills with one name; the loader has to
    // pick, and whichever it picks the other silently disappears.
    if request.skill_dir.with_extension("md").exists() {
        return Err(format!(
            "chat: a file skill named `{}` already exists beside it. Use `patch`, or remove that file first.",
            request.name
        ));
    }
    Ok(())
}

fn create(
    parsed: &Value,
    request: &Request,
    proxy: &mut dyn ChatToolHost,
) -> Result<String, String> {
    if request.explicit_file {
        return Err(format!(
            "chat: `create` always writes SKILL.md; use `write_file` to add `{}`",
            request.relative_file
        ));
    }
    reject_create_collision(request)?;

    let description = parsed
        .get("description")
        .and_then(Value::as_str)
        .map(one_line)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "chat: `create` requires `description`: one line saying when to use the skill"
                .to_string()
        })?;

    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(format!(
            "chat: `description` must be at most {MAX_DESCRIPTION_CHARS} characters; only the first line is ever shown"
        ));
    }

    let body = parsed
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_start_matches('\n');
    let contents = render_skill_md(&request.name, &description, body);

    confirm_and_write(
        request,
        &contents,
        &format!(
            "AI wants to create the {} skill `{}` at `{}`",
            request.scope.as_str(),
            request.name,
            crate::config_paths::display_path(&request.target)
        ),
        true,
        proxy,
    )
}

fn write_file(
    parsed: &Value,
    request: &Request,
    proxy: &mut dyn ChatToolHost,
) -> Result<String, String> {
    let contents = parsed
        .get("contents")
        .and_then(Value::as_str)
        .ok_or_else(|| "chat: `write_file` requires `contents`".to_string())?;

    let verb = if request.target.exists() {
        "replace"
    } else {
        "add"
    };
    confirm_and_write(
        request,
        contents,
        &format!(
            "AI wants to {verb} `{}` in the {} skill `{}`",
            request.relative_file,
            request.scope.as_str(),
            request.name
        ),
        false,
        proxy,
    )
}

fn patch(
    parsed: &Value,
    request: &Request,
    proxy: &mut dyn ChatToolHost,
) -> Result<String, String> {
    let old_string = parsed
        .get("old_string")
        .and_then(Value::as_str)
        .ok_or_else(|| "chat: `patch` requires `old_string`".to_string())?;
    let new_string = parsed
        .get("new_string")
        .and_then(Value::as_str)
        .ok_or_else(|| "chat: `patch` requires `new_string`".to_string())?;

    if old_string.is_empty() {
        return Err("chat: `old_string` must not be empty".to_string());
    }

    let existing = read_target(request, proxy)?;
    // Same rule as `str_replace`: an ambiguous match is a different edit than
    // the one the model believes it is making.
    match existing.matches(old_string).count() {
        1 => {}
        0 => {
            return Err(format!(
                "chat: `old_string` does not appear in `{}`",
                request.relative_file
            ));
        }
        count => {
            return Err(format!(
                "chat: `old_string` appears {count} times in `{}`; include enough context to match once",
                request.relative_file
            ));
        }
    }

    let contents = existing.replacen(old_string, new_string, 1);
    confirm_and_write(
        request,
        &contents,
        &format!(
            "AI wants to patch `{}` in the {} skill `{}`",
            request.relative_file,
            request.scope.as_str(),
            request.name
        ),
        false,
        proxy,
    )
}

fn delete(request: &Request, proxy: &mut dyn ChatToolHost) -> Result<String, String> {
    if request.explicit_file {
        if request.relative_file == "SKILL.md" {
            return Err(
                "chat: SKILL.md is the skill; omit `file` to delete the whole skill".to_string(),
            );
        }
        if !request.target.is_file() {
            return Err(format!(
                "chat: `{}` is not a file in this skill",
                request.relative_file
            ));
        }

        if !super::confirm_agent_action(
            proxy,
            &delete_approval_key(&request.target),
            &format!(
                "AI wants to delete `{}` from the {} skill `{}`",
                request.relative_file,
                request.scope.as_str(),
                request.name
            ),
        )? {
            return Ok("Skill change cancelled by user.".to_string());
        }

        std::fs::remove_file(&request.target)
            .map_err(|err| format!("chat: failed to delete `{}`: {err}", request.relative_file))?;
        skills::clear_skills_fragment_cache();
        return Ok(report(request, "delete", 0, &[]));
    }

    if !request.skill_dir.is_dir() {
        return Err(format!("chat: there is no skill named `{}`", request.name));
    }

    let (entries, bytes) = measure_tree(&request.skill_dir)?;

    if !super::confirm_agent_action(
        proxy,
        &delete_approval_key(&request.skill_dir),
        &format!(
            "AI wants to delete the {} skill `{}` and its {entries} files at `{}`",
            request.scope.as_str(),
            request.name,
            crate::config_paths::display_path(&request.skill_dir)
        ),
    )? {
        return Ok("Skill change cancelled by user.".to_string());
    }

    let _ = bytes;
    std::fs::remove_dir_all(&request.skill_dir)
        .map_err(|err| format!("chat: failed to delete `{}`: {err}", request.name))?;
    skills::usage::forget(&request.skill_dir);
    skills::clear_skills_fragment_cache();

    Ok(report(request, "delete", 0, &[]))
}

/// Refuse to remove a tree that is not plainly a skill.
///
/// A symlink inside it would be followed out of the skill directory, and a tree
/// this large is not something the model built one file at a time.
fn measure_tree(root: &Path) -> Result<(usize, u64), String> {
    let mut entries = 0usize;
    let mut bytes = 0u64;

    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|err| format!("chat: failed to inspect the skill: {err}"))?;
        let metadata = entry
            .metadata()
            .map_err(|err| format!("chat: failed to inspect the skill: {err}"))?;

        if metadata.file_type().is_symlink() {
            return Err(format!(
                "chat: `{}` contains a symlink; remove it by hand",
                crate::config_paths::display_path(root)
            ));
        }
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(format!(
                "chat: `{}` contains something that is not a plain file",
                crate::config_paths::display_path(root)
            ));
        }

        if metadata.is_file() {
            entries += 1;
            bytes += metadata.len();
        }
    }

    if entries > MAX_DELETE_ENTRIES || bytes > MAX_DELETE_BYTES {
        return Err(format!(
            "chat: `{}` holds {entries} files ({bytes} bytes); that is more than a skill, remove it by hand",
            crate::config_paths::display_path(root)
        ));
    }

    Ok((entries, bytes))
}

fn read_target(request: &Request, proxy: &mut dyn ChatToolHost) -> Result<String, String> {
    if proxy.agent_runtime().is_some() {
        crate::agent::files::read(&request.target)
    } else {
        std::fs::read_to_string(&request.target)
    }
    .map_err(|err| format!("chat: failed to read `{}`: {err}", request.relative_file))
}

fn confirm_and_write(
    request: &Request,
    contents: &str,
    message: &str,
    created: bool,
    proxy: &mut dyn ChatToolHost,
) -> Result<String, String> {
    // Content that would not reach the model, or would reach it broken, is
    // refused before anyone is asked about it or it is staged - a question
    // (or a proposal) about a change that was going to be rejected anyway
    // trains people to answer without reading (the same ordering `validate`
    // already follows for the request shape). `apply_skill_write` runs the
    // identical check again on the content it actually writes, so this
    // first pass exists only to reject early, not to replace that one.
    let findings = if request.relative_file == "SKILL.md" {
        skills::lint::lint_skill_md(&request.name, contents)
    } else {
        skills::lint::lint_bundled(&request.relative_file, contents)
    };
    if let Some(reason) = skills::lint::has_rejection(&findings) {
        return Err(format!("chat: {reason}"));
    }

    // Staged rather than written, when policy says so - checked before
    // `confirm_agent_action`, so a staged write never touches the task's
    // status the way falling through to that function would.
    match crate::chatgpt::resolve_skill_staging(proxy) {
        crate::chatgpt::SkillStaging::Off => {}
        crate::chatgpt::SkillStaging::Always => return stage_instead(request, contents),
        crate::chatgpt::SkillStaging::Task
            if proxy.agent_runtime().is_some()
                && !super::agent_write_granted(proxy, &request.target) =>
        {
            return stage_instead(request, contents);
        }
        crate::chatgpt::SkillStaging::Task => {}
    }

    // The same key `edit` and `str_replace` use: the user is deciding about a
    // file, not about which tool happens to write it.
    if !super::confirm_agent_action(proxy, &super::write_approval_key(&request.target), message)? {
        return Ok("Skill change cancelled by user.".to_string());
    }

    let warnings = apply_skill_write(
        &SkillWrite {
            name: &request.name,
            scope: request.scope,
            skill_dir: &request.skill_dir,
            target: &request.target,
            relative_file: &request.relative_file,
            created,
            // Symlink-safe write, which is what a task needs; `write_atomic`
            // renames into place and would follow one. `skill approve` never
            // runs under a task, so it always asks for the plain writer.
            symlink_safe: proxy.agent_runtime().is_some(),
        },
        contents,
    )?;

    Ok(report(request, &request.action, contents.len(), &warnings))
}

/// Everything `apply_skill_write` needs to lint, write and account for an
/// already-approved change - "approved" meaning either a live confirmation
/// or a staged proposal a person just ran `skill approve` on.
pub(crate) struct SkillWrite<'a> {
    pub(crate) name: &'a str,
    pub(crate) scope: SkillScope,
    pub(crate) skill_dir: &'a Path,
    pub(crate) target: &'a Path,
    pub(crate) relative_file: &'a str,
    /// `usage::note_write`'s `created_by` attribution.
    pub(crate) created: bool,
    /// `true` for an agent task's own tool call (needs the symlink-safe
    /// writer); `false` for `skill approve`, which never runs under a task.
    pub(crate) symlink_safe: bool,
}

/// Lint, write, and update bookkeeping for a skill change that has already
/// been approved. Never asks anyone anything - that is the caller's job,
/// whether the answer came from a live confirmation (`confirm_and_write`) or
/// from a person running `skill approve` on a staged proposal.
pub(crate) fn apply_skill_write(w: &SkillWrite<'_>, contents: &str) -> Result<Vec<String>, String> {
    let findings = if w.relative_file == "SKILL.md" {
        skills::lint::lint_skill_md(w.name, contents)
    } else {
        skills::lint::lint_bundled(w.relative_file, contents)
    };
    if let Some(reason) = skills::lint::has_rejection(&findings) {
        return Err(format!("chat: {reason}"));
    }
    let warnings = skills::lint::warnings(findings);

    // Remembered before anything is created, so a half-finished `create` can be
    // rolled back. A leftover empty directory was a dead end: `create` then said
    // "already exists" and `patch` said "failed to read", with nothing the model
    // could do about either.
    let created_dir = !w.skill_dir.exists();

    let written = if w.symlink_safe {
        ensure_parent(w.target).and_then(|()| {
            crate::agent::files::write(w.target, contents).map_err(|err| err.to_string())
        })
    } else {
        crate::atomic_write::write_atomic(w.target, contents, true, "skill")
            .map_err(|err| err.to_string())
    };

    if let Err(err) = written {
        if created_dir {
            let _ = std::fs::remove_dir_all(w.skill_dir);
        }
        return Err(format!(
            "chat: failed to write `{}`: {err}",
            w.relative_file
        ));
    }

    skills::usage::note_write(w.skill_dir, w.scope, w.created);
    refresh_project_trust(w.scope, w.skill_dir);
    // The directory signature is coarse; a same-size rewrite in the same second
    // would otherwise keep serving the previous list.
    skills::clear_skills_fragment_cache();

    Ok(warnings)
}

/// Stage `contents` instead of writing it, and report that back in the same
/// shape a direct write's `report` would.
///
/// `request.target`'s current content (if any) is fingerprinted so
/// `skill approve` can notice a target that moved since this was staged and
/// refuse rather than clobber it.
fn stage_instead(request: &Request, contents: &str) -> Result<String, String> {
    let base_digest = std::fs::read_to_string(&request.target)
        .ok()
        .map(|existing| pending::content_digest(&existing));
    let project_root = if request.scope == SkillScope::Project {
        request.skill_dir.parent().map(Path::to_path_buf)
    } else {
        None
    };

    let proposal = pending::Proposal {
        version: 0, // overwritten by `pending::stage`
        id: pending::proposal_id(request.scope, &request.name, &request.relative_file),
        scope: request.scope.as_str().to_string(),
        name: request.name.clone(),
        file: request.relative_file.clone(),
        // Normalized to what `apply_skill_write` will actually do, not the
        // original tool action: `Proposal.action` documents only two values
        // ("create"/"write_file"), and a `patch` (or a `write_file` against
        // a target that does not exist yet) is a write like any other once
        // its diff has already been applied into `contents`. `base_digest`
        // already carries the same distinction `approve()` reads back.
        action: if base_digest.is_none() {
            "create".to_string()
        } else {
            "write_file".to_string()
        },
        project_root,
        contents: contents.to_string(),
        base_digest,
        created_ms: skills::usage::now_ms(),
        origin: "tool".to_string(),
        note: None,
    };

    let id = pending::stage(proposal)?;
    Ok(json!({
        "action": "staged",
        "id": id,
        "scope": request.scope.as_str(),
        "skill": request.name,
        "path": crate::config_paths::display_path(&request.target),
        "note": "Not written yet - the user reviews it with `skill pending` and applies it with `skill approve`. Do not retry with `edit`.",
    })
    .to_string())
}

/// The user just approved a change to this project's skills, so a trusted root
/// stays trusted rather than asking again about their own edit.
fn refresh_project_trust(scope: SkillScope, skill_dir: &Path) {
    if scope != SkillScope::Project {
        return;
    }
    let Some(root) = skill_dir.parent() else {
        return;
    };
    // `.dsh/skills` by construction: `skill_dir` came from
    // `skills::project_skills_root`, the only project root writes may reach.
    let manager = skills::SkillsManager::with_roots(vec![skills::SkillRoot {
        scope: SkillScope::Project,
        origin: skills::SkillOrigin::Dsh,
        path: root.to_path_buf(),
    }]);
    skills::trust::refresh(root, &skills::trust::digest(&manager.load_skills()));
}

fn ensure_parent(target: &Path) -> Result<(), String> {
    let Some(parent) = target.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)
        .map_err(|err| format!("cannot create `{}`: {err}", parent.display()))
}

fn report(request: &Request, action: &str, bytes: usize, warnings: &[String]) -> String {
    let mut value = json!({
        "action": action,
        "scope": request.scope.as_str(),
        "skill": request.name,
        "path": crate::config_paths::display_path(&request.target),
        "bytes": bytes,
        "note": "The skill list in the system prompt refreshes on the next turn; the path above works now.",
    });
    if !warnings.is_empty() {
        value["warnings"] = json!(warnings);
    }
    value.to_string()
}

/// What "always" remembers for a removal.
///
/// Deliberately not the `write:` key: an earlier "always write this file" must
/// not silently authorise deleting it.
fn delete_approval_key(resolved: &Path) -> String {
    format!("delete:{}", resolved.display())
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Emit only the flat `key: value` frontmatter the reader understands.
///
/// The writer guaranteeing the subset the parser handles is what lets this crate
/// stay off a YAML dependency for two fields.
pub(crate) fn render_skill_md(name: &str, description: &str, body: &str) -> String {
    let mut out = String::from("---\n");
    out.push_str(&format!("name: {name}\n"));
    out.push_str(&format!("description: {}\n", quote_if_needed(description)));
    out.push_str("---\n\n");
    if body.trim().is_empty() {
        out.push_str(&format!("# {name}\n"));
    } else {
        out.push_str(body.trim_end());
        out.push('\n');
    }
    out
}

/// Quote a description that the flat reader would otherwise misread.
fn quote_if_needed(description: &str) -> String {
    let needs_quotes = description.starts_with(['"', '\'', '>', '|', '#', '&', '*', '!', '%', '@'])
        || description.starts_with('-')
        || description.is_empty();
    if !needs_quotes {
        return description.to_string();
    }
    format!(
        "\"{}\"",
        description.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

#[cfg(test)]
mod tests;

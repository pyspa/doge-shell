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

use crate::chatgpt::skills::{self, MAX_DESCRIPTION_CHARS, SkillScope};
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
struct Request {
    action: String,
    name: String,
    scope: SkillScope,
    skill_dir: PathBuf,
    /// Resolved target, canonical as far as it exists.
    target: PathBuf,
    /// Whether `file` was given, which is what separates "delete one file" from
    /// "delete the skill".
    explicit_file: bool,
    relative_file: String,
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
///
/// Ordering matters: a question about a change that was going to be refused
/// anyway trains people to answer without reading.
fn validate(parsed: &Value, proxy: &mut dyn ChatToolHost) -> Result<Request, String> {
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

    let current_dir = proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;

    let root = match scope {
        SkillScope::User => crate::config_paths::skills_dir(),
        SkillScope::Project => skills::project_skills_root(&current_dir).ok_or_else(|| {
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
    // Content that would not reach the model, or would reach it broken,
    // is refused before anyone is asked about it - a question about a change
    // that was going to be rejected anyway trains people to answer without
    // reading (the same ordering `validate` already follows for the request
    // shape).
    let findings = if request.relative_file == "SKILL.md" {
        skills::lint::lint_skill_md(&request.name, contents)
    } else {
        skills::lint::lint_bundled(&request.relative_file, contents)
    };
    if let Some(reason) = skills::lint::has_rejection(&findings) {
        return Err(format!("chat: {reason}"));
    }
    let warnings = skills::lint::warnings(findings);

    // The same key `edit` and `str_replace` use: the user is deciding about a
    // file, not about which tool happens to write it.
    if !super::confirm_agent_action(proxy, &super::write_approval_key(&request.target), message)? {
        return Ok("Skill change cancelled by user.".to_string());
    }

    // Remembered before anything is created, so a half-finished `create` can be
    // rolled back. A leftover empty directory was a dead end: `create` then said
    // "already exists" and `patch` said "failed to read", with nothing the model
    // could do about either.
    let created_dir = !request.skill_dir.exists();

    let written = if proxy.agent_runtime().is_some() {
        // Symlink-safe write, which is what a task needs; `write_atomic`
        // renames into place and would follow one.
        ensure_parent(&request.target).and_then(|()| {
            crate::agent::files::write(&request.target, contents).map_err(|err| err.to_string())
        })
    } else {
        crate::atomic_write::write_atomic(&request.target, contents, true, "skill")
            .map_err(|err| err.to_string())
    };

    if let Err(err) = written {
        if created_dir {
            let _ = std::fs::remove_dir_all(&request.skill_dir);
        }
        return Err(format!(
            "chat: failed to write `{}`: {err}",
            request.relative_file
        ));
    }

    skills::usage::note_write(&request.skill_dir, request.scope, created);
    refresh_project_trust(request);
    // The directory signature is coarse; a same-size rewrite in the same second
    // would otherwise keep serving the previous list.
    skills::clear_skills_fragment_cache();

    Ok(report(request, &request.action, contents.len(), &warnings))
}

/// The user just approved a change to this project's skills, so a trusted root
/// stays trusted rather than asking again about their own edit.
fn refresh_project_trust(request: &Request) {
    if request.scope != SkillScope::Project {
        return;
    }
    let Some(root) = request.skill_dir.parent() else {
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
fn render_skill_md(name: &str, description: &str, body: &str) -> String {
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
mod tests {
    use super::*;
    use crate::shell_capabilities::ApprovalDecision;
    use crate::test_support::TestShellProxy;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    type TestProxy = TestShellProxy;

    /// `skills_dir()` is read from the environment, so a test that writes a
    /// personal skill has to own `XDG_CONFIG_HOME` for its duration.
    fn with_config_home<R>(dir: &Path, f: impl FnOnce() -> R) -> R {
        let _lock = crate::chatgpt::tool::execute::tests::env_lock();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: single-threaded under `env_lock`.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir) };
        let result = f();
        match previous {
            Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        result
    }

    fn project(dir: &Path) -> PathBuf {
        let root = std::fs::canonicalize(dir).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        root
    }

    fn proxy(cwd: PathBuf) -> TestProxy {
        TestProxy {
            current_dir: cwd,
            confirm_result: true,
            ..TestProxy::default()
        }
    }

    /// This schema is sent on every turn whether or not a skill is ever
    /// touched, and it was the largest of the nine - bigger than `search`,
    /// nearly twice `execute`. Detail belongs in the errors, which are only
    /// paid for when something actually goes wrong.
    #[test]
    fn the_schema_stays_small_enough_to_carry_every_turn() {
        let rendered = definition().to_string();
        assert!(
            rendered.len() < 1400,
            "skill_manage schema is {} bytes",
            rendered.len()
        );
    }

    #[test]
    fn create_writes_skill_md_with_generated_frontmatter() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root.clone());

        let result = run(
            r##"{"action":"create","name":"rust-bisect","scope":"project","description":"Use when a regression must be located in git history","body":"# Rust bisect\n\n1. build\n"}"##,
            &mut p,
        )
        .unwrap();

        let path = root.join(".dsh/skills/rust-bisect/SKILL.md");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.starts_with("---\nname: rust-bisect\n"));
        assert!(
            written.contains("description: Use when a regression must be located in git history\n")
        );
        assert!(written.contains("1. build"));
        assert!(result.contains("rust-bisect"), "{result}");
    }

    /// The generated frontmatter has to survive the reader it was written for.
    #[test]
    fn a_created_description_with_a_colon_round_trips_through_the_frontmatter_reader() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root.clone());

        run(
            r#"{"action":"create","name":"deploy","scope":"project","description":"Use for: staging and prod","body":"x"}"#,
            &mut p,
        )
        .unwrap();

        let manager = skills::SkillsManager::with_roots(vec![skills::SkillRoot {
            scope: SkillScope::Project,
            origin: skills::SkillOrigin::Dsh,
            path: root.join(".dsh/skills"),
        }]);
        let loaded = manager.load_skills();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].summary(), "Use for: staging and prod");
    }

    #[test]
    fn create_is_refused_without_a_description() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root);

        let err = run(
            r#"{"action":"create","name":"nameless","scope":"project","body":"x"}"#,
            &mut p,
        )
        .expect_err("a skill with no trigger is invisible");
        assert!(err.contains("description"), "{err}");
    }

    #[test]
    fn a_name_with_a_slash_or_uppercase_is_refused() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root);

        for name in ["../escape", "Upper", "trailing-", "double--hyphen", ""] {
            let args =
                json!({"action": "create", "name": name, "scope": "project", "description": "d"});
            let err =
                run(&args.to_string(), &mut p).expect_err("bad skill name must not reach the disk");
            assert!(
                err.contains("skill name") || err.contains("requires"),
                "{name}: {err}"
            );
        }
    }

    #[test]
    fn a_file_escaping_the_skill_directory_is_refused() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root);

        let err = run(
            r#"{"action":"write_file","name":"demo","scope":"project","file":"../../pwned.md","contents":"x"}"#,
            &mut p,
        )
        .expect_err("a traversal must not escape");
        assert!(err.contains("inside the skill directory"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_skill_directory_cannot_escape_it() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let outside = tempdir().unwrap();
        let skill = root.join(".dsh/skills/demo");
        std::fs::create_dir_all(&skill).unwrap();
        symlink(outside.path(), skill.join("refs")).unwrap();
        let mut p = proxy(root);

        let err = run(
            r#"{"action":"write_file","name":"demo","scope":"project","file":"refs/pwned.md","contents":"x"}"#,
            &mut p,
        )
        .expect_err("a symlink must not lead out of the skill");
        assert!(err.contains("outside the skill directory"), "{err}");
        assert!(!outside.path().join("pwned.md").exists());
    }

    #[test]
    fn a_denied_answer_writes_nothing() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut p = TestProxy {
            current_dir: root.clone(),
            confirm_counter: Some(calls.clone()),
            confirm_result: false,
            ..TestProxy::default()
        };

        let result = run(
            r#"{"action":"create","name":"demo","scope":"project","description":"d"}"#,
            &mut p,
        )
        .unwrap();

        assert_eq!(result, "Skill change cancelled by user.");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!root.join(".dsh/skills/demo").exists());
    }

    /// One "always" per file, shared with `edit`: the user approved the file,
    /// not the tool that happened to write it.
    #[test]
    fn an_always_answer_uses_the_same_write_key_as_edit() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut p = TestProxy {
            current_dir: root.clone(),
            confirm_counter: Some(calls.clone()),
            approval_decision: Some(ApprovalDecision::AllowAlways),
            ..TestProxy::default()
        };

        run(
            r#"{"action":"create","name":"demo","scope":"project","description":"d","body":"one"}"#,
            &mut p,
        )
        .unwrap();
        run(
            r#"{"action":"patch","name":"demo","scope":"project","old_string":"one","new_string":"two"}"#,
            &mut p,
        )
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let written = std::fs::read_to_string(root.join(".dsh/skills/demo/SKILL.md")).unwrap();
        assert!(written.contains("two"));

        let key = super::super::write_approval_key(
            &std::fs::canonicalize(root.join(".dsh/skills/demo/SKILL.md")).unwrap(),
        );
        assert!(p.agent_session_allowlist.contains(&key), "{key}");
    }

    /// A standing "always write this file" must not quietly authorise removing
    /// the skill it belongs to.
    #[test]
    fn delete_uses_its_own_key_so_a_write_always_does_not_cover_it() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut p = TestProxy {
            current_dir: root.clone(),
            confirm_counter: Some(calls.clone()),
            approval_decision: Some(ApprovalDecision::AllowAlways),
            ..TestProxy::default()
        };

        run(
            r#"{"action":"create","name":"demo","scope":"project","description":"d"}"#,
            &mut p,
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        run(
            r#"{"action":"delete","name":"demo","scope":"project"}"#,
            &mut p,
        )
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2, "delete must ask again");
        assert!(!root.join(".dsh/skills/demo").exists());
    }

    #[test]
    fn patch_requires_a_unique_match() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root.clone());

        run(
            r#"{"action":"create","name":"demo","scope":"project","description":"d","body":"step\nstep\n"}"#,
            &mut p,
        )
        .unwrap();

        let err = run(
            r#"{"action":"patch","name":"demo","scope":"project","old_string":"step","new_string":"done"}"#,
            &mut p,
        )
        .expect_err("an ambiguous patch is a different edit than intended");
        assert!(err.contains("appears 2 times"), "{err}");
    }

    /// A `patch` is applied before it is linted, so this proves the lint sees
    /// the result of the edit, not the input to it.
    #[test]
    fn a_patch_that_removes_the_description_is_refused() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root.clone());

        run(
            r#"{"action":"create","name":"demo","scope":"project","description":"Use when demoing","body":"step one\n"}"#,
            &mut p,
        )
        .unwrap();
        let before = std::fs::read_to_string(root.join(".dsh/skills/demo/SKILL.md")).unwrap();
        let calls_before = p.confirm_calls;

        let err = run(
            r#"{"action":"patch","name":"demo","scope":"project","old_string":"description: Use when demoing\n","new_string":""}"#,
            &mut p,
        )
        .expect_err("a skill with no description falls out of the prompt");
        assert!(err.contains("description"), "{err}");

        // Refused before anyone was asked, and before anything on disk moved.
        assert_eq!(p.confirm_calls, calls_before);
        let after = std::fs::read_to_string(root.join(".dsh/skills/demo/SKILL.md")).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn a_patch_that_renames_the_frontmatter_name_is_refused() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root.clone());

        run(
            r#"{"action":"create","name":"demo","scope":"project","description":"Use when demoing","body":"step\n"}"#,
            &mut p,
        )
        .unwrap();

        let err = run(
            r#"{"action":"patch","name":"demo","scope":"project","old_string":"name: demo","new_string":"name: renamed"}"#,
            &mut p,
        )
        .expect_err("a frontmatter name that no longer matches its directory is reported broken, not shown");
        assert!(err.contains("does not match"), "{err}");
    }

    #[test]
    fn a_write_file_of_skill_md_without_frontmatter_is_refused() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root.clone());

        run(
            r#"{"action":"create","name":"demo","scope":"project","description":"Use when demoing","body":"step\n"}"#,
            &mut p,
        )
        .unwrap();

        let err = run(
            r#"{"action":"write_file","name":"demo","scope":"project","contents":"just some text, no frontmatter"}"#,
            &mut p,
        )
        .expect_err("SKILL.md without frontmatter has no description to trigger on");
        assert!(err.contains("frontmatter"), "{err}");
    }

    #[test]
    fn an_indented_description_is_refused_because_the_reader_cannot_see_it() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root.clone());

        let contents =
            "---\nname: demo\nmetadata:\n  description: nested under metadata\n---\n\nbody\n";
        let args = json!({
            "action": "write_file",
            "name": "demo",
            "scope": "project",
            "contents": contents,
        });
        let err = run(&args.to_string(), &mut p)
            .expect_err("a nested description is invisible to the flat reader");
        assert!(err.contains("indented under another key"), "{err}");
    }

    /// Long enough to blow the prompt budget but short of the hard limit: the
    /// write goes through, and the model is told why the trigger may not fire.
    #[test]
    fn a_long_description_is_written_but_warned_about() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root.clone());

        // Longer than the prompt's display budget (240) but within the hard
        // write limit (300): the write goes through, with a warning.
        let description = "Use when ".to_string() + &"x".repeat(260);
        let args = json!({
            "action": "create",
            "name": "demo",
            "scope": "project",
            "description": description,
            "body": "step",
        });
        let result = run(&args.to_string(), &mut p).unwrap();
        assert!(result.contains("warnings"), "{result}");
        assert!(root.join(".dsh/skills/demo/SKILL.md").exists());
    }

    /// `references/`, `scripts/` and `assets/` are never parsed for
    /// frontmatter, so the same content that would fail as `SKILL.md` is fine
    /// here.
    #[test]
    fn a_bundled_reference_file_is_not_linted_as_a_skill() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root.clone());

        run(
            r#"{"action":"create","name":"demo","scope":"project","description":"Use when demoing","body":"step\n"}"#,
            &mut p,
        )
        .unwrap();

        let args = json!({
            "action": "write_file",
            "name": "demo",
            "scope": "project",
            "file": "references/notes.md",
            "contents": "just some notes, no frontmatter here",
        });
        run(&args.to_string(), &mut p).unwrap();
        assert!(root.join(".dsh/skills/demo/references/notes.md").exists());
    }

    #[test]
    fn project_scope_is_refused_without_a_project() {
        let dir = tempdir().unwrap();
        let plain = std::fs::canonicalize(dir.path()).unwrap();
        let mut p = proxy(plain);

        let err = run(
            r#"{"action":"create","name":"demo","scope":"project","description":"d"}"#,
            &mut p,
        )
        .expect_err("there is no project here");
        assert!(err.contains("no project here"), "{err}");
    }

    /// `.agents/skills` is shared with other tools. It is read, never written:
    /// a directory this shell does not own is not a place for it to leave
    /// files, and `scope: "project"` has exactly one destination.
    #[test]
    fn project_scope_writes_to_dsh_skills_and_never_to_the_agents_root() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        std::fs::create_dir_all(root.join(".agents/skills")).unwrap();
        let mut p = proxy(root.clone());

        run(
            r#"{"action":"create","name":"demo","scope":"project","description":"d"}"#,
            &mut p,
        )
        .expect("create");

        assert!(root.join(".dsh/skills/demo/SKILL.md").is_file());
        assert!(!root.join(".agents/skills/demo").exists());
        // The tool takes no third scope, so there is no spelling that reaches
        // the shared root at all.
        assert!(
            run(
                r#"{"action":"create","name":"other","scope":"project-agents","description":"d"}"#,
                &mut p,
            )
            .is_err()
        );
    }

    /// A write that fails after the directory is made left `create` saying
    /// "already exists" and `patch` saying "failed to read" - nowhere to go.
    #[cfg(unix)]
    #[test]
    fn a_failed_create_leaves_no_directory_behind() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let skills = root.join(".dsh/skills");
        std::fs::create_dir_all(&skills).unwrap();
        // Nothing can be created underneath, so the write fails after the
        // request has been validated and approved.
        std::fs::set_permissions(&skills, std::fs::Permissions::from_mode(0o500)).unwrap();
        let mut p = proxy(root.clone());

        let err = run(
            r#"{"action":"create","name":"doomed","scope":"project","description":"d"}"#,
            &mut p,
        )
        .expect_err("the write cannot succeed");

        std::fs::set_permissions(&skills, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(err.contains("failed to write"), "{err}");
        assert!(
            !skills.join("doomed").exists(),
            "a half-made skill must not block the retry"
        );
    }

    /// `foo/` beside `foo.md` is two skills with one name; the loader has to
    /// pick and the other disappears.
    #[test]
    fn create_refuses_to_shadow_an_existing_file_skill() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let skills = root.join(".dsh/skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("deploy.md"), "---\ndescription: d\n---\n").unwrap();
        let mut p = proxy(root);

        let err = run(
            r#"{"action":"create","name":"deploy","scope":"project","description":"d"}"#,
            &mut p,
        )
        .expect_err("two skills would answer to one name");
        assert!(err.contains("already exists beside it"), "{err}");
    }

    #[test]
    fn create_refuses_to_overwrite_an_existing_skill() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root.clone());

        run(
            r#"{"action":"create","name":"demo","scope":"project","description":"first"}"#,
            &mut p,
        )
        .unwrap();
        let err = run(
            r#"{"action":"create","name":"demo","scope":"project","description":"second"}"#,
            &mut p,
        )
        .expect_err("create must not silently replace a skill");
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn user_scope_writes_into_the_configuration_directory() {
        let config = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        let cwd_path = std::fs::canonicalize(cwd.path()).unwrap();

        with_config_home(config.path(), || {
            let mut p = proxy(cwd_path.clone());
            run(
                r#"{"action":"create","name":"portable","description":"works anywhere"}"#,
                &mut p,
            )
            .unwrap();
        });

        assert!(config.path().join("dsh/skills/portable/SKILL.md").is_file());
    }

    #[test]
    fn deleting_skill_md_by_name_points_at_deleting_the_skill() {
        let dir = tempdir().unwrap();
        let root = project(dir.path());
        let mut p = proxy(root);

        let err = run(
            r#"{"action":"delete","name":"demo","scope":"project","file":"SKILL.md"}"#,
            &mut p,
        )
        .expect_err("removing SKILL.md leaves a directory that is not a skill");
        assert!(err.contains("omit `file`"), "{err}");
    }

    #[test]
    fn a_description_that_starts_like_yaml_syntax_is_quoted() {
        assert_eq!(quote_if_needed("plain text"), "plain text");
        assert_eq!(
            quote_if_needed("- looks like a list"),
            "\"- looks like a list\""
        );
        assert_eq!(quote_if_needed("\"quoted\""), "\"\\\"quoted\\\"\"");
    }
}

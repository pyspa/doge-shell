//! Paged file reads, optional skill-relative references, and existing safety gates.
use crate::safety_policy;
use crate::shell_capabilities::ChatToolHost;
use serde_json::{Value, json};
use std::fs;

pub(crate) const NAME: &str = "read_file";

/// Lines returned when the caller does not ask for a specific window.
pub(crate) const DEFAULT_LINE_LIMIT: usize = 400;
/// Upper bound for an explicit `limit`.
const MAX_LINE_LIMIT: usize = 2000;
/// Byte budget for the rendered window, checked before the global tool cap.
const MAX_READ_BYTES: usize = 6144;

pub(crate) fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": NAME,
            "description": "Read a line-numbered file window. Continue with `offset`. Use `skill_path` to resolve a relative resource inside one skill.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path; relative to cwd, or to skill_path when supplied."
                    },
                    "skill_path": {
                        "type": "string",
                        "description": "SKILL.md base for a relative bundled resource."
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "First line (1-based; default 1)."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of lines to return. Defaults to 400."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }
    })
}

/// Render `contents` as a line-numbered window starting at `offset` (1-based).
fn render_window(path_label: &str, contents: &str, offset: usize, limit: usize) -> String {
    if contents.is_empty() {
        return format!("{path_label}: (empty file)");
    }

    let lines: Vec<&str> = contents.lines().collect();
    let total = lines.len();

    if offset > total {
        return format!(
            "{path_label}: offset {offset} is past the end of the file ({total} lines total)"
        );
    }

    let start = offset - 1;
    let mut rendered = String::new();
    let mut last_line = start;
    let mut byte_budget_hit = false;

    for (index, line) in lines.iter().enumerate().skip(start).take(limit) {
        let entry = format!("{:>6}\t{}\n", index + 1, line);
        if !rendered.is_empty() && rendered.len() + entry.len() > MAX_READ_BYTES {
            byte_budget_hit = true;
            break;
        }
        rendered.push_str(&entry);
        last_line = index + 1;
    }

    // A single line longer than the whole budget still has to be returned.
    if rendered.is_empty() {
        let line = lines[start];
        let end = line.floor_char_boundary(MAX_READ_BYTES);
        rendered = format!("{:>6}\t{}\n", start + 1, &line[..end]);
        last_line = start + 1;
        byte_budget_hit = end < line.len();
    }

    let mut header = format!("{path_label}: lines {offset}-{last_line} of {total}");
    if byte_budget_hit {
        header.push_str(" (stopped early: byte budget reached)");
    }
    header.push('\n');

    let mut out = header;
    out.push_str(&rendered);

    if last_line < total {
        let next = last_line + 1;
        out.push_str(&format!(
            "\n... {} more lines. Continue with read_file(path=\"{path_label}\", offset={next}).\n",
            total - last_line
        ));
    }

    out
}

/// An optional skill base never expands file-tool authority. Resolve both
/// paths through the normal gate, then confine the result to one skill bundle.
pub(super) fn resolve_read_path(
    parsed: &Value,
    proxy: &mut dyn ChatToolHost,
) -> Result<std::path::PathBuf, String> {
    let path = parsed["path"].as_str().ok_or("read_file requires path")?;
    let Some(base) = parsed.get("skill_path") else {
        return super::resolve_tool_path(path, proxy);
    };
    let base = base.as_str().ok_or("skill_path must be a string")?;
    let cwd = proxy.get_current_dir().map_err(|err| err.to_string())?;
    let expanded = shellexpand::full(base).map_err(|err| err.to_string())?;
    let lexical = cwd.join(expanded.as_ref());
    if lexical.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
        return Err(
            "skill_path must name SKILL.md (bare .md skills have no resource bundle)".into(),
        );
    }
    let bundle = super::resolve_tool_path(
        &lexical
            .parent()
            .ok_or("skill_path has no parent")?
            .to_string_lossy(),
        proxy,
    )?;
    let (owner, _) = crate::chatgpt::skills::containing_skill(&bundle, &cwd)
        .ok_or("skill_path must name an instruction file under a configured skill root")?;
    if !bundle.is_dir() || bundle != owner {
        return Err("skill_path must name a skill's top-level SKILL.md".into());
    }
    let instruction = super::resolve_tool_path(base, proxy)?;
    if !instruction.is_file() || !instruction.starts_with(&bundle) {
        return Err("skill instruction escapes its bundle or is not a regular file".into());
    }
    if std::path::Path::new(path).is_absolute() || path.starts_with('~') || path.contains('$') {
        return Err("a skill resource path must be relative and contain no expansions".into());
    }
    let resolved = super::resolve_tool_path(&bundle.join(path).to_string_lossy(), proxy)?;
    let bundle = std::fs::canonicalize(bundle).map_err(|err| err.to_string())?;
    if !resolved.starts_with(bundle) {
        return Err("skill resource escapes its bundle".into());
    }
    Ok(resolved)
}

pub(crate) fn run(arguments: &str, _proxy: &mut dyn ChatToolHost) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(arguments)
        .map_err(|err| format!("chat: invalid JSON arguments for read_file tool: {err}"))?;

    let path_value = parsed
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "chat: read_file tool requires `path`".to_string())?;

    if path_value.trim().is_empty() {
        return Err("chat: read_file tool path must not be empty".to_string());
    }

    let normalized_abs_path = resolve_read_path(&parsed, _proxy)?;

    // Get CWD for gitignore check
    let current_dir = _proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;
    let normalized_current_dir =
        std::fs::canonicalize(&current_dir).unwrap_or_else(|_| super::normalize_path(&current_dir));

    super::reject_gitignored_read_path(&normalized_abs_path, &normalized_current_dir, path_value)?;

    if let Some(reason) = super::sensitive_path_reason(&normalized_abs_path)
        && !super::confirm_sensitive_access(
            _proxy,
            "read",
            path_value,
            &normalized_abs_path,
            reason,
        )?
    {
        return Ok("read_file cancelled by user.".to_string());
    }

    let contents = if _proxy.agent_runtime().is_some() {
        crate::agent::files::read(&normalized_abs_path)
    } else {
        fs::read_to_string(&normalized_abs_path)
    }
    .map_err(|err| format!("chat: failed to read file `{path_value}`: {err}"))?;

    if safety_policy::contains_sensitive_text(&contents)
        && !super::confirm_sensitive_access(
            _proxy,
            "read",
            path_value,
            &normalized_abs_path,
            "sensitive content",
        )?
    {
        return Ok("read_file cancelled by user.".to_string());
    }

    let offset = parsed
        .get("offset")
        .and_then(|v| v.as_u64())
        .map(|v| v.max(1) as usize)
        .unwrap_or(1);
    let limit = parsed
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| (v.max(1) as usize).min(MAX_LINE_LIMIT))
        .unwrap_or(DEFAULT_LINE_LIMIT);

    // Attribute the read before returning, so `skill list` can tell a skill that
    // earns its place in every prompt from one nobody has opened since it was
    // written. A no-op for every path outside a skill root.
    crate::chatgpt::skills::note_skill_read(&normalized_abs_path, &normalized_current_dir);

    let label = if parsed.get("skill_path").is_some() {
        normalized_abs_path.to_string_lossy().into_owned()
    } else {
        path_value.to_string()
    };
    Ok(render_window(&label, &contents, offset, limit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use crate::test_support::TestShellProxy;
    type NoopProxy = TestShellProxy;

    #[test]
    fn test_read_file_success() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "Hello, world!").unwrap();

        let mut proxy = proxy(dir.path());

        let result = run(r#"{"path": "test.txt"}"#, &mut proxy).unwrap();
        assert!(result.contains("test.txt: lines 1-1 of 1"));
        assert!(result.contains("     1\tHello, world!"));
    }

    #[test]
    fn test_read_file_not_found() {
        let dir = tempdir().unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(r#"{"path": "missing.txt"}"#, &mut proxy);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_file_absolute_path() {
        let dir = tempdir().unwrap();
        let mut proxy = proxy(dir.path());
        let result = run(r#"{"path": "/etc/passwd"}"#, &mut proxy);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("resolves outside allowed directories")
        );
    }

    #[test]
    fn test_read_file_parent_traversal() {
        let dir = tempdir().unwrap();
        let mut proxy = proxy(dir.path());
        let result = run(r#"{"path": "../secret.txt"}"#, &mut proxy);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed directories"));
    }

    #[cfg(unix)]
    #[test]
    fn test_read_file_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let base = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir_all(base.path().join("inside")).unwrap();
        fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        symlink(outside.path(), base.path().join("inside/link_out")).unwrap();
        let mut proxy = proxy(base.path());

        let result = run(r#"{"path": "inside/link_out/secret.txt"}"#, &mut proxy);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("outside allowed directories"));
    }

    fn proxy(path: &std::path::Path) -> NoopProxy {
        NoopProxy {
            current_dir: path.to_path_buf(),
            confirm_result: true,
            ..NoopProxy::default()
        }
    }

    #[test]
    fn test_read_file_requires_confirmation_for_sensitive_path() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".env.local"), "APP_MODE=dev").unwrap();
        let mut proxy = NoopProxy {
            current_dir: dir.path().to_path_buf(),
            confirm_result: false,
            ..NoopProxy::default()
        };

        let result = run(r#"{"path": ".env.local"}"#, &mut proxy).unwrap();

        assert_eq!(result, "read_file cancelled by user.");
        assert_eq!(proxy.confirm_calls, 1);
    }

    #[test]
    fn test_read_file_requires_confirmation_for_sensitive_content() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("config.txt"), "API_KEY=secret").unwrap();
        let mut proxy = NoopProxy {
            current_dir: dir.path().to_path_buf(),
            confirm_result: false,
            ..NoopProxy::default()
        };

        let result = run(r#"{"path": "config.txt"}"#, &mut proxy).unwrap();

        assert_eq!(result, "read_file cancelled by user.");
        assert_eq!(proxy.confirm_calls, 1);
    }
    #[test]
    fn read_file_pages_large_files_with_offset() {
        let dir = tempdir().unwrap();
        let body: String = (1..=1000).map(|i| format!("line {i}\n")).collect();
        fs::write(dir.path().join("big.txt"), &body).unwrap();
        let mut proxy = proxy(dir.path());

        let first = run(r#"{"path": "big.txt", "limit": 10}"#, &mut proxy).unwrap();
        assert!(first.contains("big.txt: lines 1-10 of 1000"));
        assert!(first.contains("     1\tline 1"));
        assert!(first.contains("    10\tline 10"));
        assert!(!first.contains("line 11\n"));
        assert!(first.contains("offset=11"));

        let second = run(
            r#"{"path": "big.txt", "offset": 11, "limit": 10}"#,
            &mut proxy,
        )
        .unwrap();
        assert!(second.contains("big.txt: lines 11-20 of 1000"));
        assert!(second.contains("    11\tline 11"));
    }

    #[test]
    fn read_file_reports_offset_past_end() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("small.txt"), "a\nb\n").unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(r#"{"path": "small.txt", "offset": 99}"#, &mut proxy).unwrap();
        assert!(result.contains("offset 99 is past the end"));
        assert!(result.contains("2 lines total"));
    }

    #[test]
    fn read_file_stops_at_byte_budget_and_points_at_the_next_line() {
        let dir = tempdir().unwrap();
        let body: String = (0..500)
            .map(|i| format!("{i}{}\n", "x".repeat(60)))
            .collect();
        fs::write(dir.path().join("wide.txt"), &body).unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(r#"{"path": "wide.txt"}"#, &mut proxy).unwrap();
        assert!(result.contains("stopped early: byte budget reached"));
        assert!(result.contains("offset="));
        assert!(result.len() < MAX_READ_BYTES * 2);
    }

    #[test]
    fn read_file_reports_empty_file() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("empty.txt"), "").unwrap();
        let mut proxy = proxy(dir.path());

        let result = run(r#"{"path": "empty.txt"}"#, &mut proxy).unwrap();
        assert_eq!(result, "empty.txt: (empty file)");
    }
}

#[cfg(test)]
#[path = "read/skills_tests.rs"]
mod skills_tests;

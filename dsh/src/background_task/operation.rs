//! Host normalization and exact preimages; no model-provided permission facts.
use dsh_types::background_task::{Operation, TaskScope};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};

const MAX_FILE: usize = 1024 * 1024;

fn resolve(raw: &str, base: &Path) -> Result<PathBuf, String> {
    if raw.is_empty() || raw.contains(['$', '~', '\0']) {
        return Err("background paths must be explicit, without expansion".into());
    }
    let path = Path::new(raw);
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("background paths cannot contain parent traversal".into());
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .ok_or("invalid background path")?
                .to_owned(),
        );
        ancestor = ancestor.parent().ok_or("background path has no parent")?;
    }
    let mut result = ancestor.canonicalize().map_err(|e| e.to_string())?;
    for part in suffix.into_iter().rev() {
        result.push(part);
    }
    Ok(result)
}

pub(super) fn normalize_operation(
    name: &str,
    arguments: &str,
    scope: &TaskScope,
) -> Result<(Operation, bool), String> {
    let mut args: Value =
        serde_json::from_str(arguments).map_err(|_| "invalid task tool arguments")?;
    if !args.is_object() {
        return Err("task tool arguments must be an object".into());
    }
    let mut preimage = None;
    let mut content = None;
    let path = match name {
        "read_file" | "ls" | "edit" | "str_replace" => {
            let raw = args["path"]
                .as_str()
                .unwrap_or(if name == "ls" { "." } else { "" });
            let path = if let Some(base) = args["skill_path"].as_str() {
                if name != "read_file" || Path::new(raw).is_absolute() {
                    return Err("invalid bundled resource path".into());
                }
                let instruction = resolve(base, &scope.cwd)?;
                if instruction.file_name().is_none_or(|n| n != "SKILL.md") {
                    return Err("bundled resources require a SKILL.md base".into());
                }
                let bundle = instruction.parent().ok_or("invalid skill base")?;
                let target = resolve(raw, bundle)?;
                if !target.starts_with(bundle) {
                    return Err("resource escapes skill bundle".into());
                }
                args.as_object_mut().unwrap().remove("skill_path");
                target
            } else {
                resolve(raw, &scope.cwd)?
            };
            let readable = scope
                .read_roots
                .iter()
                .chain(&scope.write_roots)
                .any(|r| path.starts_with(r));
            // Never inspect an out-of-scope file merely to prepare a question.
            if !readable {
                return Err("task path is outside its fixed roots".into());
            }
            if name != "ls" && path.exists() {
                let text = dsh_builtin::agent::files::read(&path).map_err(|e| e.to_string())?;
                if text.len() > MAX_FILE {
                    return Err("background file exceeds 1 MiB".into());
                }
                preimage = Some(text);
            }
            args["path"] = json!(path);
            if name == "edit" {
                content = Some(
                    args["contents"]
                        .as_str()
                        .ok_or("edit contents required")?
                        .to_owned(),
                );
            } else if name == "str_replace" {
                let old = args["old_string"].as_str().ok_or("old_string required")?;
                let new = args["new_string"].as_str().ok_or("new_string required")?;
                let before = preimage.as_deref().ok_or("replace target does not exist")?;
                if old.is_empty() {
                    return Err("old_string must not be empty".into());
                }
                content = Some(if args["replace_all"] == true {
                    before.replace(old, new)
                } else {
                    before.replacen(old, new, 1)
                });
            }
            if content.as_ref().is_some_and(|s| s.len() > MAX_FILE) {
                return Err("background edit exceeds 1 MiB".into());
            }
            path
        }
        "execute" => {
            if args["command"].as_str().is_none_or(|s| s.trim().is_empty()) {
                return Err("execute command required".into());
            }
            if args["command"]
                .as_str()
                .is_some_and(|command| command.len() > 2048)
            {
                return Err(
                    "background command exceeds the complete approval preview limit".into(),
                );
            }
            let path = resolve(
                args["cwd"]
                    .as_str()
                    .unwrap_or(scope.cwd.to_str().ok_or("non UTF-8 cwd")?),
                &scope.cwd,
            )?;
            if !scope.read_roots.iter().any(|root| path.starts_with(root)) {
                return Err("execute cwd is outside the task roots".into());
            }
            if !path.is_dir() {
                return Err("execute cwd is not a directory".into());
            }
            args["cwd"] = json!(path);
            args["yield_time_ms"] = json!(0);
            let requested = args["timeout_ms"].as_u64().unwrap_or(scope.budget.time_ms);
            args["timeout_ms"] = json!(requested.min(scope.budget.time_ms));
            path
        }
        _ => return Err("unsupported background tool".into()),
    };
    let protected = path.components().any(|c| {
        matches!(c, Component::Normal(n)
        if n == ".git" || n == ".github" || n == ".dogesh" || n == ".agents" || n == "config.lisp")
    });
    let manual = protected
        || dsh_types::safety_policy::is_sensitive_path(&path)
        || preimage
            .as_deref()
            .is_some_and(dsh_types::safety_policy::contains_sensitive_text)
        || content
            .as_deref()
            .is_some_and(dsh_types::safety_policy::contains_sensitive_text);
    Ok((
        Operation {
            tool: name.into(),
            args,
            canonical_targets: vec![path],
            preimage_digest: preimage,
            content_digest: content,
        },
        manual,
    ))
}

pub(super) fn revalidate_operation(op: &Operation, scope: &TaskScope) -> Result<Operation, String> {
    normalize_operation(&op.tool, &op.args.to_string(), scope).map(|(op, _)| op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_types::background_task::{BudgetLimits, Profile};
    fn scope(root: &Path) -> TaskScope {
        TaskScope {
            profile: Profile::WorkspaceEdit,
            cwd: root.into(),
            read_roots: vec![root.into()],
            write_roots: vec![root.into()],
            environment_names: vec![],
            network_hosts: vec![],
            sandbox_verified: false,
            budget: BudgetLimits {
                time_ms: 10000,
                tool_calls: 10,
                tokens: 1000,
                output_bytes: 10000,
            },
        }
    }
    #[test]
    fn approval_identity_detects_changed_preimage() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::write(root.join("file"), "before").unwrap();
        let scope = scope(&root);
        let (operation, _) =
            normalize_operation("edit", r#"{"path":"file","contents":"after"}"#, &scope).unwrap();
        assert_eq!(operation.preimage_digest.as_deref(), Some("before"));
        std::fs::write(root.join("file"), "changed").unwrap();
        assert_ne!(operation, revalidate_operation(&operation, &scope).unwrap());
    }
    #[test]
    fn rejects_scope_escape_and_unknown_tool() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let scope = scope(&root);
        for (name, args) in [
            ("read_file", r#"{"path":"../outside"}"#),
            ("execute", r#"{"command":"pwd","cwd":"/"}"#),
            ("mcp_unknown", "{}"),
        ] {
            assert!(normalize_operation(name, args, &scope).is_err());
        }
        std::os::unix::fs::symlink("/etc", root.join("escape")).unwrap();
        assert!(normalize_operation("read_file", r#"{"path":"escape/passwd"}"#, &scope).is_err());
    }
    #[test]
    fn rejects_resource_escape_and_protects_control_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let scope = scope(&root);
        std::fs::create_dir(root.join("bundle")).unwrap();
        std::fs::write(root.join("bundle/SKILL.md"), "skill").unwrap();
        assert!(
            normalize_operation(
                "read_file",
                r#"{"skill_path":"bundle/SKILL.md","path":"../other"}"#,
                &scope
            )
            .is_err()
        );
        let (_, manual) =
            normalize_operation("edit", r#"{"path":"config.lisp","contents":"x"}"#, &scope)
                .unwrap();
        assert!(manual);
    }
}

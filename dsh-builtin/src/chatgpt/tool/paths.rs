//! Resolving a tool's path argument to somewhere it may actually touch: expand `~`, canonicalize (or resolve through the nearest existing ancestor for a not-yet-created file), and confirm it falls under the
//! project root, the shell's skills directory, or - for an agent task - an explicit read/write grant (`resolve_tool_path`). `workspace_root` is the one place that climbs to the outermost enclosing project.
use super::*;

pub(crate) fn normalize_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {
                // Skip current directory components
            }
            _ => {
                normalized.push(component);
            }
        }
    }
    normalized
}
pub(crate) fn tool_skills_dir() -> PathBuf {
    crate::config_paths::skills_dir()
}
pub(crate) fn canonicalize_or_normalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize_path(path))
}
pub(crate) fn resolve_with_existing_ancestor(path: &Path) -> Result<PathBuf, String> {
    let mut current = path.to_path_buf();
    let mut suffix = PathBuf::new();

    loop {
        if current.exists() {
            let canonical = std::fs::canonicalize(&current).map_err(|err| {
                format!(
                    "chat: failed to canonicalize path ancestor `{}`: {err}",
                    current.display()
                )
            })?;
            return Ok(if suffix.as_os_str().is_empty() {
                canonical
            } else {
                canonical.join(suffix)
            });
        }

        let name = current.file_name().ok_or_else(|| {
            format!(
                "chat: path `{}` has no existing ancestor",
                path.to_string_lossy()
            )
        })?;
        // `Path::join` on an empty path appends a separator, so building the
        // suffix from an empty `PathBuf` produced `notes.txt/` - a directory
        // name. `fs::write` then failed with ENOENT and the `edit` tool could
        // not create a file at all, which is half of what it is for.
        suffix = if suffix.as_os_str().is_empty() {
            PathBuf::from(name)
        } else {
            PathBuf::from(name).join(&suffix)
        };

        if !current.pop() {
            return Err(format!(
                "chat: path `{}` has no existing ancestor",
                path.to_string_lossy()
            ));
        }
    }
}
/// The directories a tool may touch.
///
/// The project root, not just the current directory: in a workspace, running
/// `!` from `dsh-builtin/` used to put the top-level `Cargo.toml` and every
/// sibling crate out of reach, with an error message that offered no way
/// around it. The root is the nearest ancestor carrying a project marker, so
/// this widens to the repository and stops there rather than at `$HOME`.
fn allowed_tool_roots(current_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![canonicalize_or_normalize(current_dir)];
    let workspace_root = canonicalize_or_normalize(&workspace_root(current_dir));
    if !roots.contains(&workspace_root) {
        roots.push(workspace_root);
    }
    roots.push(canonicalize_or_normalize(&tool_skills_dir()));
    roots
}
/// The outermost project enclosing `current_dir`.
///
/// `find_project_root` stops at the *nearest* marker, which for a workspace
/// member is the member itself - so running `!` in `dsh-builtin/` still could
/// not see the workspace `Cargo.toml` one level up. Walking while each parent
/// is also a project extends the reach to the repository and stops there: the
/// chain breaks at the first directory that is not a project, so an unrelated
/// parent never becomes readable.
///
/// Never expands to the home directory itself, which a dotfiles repository
/// would otherwise qualify by way of its `.git`.
pub(crate) fn workspace_root(current_dir: &Path) -> PathBuf {
    let current_dir = canonicalize_or_normalize(current_dir);
    let home = dirs::home_dir().map(|home| canonicalize_or_normalize(&home));
    let too_far = |candidate: &Path| home.as_deref().is_some_and(|home| candidate == home);

    let mut root =
        canonicalize_or_normalize(&crate::project_context::find_project_root(&current_dir));

    // `find_project_root` walks ancestors, so with a dotfiles repository in
    // `$HOME` it answers `$HOME` for any directory that is not itself a
    // project - which would have put `~/.ssh` and `~/.aws` inside the sandbox.
    // Checking the starting point, not only the climb, is what stops that.
    if too_far(&root) {
        return current_dir;
    }

    while let Some(parent) = root.parent() {
        if too_far(parent) || !crate::project_context::has_project_marker(parent) {
            break;
        }
        root = parent.to_path_buf();
    }

    root
}
pub(crate) fn is_path_within_tool_roots(path: &Path, current_dir: &Path) -> bool {
    let roots = allowed_tool_roots(current_dir);
    roots.iter().any(|root| path.starts_with(root))
}
pub(crate) fn resolve_tool_path(
    path_str: &str,
    proxy: &mut dyn ChatToolHost,
) -> Result<std::path::PathBuf, String> {
    // Use shellexpand to handle ~
    let expanded = shellexpand::full(path_str)
        .map_err(|e| format!("chat: failed to expand path `{path_str}`: {e}"))?;
    let path = Path::new(expanded.as_ref());
    let current_dir = proxy
        .get_current_dir()
        .map_err(|err| format!("chat: failed to get current working directory: {err}"))?;

    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        current_dir.join(path)
    };
    let resolved_path = if absolute_path.exists() {
        std::fs::canonicalize(&absolute_path).map_err(|err| {
            format!(
                "chat: failed to canonicalize path `{}`: {err}",
                absolute_path.display()
            )
        })?
    } else {
        resolve_with_existing_ancestor(&absolute_path)?
    };

    if let Some(runtime) = proxy.agent_runtime() {
        let runtime = runtime.lock();
        let grant = &runtime.task.grant;
        let state = crate::config_paths::agent_state_dir();
        if resolved_path.starts_with(&state)
            || crate::safety_policy::is_sensitive_path(&resolved_path)
        {
            return Err("agent: protected path cannot be read through task tools".into());
        }
        if grant
            .read_roots
            .iter()
            .chain(&grant.write_roots)
            .any(|root| resolved_path.starts_with(root))
            || resolved_path.starts_with(crate::config_paths::skills_dir())
        {
            return Ok(resolved_path);
        }
        return Err(
            "agent: path is outside task grants; resume with an explicit --read or --write grant"
                .into(),
        );
    }
    if is_path_within_tool_roots(&resolved_path, &current_dir) {
        return Ok(resolved_path);
    }

    Err(format!(
        "chat: path `{path_str}` resolves outside allowed directories"
    ))
}

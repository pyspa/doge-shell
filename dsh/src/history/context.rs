//! Context detection for history entries.
//!
//! Provides functions to determine the current context (e.g., git repository root)
//! for context-aware history features.

use std::path::Path;

/// Get the current context for history entries.
///
/// Pure filesystem lookup: the nearest `.git` marker above the process cwd,
/// falling back to the cwd itself. Never spawns `git`, so history
/// persistence never depends on which `git` binary the runtime resolves.
pub fn get_current_context() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    get_current_context_from(&cwd)
}

/// Context for an explicit directory: the enclosing repository root when a
/// `.git` directory or `.git` file (worktree/submodule) marks one,
/// otherwise the directory itself.
///
/// Shared marker walking with the prompt (`crate::git_context`) without
/// depending on `Prompt`: history must not observe prompt state.
pub fn get_current_context_from(cwd: &Path) -> Option<String> {
    context_with_marker_lookup(cwd, crate::git_context::find_marker_root)
}

fn context_with_marker_lookup(
    cwd: &Path,
    find_root: impl FnOnce(&Path) -> Option<std::path::PathBuf>,
) -> Option<String> {
    if let Some(root) = find_root(cwd) {
        return Some(root.to_string_lossy().into_owned());
    }
    Some(cwd.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_dir_resolves_to_repo_root() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let nested = repo.join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(
            get_current_context_from(&nested),
            Some(repo.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn git_file_counts_as_root_marker() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(
            get_current_context_from(&repo),
            Some(repo.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn without_marker_falls_back_to_cwd() {
        let root = tempfile::tempdir().unwrap();
        // No `git` binary involved: even with an empty PATH this answers.
        assert_eq!(
            context_with_marker_lookup(root.path(), |_| None),
            Some(root.path().to_string_lossy().into_owned())
        );
    }
    #[test]
    fn ancestor_marker_wins_over_cwd() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        let child = root.path().join("unmarked/child");
        std::fs::create_dir_all(&child).unwrap();
        assert_eq!(
            get_current_context_from(&child),
            Some(root.path().to_string_lossy().into_owned())
        );
    }
}

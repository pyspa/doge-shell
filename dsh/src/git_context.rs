//! Pure `.git` marker walking shared by prompt and history lookups.
//!
//! Both the prompt's git-root detection and the history context lookup need
//! the same answer — "which repository owns this directory?" — without
//! spawning a subprocess. A `.git` directory marks a repository root; a
//! `.git` file marks a worktree/submodule pointer. Anything fancier (the
//! `git rev-parse` fallback) stays with the caller that owns a runtime
//! snapshot, so history persistence never depends on which `git` binary the
//! shell resolves and never touches `Prompt`.

use std::path::{Path, PathBuf};

/// Walk from `cwd` toward the filesystem root for a `.git` marker.
///
/// Returns the nearest enclosing directory whose `.git` entry marks a
/// repository: a `.git` directory, or a `.git` file (worktree/submodule
/// pointer). Pure filesystem lookup — no subprocess, no environment reads.
pub fn find_marker_root(cwd: &Path) -> Option<PathBuf> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        let marker = dir.join(".git");
        if marker.is_dir() {
            return Some(dir.to_path_buf());
        }
        if marker.is_file() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_dir_resolves_to_repo_root() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let nested = repo.join("a").join("b");
        std::fs::create_dir_all(nested.clone()).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(find_marker_root(&nested), Some(repo));
    }

    #[test]
    fn git_file_marks_worktree_root() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: /elsewhere/worktrees/repo\n").unwrap();
        assert_eq!(find_marker_root(&repo.join("sub")), Some(repo));
    }

    #[test]
    fn no_marker_yields_none() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(find_marker_root(root.path()), None);
    }
}

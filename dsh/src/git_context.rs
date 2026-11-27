//! Pure `.git` marker walking shared by prompt and history lookups.
//!
//! Both the prompt's git-root detection and the history context lookup need
//! the same answer — "which repository owns this directory?" — without
//! spawning a subprocess. A `.git` directory marks a repository root; a
//! `.git` file marks a worktree/submodule pointer. Anything fancier (the
//! `git rev-parse` fallback) stays with the caller that owns a runtime
//! snapshot, so history persistence never depends on which `git` binary the
//! shell resolves and never touches `Prompt`.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

// Same limit as Git's read_gitfile_raw.
const MAX_GITFILE_SIZE: u64 = 1 << 20;

/// Walk from `cwd` toward the filesystem root for a `.git` marker.
///
/// Returns the nearest enclosing directory whose `.git` entry marks a
/// repository: a `.git` directory, or a `.git` file (worktree/submodule
/// pointer). Pure filesystem lookup — no subprocess, no environment reads.
pub fn find_marker_root(cwd: &Path) -> Option<PathBuf> {
    find_marker_root_with(cwd, |dir| git_dir_for_worktree(dir).is_some())
}

/// Resolve a worktree's metadata directory, including relative gitfile pointers.
/// An empty or dangling marker has no HEAD and must not capture its descendants.
pub(crate) fn git_dir_for_worktree(root: &Path) -> Option<PathBuf> {
    let marker = root.join(".git");
    let metadata = marker.metadata().ok()?;
    let git_dir = if metadata.is_dir() {
        marker
    } else if metadata.is_file() && metadata.len() <= MAX_GITFILE_SIZE {
        let mut contents = Vec::new();
        std::fs::File::open(marker)
            .ok()?
            .take(MAX_GITFILE_SIZE + 1)
            .read_to_end(&mut contents)
            .ok()?;
        if contents.len() as u64 > MAX_GITFILE_SIZE {
            return None;
        }
        let path = contents.strip_prefix(b"gitdir: ")?;
        let end = path.iter().rposition(|b| !matches!(b, b'\r' | b'\n'))?;
        root.join(OsStr::from_bytes(&path[..=end]))
    } else {
        return None;
    };
    git_dir.join("HEAD").is_file().then_some(git_dir)
}

fn find_marker_root_with(cwd: &Path, mut has_marker: impl FnMut(&Path) -> bool) -> Option<PathBuf> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        if has_marker(dir) {
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
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let repo = root.path().join("repo");
        let nested = repo.join("a").join("b");
        std::fs::create_dir_all(nested.clone()).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(find_marker_root(&nested), Some(repo));
    }

    #[test]
    fn git_file_marks_worktree_root() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir(repo.join("metadata")).unwrap();
        std::fs::write(repo.join("metadata/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(repo.join(".git"), "gitdir: metadata\r\n\r\n").unwrap();
        assert_eq!(find_marker_root(&repo.join("sub")), Some(repo));
    }

    #[test]
    fn oversized_gitfile_is_skipped_in_favor_of_parent() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let child = root.path().join("child");
        std::fs::create_dir(&child).unwrap();
        std::fs::File::create(child.join(".git"))
            .unwrap()
            .set_len(MAX_GITFILE_SIZE + 1)
            .unwrap();
        assert_eq!(find_marker_root(&child).as_deref(), Some(root.path()));
    }

    #[test]
    fn no_marker_yields_none() {
        let root = tempfile::tempdir().unwrap();
        // A real tempdir's ancestors may be repositories. Control the marker
        // predicate while checking that the walk still visits every ancestor.
        let mut visited = Vec::new();
        assert_eq!(
            find_marker_root_with(root.path(), |dir| {
                visited.push(dir.to_path_buf());
                false
            }),
            None
        );
        assert_eq!(
            visited,
            root.path()
                .ancestors()
                .map(Path::to_path_buf)
                .collect::<Vec<_>>()
        );
    }
}

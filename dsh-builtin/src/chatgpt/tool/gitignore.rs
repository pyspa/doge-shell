use ignore::gitignore::GitignoreBuilder;
use std::path::Path;

/// Whether `path` is ignored, and so must not be read or written by a tool.
///
/// Git consults every `.gitignore` between the repository root and the file,
/// plus `.git/info/exclude` and the user's global excludes, with the closest
/// file winning. This used to read a single `.gitignore` in `base_dir`, which
/// made the rule disagree with the `search` tool - that one walks with
/// `ignore::WalkBuilder` and honours the full set. The same path could be
/// invisible to `search` and readable by `read_file`, or the reverse.
pub fn is_gitignored(path: &Path, base_dir: &Path) -> Result<bool, String> {
    let is_dir = path.is_dir();

    // Innermost first: the closest .gitignore wins, and a `!pattern` there is
    // allowed to override an ignore further up.
    for directory in ignore_file_directories(path, base_dir) {
        let gitignore_path = directory.join(".gitignore");
        if !gitignore_path.is_file() {
            continue;
        }

        match matches_ignore_file(&directory, &gitignore_path, path, is_dir)? {
            Some(ignored) => return Ok(ignored),
            None => continue,
        }
    }

    if let Some(repo_root) = repository_root(base_dir) {
        let exclude = repo_root.join(".git").join("info").join("exclude");
        if exclude.is_file()
            && let Some(ignored) = matches_ignore_file(&repo_root, &exclude, path, is_dir)?
        {
            return Ok(ignored);
        }
    }

    let (global, err) = ignore::gitignore::Gitignore::global();
    if err.is_none()
        && let Some(ignored) = matched_under_root(&global, path, is_dir)
    {
        return Ok(ignored);
    }

    Ok(false)
}

/// The directories whose `.gitignore` applies to `path`, innermost first.
///
/// Every directory from the file up to the enclosing repository (or `base_dir`
/// when there is none), inclusive. An earlier version stopped at `base_dir`
/// and then jumped straight to the root, which skipped the levels in between -
/// so a file ignored only by `/repo/a/.gitignore` stayed readable while
/// `search` hid it, exactly the disagreement this is here to prevent.
fn ignore_file_directories(path: &Path, base_dir: &Path) -> Vec<std::path::PathBuf> {
    let ceiling = repository_root(base_dir).unwrap_or_else(|| base_dir.to_path_buf());

    let start = if path.is_dir() {
        Some(path)
    } else {
        path.parent()
    };

    // Only the part of the chain that lies inside the ceiling. A path outside
    // it - the runtime skills directory, say - has no repository above it, and
    // walking to `/` from there would collect unrelated ignore files.
    let mut directories = Vec::new();
    let mut current = start;
    while let Some(directory) = current {
        if !directory.starts_with(&ceiling) {
            break;
        }
        directories.push(directory.to_path_buf());
        if directory == ceiling {
            break;
        }
        current = directory.parent();
    }

    directories
}

fn matches_ignore_file(
    root: &Path,
    ignore_file: &Path,
    path: &Path,
    is_dir: bool,
) -> Result<Option<bool>, String> {
    if !path.starts_with(root) {
        return Ok(None);
    }

    let mut builder = GitignoreBuilder::new(root);
    if let Some(err) = builder.add(ignore_file) {
        return Err(format!("failed to read {}: {err}", ignore_file.display()));
    }

    let gitignore = builder.build().map_err(|err| {
        format!(
            "failed to build .gitignore matcher for {}: {err}",
            ignore_file.display()
        )
    })?;

    Ok(matched_under_root(&gitignore, path, is_dir))
}

/// Ask a matcher about `path`, but only when `path` is under its root.
///
/// `matched_path_or_any_parents` *asserts* that, and panics otherwise. The
/// global matcher is rooted at the process working directory, so any tool path
/// outside it - which the widened sandbox and the skills directory make
/// routine - crashed the shell for every user who has a global gitignore.
fn matched_under_root(
    gitignore: &ignore::gitignore::Gitignore,
    path: &Path,
    is_dir: bool,
) -> Option<bool> {
    if !path.starts_with(gitignore.path()) {
        return None;
    }
    decide(gitignore.matched_path_or_any_parents(path, is_dir))
}

/// `None` means "this file had nothing to say"; the next one gets a turn.
fn decide(matched: ignore::Match<&ignore::gitignore::Glob>) -> Option<bool> {
    match matched {
        ignore::Match::Ignore(_) => Some(true),
        ignore::Match::Whitelist(_) => Some(false),
        ignore::Match::None => None,
    }
}

/// The repository containing `start`, if there is one.
fn repository_root(start: &Path) -> Option<std::path::PathBuf> {
    start
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_no_gitignore_allows_all() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "content").unwrap();

        assert!(!is_gitignored(&file_path, dir.path()).unwrap());
    }

    #[test]
    fn test_gitignore_blocks_matching_file() {
        let dir = tempdir().unwrap();
        let gitignore_path = dir.path().join(".gitignore");
        fs::write(&gitignore_path, "*.secret\n.env\n").unwrap();

        let secret_file = dir.path().join("password.secret");
        fs::write(&secret_file, "secret").unwrap();

        let env_file = dir.path().join(".env");
        fs::write(&env_file, "SECRET=value").unwrap();

        let normal_file = dir.path().join("normal.txt");
        fs::write(&normal_file, "normal").unwrap();

        assert!(is_gitignored(&secret_file, dir.path()).unwrap());
        assert!(is_gitignored(&env_file, dir.path()).unwrap());
        assert!(!is_gitignored(&normal_file, dir.path()).unwrap());
    }

    #[test]
    fn test_gitignore_directory_pattern() {
        let dir = tempdir().unwrap();
        let gitignore_path = dir.path().join(".gitignore");
        fs::write(&gitignore_path, "node_modules/\n").unwrap();

        let node_modules = dir.path().join("node_modules");
        fs::create_dir(&node_modules).unwrap();

        let file_in_node_modules = node_modules.join("package.json");
        fs::write(&file_in_node_modules, "{}").unwrap();

        assert!(is_gitignored(&node_modules, dir.path()).unwrap());
        assert!(is_gitignored(&file_in_node_modules, dir.path()).unwrap());
    }

    /// A nested `.gitignore` used to be invisible here while `search` honoured
    /// it, so the same path was hidden from one tool and readable by another.
    #[test]
    fn a_nested_gitignore_applies() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        let nested = dir.path().join("crate");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join(".gitignore"), "generated.rs\n").unwrap();

        let generated = nested.join("generated.rs");
        fs::write(&generated, "// generated").unwrap();
        let kept = nested.join("main.rs");
        fs::write(&kept, "fn main() {}").unwrap();

        assert!(is_gitignored(&generated, dir.path()).unwrap());
        assert!(!is_gitignored(&kept, dir.path()).unwrap());
    }

    /// The closest file wins, so a nested un-ignore overrides the root.
    #[test]
    fn a_nested_negation_overrides_an_outer_ignore() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();

        let nested = dir.path().join("keep");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join(".gitignore"), "!important.log\n").unwrap();

        let important = nested.join("important.log");
        fs::write(&important, "keep me").unwrap();
        let ordinary = dir.path().join("other.log");
        fs::write(&ordinary, "noise").unwrap();

        assert!(!is_gitignored(&important, dir.path()).unwrap());
        assert!(is_gitignored(&ordinary, dir.path()).unwrap());
    }

    /// The levels between the working directory and the repository root have
    /// to be consulted too, or `search` and `read_file` disagree about the
    /// same file - the exact split this rewrite exists to close.
    #[test]
    fn an_intermediate_gitignore_between_cwd_and_the_repo_root_applies() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        let middle = dir.path().join("a");
        let deep = middle.join("b");
        fs::create_dir_all(&deep).unwrap();
        fs::write(middle.join(".gitignore"), "*.log\n").unwrap();

        let ignored = deep.join("run.log");
        fs::write(&ignored, "noise").unwrap();

        assert!(is_gitignored(&ignored, &deep).unwrap());
    }

    /// A path outside the repository must not panic, and must not pick up
    /// ignore files belonging to unrelated directories above it.
    #[test]
    fn a_path_outside_the_repository_is_answered_not_crashed() {
        let repo = tempdir().unwrap();
        fs::create_dir(repo.path().join(".git")).unwrap();
        fs::write(repo.path().join(".gitignore"), "*.md\n").unwrap();

        let elsewhere = tempdir().unwrap();
        let outside = elsewhere.path().join("SKILL.md");
        fs::write(&outside, "skill").unwrap();

        assert!(!is_gitignored(&outside, repo.path()).unwrap());
    }

    #[test]
    fn git_info_exclude_is_honoured() {
        let dir = tempdir().unwrap();
        let info = dir.path().join(".git").join("info");
        fs::create_dir_all(&info).unwrap();
        fs::write(info.join("exclude"), "scratch.txt\n").unwrap();

        let excluded = dir.path().join("scratch.txt");
        fs::write(&excluded, "notes").unwrap();

        assert!(is_gitignored(&excluded, dir.path()).unwrap());
    }
}

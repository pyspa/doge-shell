//! Shared filesystem/PATH helpers used by more than one `doctor` section.
use crate::ShellProxy;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn count_extra_skill_dirs(root: &Path, expected_skills: &[&str]) -> usize {
    let expected = expected_skills.iter().copied().collect::<BTreeSet<_>>();
    fs::read_dir(root)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    let path = entry.path();
                    path.is_dir()
                        && path.join("SKILL.md").is_file()
                        && entry
                            .file_name()
                            .to_str()
                            .is_some_and(|name| !expected.contains(name))
                })
                .count()
        })
        .unwrap_or(0)
}

pub(super) fn skill_dirs_match(source: &Path, dest: &Path) -> bool {
    let Ok(source_files) = relative_files(source) else {
        return false;
    };
    let Ok(dest_files) = relative_files(dest) else {
        return false;
    };
    if source_files != dest_files {
        return false;
    }

    source_files.into_iter().all(|relative| {
        let source_path = source.join(&relative);
        let dest_path = dest.join(&relative);
        match (fs::read(source_path), fs::read(dest_path)) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
    })
}

pub(super) fn relative_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, files)?;
            } else if path.is_file()
                && let Ok(relative) = path.strip_prefix(root)
            {
                files.push(relative.to_path_buf());
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

pub(super) fn mask_secret(value: Option<String>) -> String {
    match value {
        Some(secret) if !secret.is_empty() => {
            let visible = secret.chars().rev().take(4).collect::<String>();
            let suffix = visible.chars().rev().collect::<String>();
            format!("***{}", suffix)
        }
        _ => "missing".to_string(),
    }
}

pub(super) fn read_version(command: &str) -> Option<String> {
    let args = match command {
        "go" => vec!["version"],
        _ => vec!["--version"],
    };
    let output = Command::new(command).args(args).output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    stdout
        .lines()
        .chain(stderr.lines())
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
}

pub(super) fn resolve_in_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(command);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

pub(super) fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(path) {
            return metadata.permissions().mode() & 0o111 != 0;
        }
        false
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub(super) fn count_skill_dirs(root: &Path) -> usize {
    fs::read_dir(root)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    let path = entry.path();
                    path.is_dir() && path.join("SKILL.md").is_file()
                })
                .count()
        })
        .unwrap_or(0)
}

/// Where the Codex runtime skills live.
///
/// `CODEX_HOME` moves the whole Codex directory, so the `--json` path used to
/// report a directory nobody was using once it was set.
pub(super) fn codex_runtime_skills_dir(proxy: &mut dyn ShellProxy) -> Option<PathBuf> {
    proxy
        .get_var("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|path| path.join(".codex")))
        .map(|path| path.join("skills"))
}

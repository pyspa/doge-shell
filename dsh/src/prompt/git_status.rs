//! Fetching and parsing `git status --porcelain=2 --branch` outside of `Prompt` itself, so a background task can call the async half without borrowing the prompt.
use super::*;

// Standalone functions (kept for async task compatibility)

/// Synchronous git status fetch for accurate display after command execution.
/// This blocks but ensures the prompt shows the correct state immediately.
pub fn fetch_git_status_sync(path: &Path) -> Option<GitStatus> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("--no-optional-locks")
        .arg("status")
        .arg("--porcelain=2")
        .arg("--branch")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    parse_git_status_output(&output.stdout)
}

pub async fn fetch_git_status_async(path: &Path) -> Option<GitStatus> {
    use tokio::process::Command;

    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("--no-optional-locks")
        .arg("status")
        .arg("--porcelain=2")
        .arg("--branch")
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    parse_git_status_output(&output.stdout)
}

pub(crate) fn parse_git_status_output(stdout: &[u8]) -> Option<GitStatus> {
    let mut status = GitStatus::new();
    let mut reader = BufReader::new(stdout);
    let mut buf = String::new();

    while let Ok(size) = reader.read_line(&mut buf) {
        if size == 0 {
            break;
        }

        let line = buf.trim();
        if list_stats(line, &mut status) {
            // Continued
        }

        buf.clear();
    }

    Some(status)
}

fn list_stats(line: &str, status: &mut GitStatus) -> bool {
    if line.starts_with('#') {
        let splited: Vec<&str> = line.split_whitespace().collect();
        if line.starts_with("# branch.oid") {
            if let Some(oid) = splited.get(2) {
                status.oid = Some(oid.to_string());
            }
        } else if line.starts_with("# branch.head") {
            if let Some(branch) = splited.get(2) {
                status.branch = branch.to_string();
            }
        } else if line.starts_with("# branch.ab") {
            if let Some(val) = splited.get(2)
                && let Ok(count) = val.replace('+', "").parse::<u32>()
            {
                status.ahead = count;
            }
            if let Some(val) = splited.get(3)
                && let Ok(count) = val.replace('-', "").parse::<u32>()
            {
                status.behind = count;
            }
        }
        return true;
    }

    // porcelain=2 format
    // 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
    // 2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path> <origPath>
    // u <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
    // ? <path>

    if line.starts_with('?') {
        status.untracked += 1;
        return true;
    }

    if line.starts_with('u') {
        status.conflicted += 1;
        return true;
    }

    if line.starts_with('1') || line.starts_with('2') {
        let mut chars = line.chars();
        // Skip '1' or '2' and space
        if let (Some(_), Some(_)) = (chars.next(), chars.next()) {
            // XY
            let x = chars.next().unwrap_or('.');
            let y = chars.next().unwrap_or('.');

            // X = Index (Staged)
            // Y = Worktree (Modified)

            if x == 'R' {
                status.renamed += 1;
            }

            if x == 'D' || y == 'D' {
                status.deleted += 1;
            }

            // Staged: X is not '.' and not '?' (untracked handled elsewhere)
            // If X is 'D', it's staged deletion, we count it as deleted, maybe also staged?
            // Starship counts staged separately.
            // Let's count any index change as staged, except '?'
            if x != '.' && x != '?' {
                status.staged += 1;
            }

            // Modified: Y is 'M'
            if y == 'M' {
                status.modified += 1;
            }
        }
        return true;
    }

    false
}

pub async fn find_git_root_async(cwd: PathBuf) -> Option<PathBuf> {
    tokio::task::spawn_blocking(move || find_git_root(&cwd).map(PathBuf::from))
        .await
        .unwrap_or(None)
}

fn find_git_root(cwd: &Path) -> Option<String> {
    let mut p = cwd;
    loop {
        let git_dir = p.join(".git");
        if git_dir.exists() {
            if git_dir.is_dir() {
                return Some(p.to_string_lossy().into_owned());
            } else if git_dir.is_file() {
                if let Ok(content) = std::fs::read_to_string(&git_dir)
                    && content.trim().starts_with("gitdir:")
                {
                    return Some(p.to_string_lossy().into_owned());
                }
                break;
            }
        }
        {
            let parent = p.parent()?;
            p = parent;
        }
    }

    let result = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(cwd)
        .output();

    if let Ok(output) = result
        && output.status.success()
        && let Ok(out) = String::from_utf8(output.stdout)
    {
        return Some(out.trim().to_string());
    }
    None
}

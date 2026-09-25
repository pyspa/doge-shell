//! Fetching and parsing `git status --porcelain=2 --branch` outside of `Prompt` itself, so a background task can call the async half without borrowing the prompt.
use super::runtime::PromptRuntimeSnapshot;
use super::*;
use std::sync::Arc;

// Standalone functions (kept for async task compatibility)

/// Synchronous git status fetch for accurate display after command execution.
///
/// Resolves `git` through the refresh tick's logical runtime snapshot and
/// spawns it with the snapshot's exported child environment: a shell-level
/// `PATH` change or `unset` cannot leak the process-global `git` in.
///
/// Test-only: the interactive loop uses the async half so the prompt never
/// blocks on a subprocess.
#[cfg(test)]
pub(crate) fn fetch_git_status_sync(
    runtime: &PromptRuntimeSnapshot,
    path: &Path,
) -> Option<GitStatus> {
    let output = runtime
        .command_runtime()
        .std_command("git")?
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

/// Async git status fetch sharing one refresh tick's runtime snapshot.
///
/// The caller holds the same `Arc<PromptRuntimeSnapshot>` across the root
/// lookup and this fetch, so `PATH`, child environment, and cwd cannot be
/// re-read mid-operation.
pub(crate) async fn fetch_git_status_async(
    runtime: &PromptRuntimeSnapshot,
    path: &Path,
) -> Option<GitStatus> {
    let output = runtime
        .command("git")?
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

/// Async git-root lookup bound to one refresh tick's runtime snapshot.
///
/// The pure `.git` marker walk comes first (shared with history context via
/// `crate::git_context`, no subprocess involved); only when no marker is
/// found does the logical-runtime `git rev-parse` fallback run. Both halves
/// read the same immutable snapshot, and the caller publishes the result
/// under the tick's probe epoch so a stale task never overwrites a newer
/// runtime.
pub(crate) async fn find_git_root_async(runtime: Arc<PromptRuntimeSnapshot>) -> Option<PathBuf> {
    tokio::task::spawn_blocking(move || find_git_root(&runtime).map(PathBuf::from))
        .await
        .unwrap_or(None)
}

fn find_git_root(runtime: &PromptRuntimeSnapshot) -> Option<String> {
    let cwd = runtime.current_dir();
    if let Some(root) = crate::git_context::find_marker_root(cwd) {
        return Some(root.to_string_lossy().into_owned());
    }

    let result = runtime
        .command_runtime()
        .std_command("git")?
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

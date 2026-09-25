use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::process::Stdio;
use tracing::debug;

/// Built-in gco command description
pub fn description() -> &'static str {
    "Checkout git branches with fzf selection"
}

// Define a simple wrapper since String doesn't impl SkimItem in skim 2.0
struct StringItem(String);

impl SkimItem for StringItem {
    fn text(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed(&self.0)
    }

    fn output(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Borrowed(&self.0)
    }
}

pub fn command(ctx: &Context, _argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Check if we're in a git repository
    if !is_git_repository(proxy) {
        ctx.write_stderr("gco: not a git repository").ok();
        return ExitStatus::ExitedWith(1);
    }

    // Get git log entries
    let branch_entries = match get_git_branches(proxy) {
        Ok(entries) => entries,
        Err(err) => {
            ctx.write_stderr(&format!("gco: failed to get git log: {err}"))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    debug!("get git branches {:?}", branch_entries);
    if branch_entries.is_empty() {
        ctx.write_stderr("gco: no branch found").ok();
        return ExitStatus::ExitedWith(1);
    }

    // Check if there's only one candidate - if so, checkout directly without showing skim interface
    if branch_entries.len() == 1 {
        let branch = extract_branch_name(&branch_entries[0]);
        debug!(
            "gco: only one branch candidate, checking out directly: {}",
            branch
        );
        let args = vec!["checkout", &branch];
        match checkout_branch(proxy, &args) {
            Ok(output) => {
                if !output.status.success() {
                    let error = String::from_utf8_lossy(&output.stderr);
                    let err = error.trim().to_string();
                    ctx.write_stderr(&format!("gco: failed to checkout branch: {err}"))
                        .ok();
                    return ExitStatus::ExitedWith(1);
                } else {
                    let output = String::from_utf8_lossy(&output.stdout);
                    let output = output.trim().to_string();
                    ctx.write_stdout(&output.to_string()).ok();
                }
            }
            Err(err) => {
                let err = err.to_string();
                ctx.write_stderr(&format!("gco: failed to checkout branch: {err}"))
                    .ok();
                return ExitStatus::ExitedWith(1);
            }
        }
        return ExitStatus::ExitedWith(0);
    }

    let selected = std::thread::spawn(move || {
        let options = SkimOptionsBuilder::default()
            .bind(vec!["Enter:accept".to_string()])
            .preview("git log --oneline --graph --color=always -n 20 {}".to_string())
            // .preview_window("right:60%") // Disabled until PreviewLayout is known
            .build();

        let options = match options {
            Ok(o) => o,
            Err(_) => return Vec::new(),
        };

        let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
        for branch in branch_entries {
            let item = Arc::new(StringItem(branch));
            let _ = tx_item.send(vec![item]);
        }
        drop(tx_item);

        Skim::run_with(options, Some(rx_item))
            .ok()
            .map(|out| {
                if out.is_abort {
                    Vec::new()
                } else {
                    out.selected_items
                }
            })
            .unwrap_or_default()
    })
    .join()
    .unwrap_or_default();

    if !selected.is_empty() {
        let val = selected[0].output().to_string();
        let val = extract_branch_name(&val);
        debug!("selected branch {:?}", val);
        let args = vec!["checkout", &val];
        match checkout_branch(proxy, &args) {
            Ok(output) => {
                if !output.status.success() {
                    let error = String::from_utf8_lossy(&output.stderr);
                    let err = error.trim().to_string();
                    ctx.write_stderr(&format!("gco: failed to get git log: {err}"))
                        .ok();
                    return ExitStatus::ExitedWith(1);
                } else {
                    let output = String::from_utf8_lossy(&output.stdout);
                    let output = output.trim().to_string();
                    ctx.write_stdout(&output.to_string()).ok();
                }
            }
            Err(err) => {
                let err = err.to_string();
                ctx.write_stderr(&format!("gco: failed to get git log: {err}"))
                    .ok();
                return ExitStatus::ExitedWith(1);
            }
        }
    }

    ExitStatus::ExitedWith(0)
}

/// Check if current directory is within a git repository
fn is_git_repository(proxy: &mut dyn ShellProxy) -> bool {
    crate::runtime_spawn::runtime_command(proxy, "git")
        .and_then(|mut command| {
            command
                .args(["rev-parse", "--git-dir"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| anyhow::anyhow!("{e}"))
        })
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Run `git checkout` through the logical runtime: the same `git` the
/// shell would run, with exactly the exported child environment.
fn checkout_branch(
    proxy: &mut dyn ShellProxy,
    args: &[&str],
) -> std::io::Result<std::process::Output> {
    let mut command = crate::runtime_spawn::runtime_command(proxy, "git")
        .map_err(|e| std::io::Error::other(format!("{e}")))?;
    command.args(args).output()
}

/// Get formatted git log entries
fn get_git_branches(proxy: &mut dyn ShellProxy) -> Result<Vec<String>, String> {
    let args = vec!["branch", "--all"];

    let output = crate::runtime_spawn::runtime_command(proxy, "git")
        .map_err(|e| format!("failed to execute git: {e}"))?
        .args(&args)
        .output()
        .map_err(|e| format!("failed to execute git: {e}"))?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(error.trim().to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let entries: Vec<String> = stdout
        .lines()
        .map(|line| line.replace("*", "").trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();

    Ok(entries)
}

fn extract_branch_name(branch: &str) -> String {
    let branch = branch.trim();

    // Handle "remotes/origin/HEAD -> origin/main" case
    if let Some(arrow_idx) = branch.find(" -> ") {
        // Use the part after " -> " which is usually "origin/main"
        let target = &branch[arrow_idx + 4..];
        // Now parse "origin/main" to "main"
        if let Some((_remote, local)) = target.split_once('/') {
            return local.to_string();
        }
        return target.to_string();
    }

    if let Some(stripped) = branch.strip_prefix("remotes/") {
        let parts: Vec<&str> = stripped.splitn(2, '/').collect();
        if parts.len() == 2 {
            return parts[1].to_string();
        }
    }
    branch.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_branch_name() {
        assert_eq!(extract_branch_name("master"), "master");
        assert_eq!(extract_branch_name("  master  "), "master");
        assert_eq!(extract_branch_name("remotes/origin/master"), "master");
        assert_eq!(
            extract_branch_name("remotes/origin/feature/foo"),
            "feature/foo"
        );
        assert_eq!(extract_branch_name("remotes/upstream/v1.0"), "v1.0");
        assert_eq!(extract_branch_name("feature/bar"), "feature/bar");

        // Symref handling
        assert_eq!(
            extract_branch_name("remotes/origin/HEAD -> origin/main"),
            "main"
        );
        assert_eq!(
            extract_branch_name("remotes/origin/HEAD -> origin/feature/bar"),
            "feature/bar"
        );
    }
}

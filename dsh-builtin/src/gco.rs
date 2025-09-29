use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::process::{Command, Stdio};
use tracing::debug;

/// Built-in gco command description
pub fn description() -> &'static str {
    "Checkout git branches with fzf selection"
}

pub fn command(ctx: &Context, _argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Check if we're in a git repository
    if !is_git_repository() {
        ctx.write_stderr("gco: not a git repository").ok();
        return ExitStatus::ExitedWith(1);
    }

    // Get git log entries
    let branch_entries = match get_git_branches() {
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
        let branch = &branch_entries[0];
        debug!(
            "gco: only one branch candidate, checking out directly: {}",
            branch
        );
        let args = vec!["checkout", branch];
        match Command::new("git").args(&args).output() {
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

    let options = SkimOptionsBuilder::default()
        .select_1(true)
        .bind(vec!["Enter:accept".to_string()])
        .build()
        .unwrap();

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
    for branch in branch_entries {
        let _ = tx_item.send(Arc::new(branch));
    }
    drop(tx_item);

    let selected = Skim::run_with(&options, Some(rx_item))
        .map(|out| match out.final_key {
            Key::Enter => out.selected_items,
            _ => Vec::new(),
        })
        .unwrap_or_default();

    if !selected.is_empty() {
        let val = selected[0].output().to_string();
        debug!("selected branch {:?}", val);
        let args = vec!["checkout", &val];
        match Command::new("git").args(&args).output() {
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
fn is_git_repository() -> bool {
    Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Get formatted git log entries
fn get_git_branches() -> Result<Vec<String>, String> {
    let args = vec!["branch", "--all"];

    let output = Command::new("git")
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

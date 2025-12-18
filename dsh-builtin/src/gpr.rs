//! GitHub PR checkout command
//!
//! Provides interactive PR selection and checkout using gh CLI + skim.

use super::ShellProxy;
use crate::github_client;
use dsh_types::{Context, ExitStatus};
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use tracing::debug;

/// Built-in gpr command description
pub fn description() -> &'static str {
    "Checkout GitHub Pull Request with fzf selection"
}

pub fn command(ctx: &Context, _argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Check if we're in a git repository
    if !is_git_repository() {
        ctx.write_stderr("gpr: not a git repository").ok();
        return ExitStatus::ExitedWith(1);
    }

    // Check if gh is installed
    if !github_client::is_gh_installed() {
        ctx.write_stderr("gpr: gh command not found").ok();
        return ExitStatus::ExitedWith(1);
    }

    // Get PRs
    let mut prs = match github_client::get_prs() {
        Ok(entries) => entries,
        Err(err) => {
            ctx.write_stderr(&format!("gpr: failed to get PRs: {err}"))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    if prs.is_empty() {
        ctx.write_stderr("gpr: no PR found").ok();
        return ExitStatus::ExitedWith(1);
    }

    // Skim options
    let options = SkimOptionsBuilder::default()
        .height("50%".to_string())
        .multi(false)
        .preview(Some("".to_string())) // No preview text, but window might show up? set empty function
        .preview_window("".to_string()) // Disable preview window
        .bind(vec!["Enter:accept".to_string()])
        .build()
        .unwrap();

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();

    // Sort PRs by number descending (usually newer first)
    prs.sort_by(|a, b| b.number.cmp(&a.number));

    for pr in prs {
        let _ = tx_item.send(Arc::new(pr));
    }
    drop(tx_item);

    let selected = Skim::run_with(&options, Some(rx_item))
        .map(|out| match out.final_key {
            Key::Enter => out.selected_items,
            _ => Vec::new(),
        })
        .unwrap_or_default();

    if !selected.is_empty() {
        // Retrieve the PrInfo back from SkimItem
        // Since we implemented SkimItem for PrInfo, we can assume the output is the number
        let pr_number = selected[0].output().to_string();
        debug!("selected PR #{}", pr_number);

        ctx.write_stdout(&format!("Checking out PR #{}...", pr_number))
            .ok();

        let args = vec!["pr", "checkout", &pr_number];
        match Command::new("gh").args(&args).output() {
            Ok(output) => {
                if !output.status.success() {
                    let error = String::from_utf8_lossy(&output.stderr);
                    let err = error.trim().to_string();
                    ctx.write_stderr(&format!("gpr: failed to checkout PR: {err}"))
                        .ok();
                    return ExitStatus::ExitedWith(1);
                } else {
                    let output = String::from_utf8_lossy(&output.stderr); // gh often talks on stderr
                    let output = output.trim().to_string();
                    ctx.write_stdout(&output.to_string()).ok();
                }
            }
            Err(err) => {
                let err = err.to_string();
                ctx.write_stderr(&format!("gpr: failed to checkout PR: {err}"))
                    .ok();
                return ExitStatus::ExitedWith(1);
            }
        }
    }

    ExitStatus::ExitedWith(0)
}

fn is_git_repository() -> bool {
    Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

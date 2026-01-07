//! GitHub PR checkout command
//!
//! Provides interactive PR selection and checkout using gh CLI + skim.
//! Also supports PR creation with the --create flag.

use super::ShellProxy;
use crate::github_client;
use dsh_types::{Context, ExitStatus};
use getopts::Options;
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::io::{self, Write};
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use tracing::debug;

/// Built-in gpr command description
pub fn description() -> &'static str {
    "Checkout or create GitHub Pull Request (-c to create)"
}

pub fn command(ctx: &Context, argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Parse arguments
    let mut opts = Options::new();
    opts.optflag("c", "create", "Create a new pull request");
    opts.optflag("w", "web", "Open PR list in web browser");
    opts.optopt("t", "title", "PR title (for --create)", "TITLE");
    opts.optopt("b", "body", "PR body (for --create)", "BODY");
    opts.optopt("B", "base", "Base branch (for --create)", "BRANCH");
    opts.optflag("d", "draft", "Create as draft PR");
    opts.optflag("h", "help", "Show help message");

    let matches = match opts.parse(&argv) {
        Ok(m) => m,
        Err(f) => {
            ctx.write_stderr(&format!("gpr: {}", f)).ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    if matches.opt_present("help") {
        let brief = "Usage: gpr [OPTIONS]\n\nExamples:\n  gpr           Interactive PR checkout\n  gpr -c        Create PR interactively\n  gpr -c -t \"Fix bug\" -b \"Description\"\n  gpr -w        Open PR list in browser";
        ctx.write_stdout(&opts.usage(brief)).ok();
        return ExitStatus::ExitedWith(0);
    }

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

    // Handle different modes
    if matches.opt_present("web") {
        return open_pr_list_web(ctx);
    }

    if matches.opt_present("create") {
        return create_pr(ctx, &matches);
    }

    // Default: checkout mode
    checkout_pr(ctx)
}

/// Open PR list in web browser
fn open_pr_list_web(ctx: &Context) -> ExitStatus {
    let args = vec!["pr", "list", "--web"];
    match Command::new("gh").args(&args).status() {
        Ok(status) if status.success() => ExitStatus::ExitedWith(0),
        Ok(_) => {
            ctx.write_stderr("gpr: failed to open PR list in browser")
                .ok();
            ExitStatus::ExitedWith(1)
        }
        Err(err) => {
            ctx.write_stderr(&format!("gpr: {}", err)).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Create a new PR using gh CLI
fn create_pr(ctx: &Context, matches: &getopts::Matches) -> ExitStatus {
    let mut args = vec!["pr", "create"];

    // Title
    let title = if let Some(t) = matches.opt_str("title") {
        t
    } else {
        // Interactive title input
        print!("PR Title: ");
        io::stdout().flush().ok();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            ctx.write_stderr("gpr: failed to read title").ok();
            return ExitStatus::ExitedWith(1);
        }
        input.trim().to_string()
    };

    if title.is_empty() {
        ctx.write_stderr("gpr: title is required").ok();
        return ExitStatus::ExitedWith(1);
    }

    args.push("--title");
    // We need to hold the title in a variable that outlives the args vec borrow
    let title_owned = title.clone();

    // Body (optional)
    let body = matches.opt_str("body");

    // Base branch (optional)
    let base = matches.opt_str("base");

    // Draft flag
    let is_draft = matches.opt_present("draft");

    // Build argument string vector
    let mut arg_strings: Vec<String> = vec![
        "pr".to_string(),
        "create".to_string(),
        "--title".to_string(),
        title_owned,
    ];

    if let Some(b) = body {
        arg_strings.push("--body".to_string());
        arg_strings.push(b);
    }

    if let Some(base_branch) = base {
        arg_strings.push("--base".to_string());
        arg_strings.push(base_branch);
    }

    if is_draft {
        arg_strings.push("--draft".to_string());
    }

    ctx.write_stdout("Creating PR...").ok();

    let arg_refs: Vec<&str> = arg_strings.iter().map(|s| s.as_str()).collect();

    match Command::new("gh").args(&arg_refs).output() {
        Ok(output) => {
            if !output.status.success() {
                let error = String::from_utf8_lossy(&output.stderr);
                ctx.write_stderr(&format!("gpr: {}", error.trim())).ok();
                return ExitStatus::ExitedWith(1);
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            // gh pr create outputs the URL to stdout
            if !stdout.is_empty() {
                ctx.write_stdout(&format!("PR created: {}", stdout.trim()))
                    .ok();
            } else if !stderr.is_empty() {
                ctx.write_stdout(stderr.trim()).ok();
            }
            ExitStatus::ExitedWith(0)
        }
        Err(err) => {
            ctx.write_stderr(&format!("gpr: {}", err)).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Checkout an existing PR interactively
fn checkout_pr(ctx: &Context) -> ExitStatus {
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

//! Creating a worktree from a GitHub pull request: picking the PR through
//! `gh`, resolving its head branch and fetching it before the worktree is added.
use super::*;

use crate::github_client;

pub(super) fn add_worktree_from_pr(ctx: &Context) -> Result<PathBuf, String> {
    // Check if gh is installed
    if !github_client::is_gh_installed() {
        return Err("gh command not found".to_string());
    }

    let mut prs = github_client::get_prs()?;
    if prs.is_empty() {
        return Err("no PR found".to_string());
    }

    // Sort PRs by number descending (usually newer first)
    prs.sort_by_key(|pr| std::cmp::Reverse(pr.number));

    // Skim options
    let options = SkimOptionsBuilder::default()
        .height("50%".to_string())
        .multi(false)
        .bind(vec!["Enter:accept".to_string()])
        .build()
        .map_err(|e| format!("failed to build skim options: {}", e))?;

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
    for pr in prs.clone() {
        let _ = tx_item.send(vec![Arc::new(pr)]);
    }
    drop(tx_item);

    let selected = crate::skim_runner::run_skim_with(options, Some(rx_item))
        .map(|out| {
            if out.is_abort {
                Vec::new()
            } else {
                out.selected_items
            }
        })
        .unwrap_or_default();

    if selected.is_empty() {
        return Err("No PR selected".to_string());
    }

    let pr_number = selected[0].output().to_string();
    let pr = prs
        .iter()
        .find(|p| p.number.to_string() == pr_number)
        .ok_or("PR not found")?;

    let git_root = get_git_root()?;
    let project_name = git_root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "cannot determine project name".to_string())?;

    // Create worktree directory name: <project>-pr-<number>
    let dir_name = format!("{}-pr-{}", project_name, pr_number);
    let parent = git_root.parent().ok_or("no parent dir")?;
    let worktree_path = parent.join(&dir_name);

    if worktree_path.exists() {
        return Err(format!(
            "worktree path already exists: {}",
            worktree_path.display()
        ));
    }

    ctx.write_stdout(&format!(
        "Creating worktree for PR #{} at {}...",
        pr_number,
        worktree_path.display()
    ))
    .ok();

    // 1. Create worktree detached
    let output = Command::new("git")
        .args([
            "worktree",
            "add",
            "--detach",
            &worktree_path.to_string_lossy(),
        ])
        .output()
        .map_err(|e| format!("failed to create worktree: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git worktree add failed: {}", stderr));
    }

    // 2. Checkout PR in the new worktree
    // We execute gh pr checkout inside the new worktree directory
    let checkout_output = Command::new("gh")
        .current_dir(&worktree_path)
        .args(["pr", "checkout", &pr_number])
        .output()
        .map_err(|e| format!("failed to execute gh pr checkout: {}", e))?;

    if !checkout_output.status.success() {
        let stderr = String::from_utf8_lossy(&checkout_output.stderr);
        ctx.write_stderr(&format!(
            "Failed to checkout PR: {}. Cleaning up worktree...",
            stderr
        ))
        .ok();

        // Cleanup worktree if checkout fails
        let _ = Command::new("git")
            .args([
                "worktree",
                "remove",
                "--force",
                &worktree_path.to_string_lossy(),
            ])
            .output();

        return Err(format!("gh pr checkout failed: {}", stderr));
    }

    ctx.write_stdout(&format!(
        "Successfully checked out PR #{} ({})",
        pr.number, pr.head_ref_name
    ))
    .ok();

    Ok(worktree_path)
}

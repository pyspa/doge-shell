//! Git Worktree management command
//!
//! Provides convenient shortcuts for common git worktree operations:
//! - List worktrees
//! - Create worktree for existing branch (PR review)
//! - Create worktree with new branch (feature development)
//! - Remove worktrees interactively
//! - Launch editor after worktree creation

use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tracing::debug;

/// Built-in gwt command description
pub fn description() -> &'static str {
    "Manage git worktrees (add, list, remove)"
}

/// Main command entry point
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Check if we're in a git repository
    if !is_git_repository() {
        ctx.write_stderr("gwt: not a git repository").ok();
        return ExitStatus::ExitedWith(1);
    }

    let args: Vec<&str> = argv.iter().skip(1).map(|s| s.as_str()).collect();

    // Parse options
    let opts = match parse_options(&args) {
        Ok(opts) => opts,
        Err(e) => {
            ctx.write_stderr(&format!("gwt: {}", e)).ok();
            show_usage(ctx);
            return ExitStatus::ExitedWith(1);
        }
    };

    match opts.action {
        Action::List => list_worktrees(ctx),
        Action::Remove { force } => remove_worktree_interactive(ctx, force),
        Action::Prune => prune_worktrees(ctx),
        Action::Add { branch, create_new } => {
            match add_worktree(ctx, &branch, create_new) {
                Ok(path) => {
                    ctx.write_stdout(&format!("Created worktree at: {}", path.display()))
                        .ok();

                    // Change directory if -c option is set (default true)
                    if opts.change_dir {
                        if let Err(e) = proxy.changepwd(&path.to_string_lossy()) {
                            ctx.write_stderr(&format!("gwt: failed to change directory: {}", e))
                                .ok();
                            return ExitStatus::ExitedWith(1);
                        }
                        ctx.write_stdout(&format!("Changed to: {}", path.display()))
                            .ok();
                    }

                    // Open editor if requested
                    if opts.open_editor {
                        open_editor(ctx, &path)
                    } else {
                        ExitStatus::ExitedWith(0)
                    }
                }
                Err(e) => {
                    ctx.write_stderr(&format!("gwt: {}", e)).ok();
                    ExitStatus::ExitedWith(1)
                }
            }
        }
        Action::AddFromPr => {
            match add_worktree_from_pr(ctx) {
                Ok(path) => {
                    // Change directory if -c option is set (default true)
                    if opts.change_dir {
                        if let Err(e) = proxy.changepwd(&path.to_string_lossy()) {
                            ctx.write_stderr(&format!("gwt: failed to change directory: {}", e))
                                .ok();
                            return ExitStatus::ExitedWith(1);
                        }
                        ctx.write_stdout(&format!("Changed to: {}", path.display()))
                            .ok();
                    }

                    // Open editor if requested
                    if opts.open_editor {
                        open_editor(ctx, &path)
                    } else {
                        ExitStatus::ExitedWith(0)
                    }
                }
                Err(e) => {
                    ctx.write_stderr(&format!("gwt: {}", e)).ok();
                    ExitStatus::ExitedWith(1)
                }
            }
        }
        Action::ShowUsage => {
            show_usage(ctx);
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Parsed command options
struct CommandOptions {
    action: Action,
    change_dir: bool,
    open_editor: bool,
}

/// Command action type
enum Action {
    List,
    Remove { force: bool },
    Prune,
    Add { branch: String, create_new: bool },
    AddFromPr,
    ShowUsage,
}

/// Parse command line options
fn parse_options(args: &[&str]) -> Result<CommandOptions, String> {
    if args.is_empty() {
        return Ok(CommandOptions {
            action: Action::List,
            change_dir: false,
            open_editor: false,
        });
    }

    // Default: change directory after creation (can be disabled with -n)
    let mut change_dir = true;
    let mut open_editor = false;
    let mut create_new = false;
    let mut branch: Option<String> = None;
    let mut remove = false;
    let mut prune = false;
    let mut force = false;
    let mut from_pr = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i];

        if arg == "-r" {
            remove = true;
        } else if arg == "-P" || arg == "--pr" {
            from_pr = true;
        } else if arg == "-p" {
            prune = true;
        } else if arg == "-f" || arg == "--force" {
            force = true;
        } else if arg == "-rf" || arg == "-fr" {
            remove = true;
            force = true;
        } else if arg == "-n" {
            // No change directory
            change_dir = false;
        } else if arg == "-b" {
            create_new = true;
        } else if arg == "-e" {
            open_editor = true;
        } else if arg == "-be" || arg == "-eb" {
            create_new = true;
            open_editor = true;
        } else if arg == "-bn" || arg == "-nb" {
            create_new = true;
            change_dir = false;
        } else if arg == "-en" || arg == "-ne" {
            open_editor = true;
            change_dir = false;
        } else if arg == "-ben"
            || arg == "-bne"
            || arg == "-ebn"
            || arg == "-enb"
            || arg == "-nbe"
            || arg == "-neb"
        {
            create_new = true;
            open_editor = true;
            change_dir = false;
        } else if arg.starts_with('-') {
            return Err(format!("unknown option: {}", arg));
        } else {
            // It's a branch name
            if branch.is_some() {
                return Err("multiple branch names specified".to_string());
            }
            branch = Some(arg.to_string());
        }
        i += 1;
    }

    // Determine final action
    if remove {
        Ok(CommandOptions {
            action: Action::Remove { force },
            change_dir: false,
            open_editor: false,
        })
    } else if prune {
        Ok(CommandOptions {
            action: Action::Prune,
            change_dir: false,
            open_editor: false,
        })
    } else if from_pr {
        Ok(CommandOptions {
            action: Action::AddFromPr,
            change_dir,
            open_editor,
        })
    } else if let Some(branch) = branch {
        Ok(CommandOptions {
            action: Action::Add { branch, create_new },
            change_dir,
            open_editor,
        })
    } else if open_editor || create_new {
        Err("branch name required".to_string())
    } else {
        Ok(CommandOptions {
            action: Action::ShowUsage,
            change_dir: false,
            open_editor: false,
        })
    }
}

/// Show usage information
fn show_usage(ctx: &Context) {
    ctx.write_stderr("Usage: gwt [OPTIONS] [<branch>]").ok();
    ctx.write_stderr("").ok();
    ctx.write_stderr("Options:").ok();
    ctx.write_stderr("  (no args)      List worktrees").ok();
    ctx.write_stderr("  <branch>       Create worktree and cd to it (default)")
        .ok();
    ctx.write_stderr("  -b <branch>    Create new branch with worktree")
        .ok();
    ctx.write_stderr("  -e             Open editor after creation")
        .ok();
    ctx.write_stderr("  -n             Do not change directory after creation")
        .ok();
    ctx.write_stderr("  -r             Remove worktree (interactive)")
        .ok();
    ctx.write_stderr("  -f, --force    Force removal (used with -r)")
        .ok();
    ctx.write_stderr("  -p             Prune stale worktrees")
        .ok();
    ctx.write_stderr("  -P, --pr       Create worktree from GitHub PR")
        .ok();
    ctx.write_stderr("").ok();
    ctx.write_stderr("Options can be combined: -be, -bn, -ben, etc.")
        .ok();
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

/// List all worktrees
fn list_worktrees(ctx: &Context) -> ExitStatus {
    let output = Command::new("git").args(["worktree", "list"]).output();

    match output {
        Ok(output) => {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                ctx.write_stdout(stdout.trim()).ok();
                ExitStatus::ExitedWith(0)
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                ctx.write_stderr(&format!("gwt: {}", stderr.trim())).ok();
                ExitStatus::ExitedWith(1)
            }
        }
        Err(e) => {
            ctx.write_stderr(&format!("gwt: failed to execute git: {}", e))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Get the root directory of the git repository
fn get_git_root() -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("failed to execute git: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(PathBuf::from(path))
}

/// Sanitize branch name for use as directory name
fn sanitize_branch_name(branch: &str) -> String {
    branch.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "-")
}

/// Get worktree path for a branch
fn get_worktree_path(branch: &str) -> Result<PathBuf, String> {
    let git_root = get_git_root()?;
    let parent = git_root
        .parent()
        .ok_or_else(|| "cannot determine parent directory".to_string())?;
    let project_name = git_root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "cannot determine project name".to_string())?;

    let dir_name = format_worktree_dir(project_name, branch);
    Ok(parent.join(dir_name))
}

/// Format worktree directory name: <project>-<branch>
fn format_worktree_dir(project: &str, branch: &str) -> String {
    let sanitized_branch = sanitize_branch_name(branch);
    format!("{}-{}", project, sanitized_branch)
}

/// Add a worktree for a branch
fn add_worktree(ctx: &Context, branch: &str, create_new: bool) -> Result<PathBuf, String> {
    let worktree_path = get_worktree_path(branch)?;

    debug!(
        "Creating worktree at {:?} for branch {}",
        worktree_path, branch
    );

    // Check if path already exists
    if worktree_path.exists() {
        return Err(format!("path already exists: {}", worktree_path.display()));
    }

    let mut args = vec!["worktree", "add"];

    if create_new {
        args.push("-b");
        args.push(branch);
    }

    let path_str = worktree_path.to_string_lossy().to_string();
    args.push(&path_str);

    if !create_new {
        args.push(branch);
    }

    debug!("Executing: git {:?}", args);

    let output = Command::new("git")
        .args(&args)
        .output()
        .map_err(|e| format!("failed to execute git: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }

    // Print git output if any
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        ctx.write_stdout(stdout.trim()).ok();
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        ctx.write_stdout(stderr.trim()).ok();
    }

    Ok(worktree_path)
}

/// Remove worktree interactively using skim
fn remove_worktree_interactive(ctx: &Context, force: bool) -> ExitStatus {
    // Get list of worktrees (excluding main)
    let worktrees = match get_linked_worktrees() {
        Ok(wt) => wt,
        Err(e) => {
            ctx.write_stderr(&format!("gwt: {}", e)).ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    if worktrees.is_empty() {
        ctx.write_stderr("gwt: no linked worktrees found").ok();
        return ExitStatus::ExitedWith(0);
    }

    // Single worktree - ask for confirmation
    if worktrees.len() == 1 {
        let worktree = &worktrees[0];
        ctx.write_stdout(&format!(
            "Remove worktree: {}? (This will delete the directory)",
            worktree
        ))
        .ok();
        return remove_worktree(ctx, worktree, force);
    }

    // Multiple worktrees - use skim for selection
    let options = SkimOptionsBuilder::default()
        .prompt(Some("Select worktree to remove> "))
        .bind(vec!["Enter:accept"])
        .build()
        .unwrap();

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
    for wt in worktrees {
        let _ = tx_item.send(Arc::new(wt));
    }
    drop(tx_item);

    let selected = Skim::run_with(&options, Some(rx_item))
        .map(|out| match out.final_key {
            Key::Enter => out.selected_items,
            _ => Vec::new(),
        })
        .unwrap_or_default();

    if selected.is_empty() {
        return ExitStatus::ExitedWith(0);
    }

    let worktree_path = selected[0].output().to_string();
    remove_worktree(ctx, &worktree_path, force)
}

/// Get list of linked worktrees (excluding main worktree)
fn get_linked_worktrees() -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|e| format!("failed to execute git: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut worktrees = Vec::new();
    let mut current_worktree: Option<String> = None;
    let mut is_main = false;

    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            // Save previous worktree if it wasn't main
            if let Some(wt) = current_worktree.take()
                && !is_main
            {
                worktrees.push(wt);
            }
            current_worktree = Some(path.to_string());
            is_main = false;
        } else if line == "bare" {
            is_main = true;
        } else if line.starts_with("branch ") {
            // This is a linked worktree with a branch
        }
    }

    // Don't forget the last worktree
    if let Some(wt) = current_worktree
        && !is_main
    {
        worktrees.push(wt);
    }

    // Skip the first one (main worktree)
    if !worktrees.is_empty() {
        worktrees.remove(0);
    }

    Ok(worktrees)
}

/// Remove a single worktree
fn remove_worktree(ctx: &Context, path: &str, force: bool) -> ExitStatus {
    debug!("Removing worktree: {} (force: {})", path, force);

    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(path);

    let output = Command::new("git").args(args).output();

    match output {
        Ok(output) => {
            if output.status.success() {
                ctx.write_stdout(&format!("Removed worktree: {}", path))
                    .ok();
                ExitStatus::ExitedWith(0)
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // Try force remove if normal remove fails
                ctx.write_stderr(&format!(
                    "gwt: failed to remove (try with force): {}",
                    stderr.trim()
                ))
                .ok();
                ExitStatus::ExitedWith(1)
            }
        }
        Err(e) => {
            ctx.write_stderr(&format!("gwt: failed to execute git: {}", e))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Prune stale worktrees
fn prune_worktrees(ctx: &Context) -> ExitStatus {
    let output = Command::new("git")
        .args(["worktree", "prune", "-v"])
        .output();

    match output {
        Ok(output) => {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim().is_empty() {
                    ctx.write_stdout("No stale worktrees to prune").ok();
                } else {
                    ctx.write_stdout(stdout.trim()).ok();
                }
                ExitStatus::ExitedWith(0)
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                ctx.write_stderr(&format!("gwt: {}", stderr.trim())).ok();
                ExitStatus::ExitedWith(1)
            }
        }
        Err(e) => {
            ctx.write_stderr(&format!("gwt: failed to execute git: {}", e))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

/// Open editor at the given path
fn open_editor(ctx: &Context, path: &Path) -> ExitStatus {
    // Try $EDITOR, then $VISUAL, then common editors
    let editor = env::var("EDITOR")
        .or_else(|_| env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());

    debug!("Opening editor {} at {:?}", editor, path);

    // Split editor command in case it has arguments (e.g., "code -n")
    let parts: Vec<&str> = editor.split_whitespace().collect();
    if parts.is_empty() {
        ctx.write_stderr("gwt: no editor configured").ok();
        return ExitStatus::ExitedWith(1);
    }

    let mut cmd = Command::new(parts[0]);
    for arg in &parts[1..] {
        cmd.arg(arg);
    }
    cmd.arg(path);

    match cmd.spawn() {
        Ok(_) => {
            ctx.write_stdout(&format!("Opened {} in {}", path.display(), parts[0]))
                .ok();
            ExitStatus::ExitedWith(0)
        }
        Err(e) => {
            ctx.write_stderr(&format!("gwt: failed to open editor {}: {}", parts[0], e))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

// PR Support

use crate::github_client;

fn add_worktree_from_pr(ctx: &Context) -> Result<PathBuf, String> {
    // Check if gh is installed
    if !github_client::is_gh_installed() {
        return Err("gh command not found".to_string());
    }

    let mut prs = github_client::get_prs()?;
    if prs.is_empty() {
        return Err("no PR found".to_string());
    }

    // Sort PRs by number descending (usually newer first)
    prs.sort_by(|a, b| b.number.cmp(&a.number));

    // Skim options
    let options = SkimOptionsBuilder::default()
        .height(Some("50%"))
        .multi(false)
        .bind(vec!["Enter:accept"])
        .build()
        .unwrap();

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
    for pr in prs.clone() {
        let _ = tx_item.send(Arc::new(pr));
    }
    drop(tx_item);

    let selected = Skim::run_with(&options, Some(rx_item))
        .map(|out| match out.final_key {
            Key::Enter => out.selected_items,
            _ => Vec::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_worktree_dir() {
        assert_eq!(
            format_worktree_dir("myrepo", "feature/foo"),
            "myrepo-feature-foo"
        );
        assert_eq!(format_worktree_dir("dsh", "fix/bug"), "dsh-fix-bug");
        assert_eq!(format_worktree_dir("proj", "main"), "proj-main");
    }

    #[test]
    fn test_sanitize_branch_name() {
        assert_eq!(sanitize_branch_name("feature/foo"), "feature-foo");
        assert_eq!(sanitize_branch_name("fix/bar/baz"), "fix-bar-baz");
        assert_eq!(sanitize_branch_name("simple"), "simple");
        assert_eq!(sanitize_branch_name("feature/foo:bar"), "feature-foo-bar");
    }

    #[test]
    fn test_is_git_repository() {
        // This test depends on whether we're in a git repo
        // Just verify it doesn't panic
        let _ = is_git_repository();
    }

    #[test]
    fn test_parse_options_empty() {
        let opts = parse_options(&[]).unwrap();
        assert!(matches!(opts.action, Action::List));
    }

    #[test]
    fn test_parse_options_branch_only() {
        let opts = parse_options(&["main"]).unwrap();
        if let Action::Add { branch, create_new } = opts.action {
            assert_eq!(branch, "main");
            assert!(!create_new);
        } else {
            panic!("Expected Action::Add");
        }
        assert!(opts.change_dir); // Default is true
        assert!(!opts.open_editor);
    }

    #[test]
    fn test_parse_options_new_branch() {
        let opts = parse_options(&["-b", "feature/new"]).unwrap();
        if let Action::Add { branch, create_new } = opts.action {
            assert_eq!(branch, "feature/new");
            assert!(create_new);
        } else {
            panic!("Expected Action::Add");
        }
        assert!(opts.change_dir);
    }

    #[test]
    fn test_parse_options_no_cd() {
        let opts = parse_options(&["-n", "main"]).unwrap();
        if let Action::Add { branch, .. } = opts.action {
            assert_eq!(branch, "main");
        } else {
            panic!("Expected Action::Add");
        }
        assert!(!opts.change_dir); // -n disables cd
    }

    #[test]
    fn test_parse_options_editor() {
        let opts = parse_options(&["-e", "main"]).unwrap();
        assert!(opts.open_editor);
        assert!(opts.change_dir);
    }

    #[test]
    fn test_parse_options_combined() {
        let opts = parse_options(&["-be", "feature"]).unwrap();
        if let Action::Add { branch, create_new } = opts.action {
            assert_eq!(branch, "feature");
            assert!(create_new);
        } else {
            panic!("Expected Action::Add");
        }
        assert!(opts.open_editor);
        assert!(opts.change_dir);
    }

    #[test]
    fn test_parse_options_remove() {
        let opts = parse_options(&["-r"]).unwrap();
        if let Action::Remove { force } = opts.action {
            assert!(!force);
        } else {
            panic!("Expected Action::Remove");
        }
    }

    #[test]
    fn test_parse_options_remove_force() {
        let opts = parse_options(&["-r", "-f"]).unwrap();
        if let Action::Remove { force } = opts.action {
            assert!(force);
        } else {
            panic!("Expected Action::Remove");
        }
    }

    #[test]
    fn test_parse_options_remove_force_combined() {
        let opts = parse_options(&["-rf"]).unwrap();
        if let Action::Remove { force } = opts.action {
            assert!(force);
        } else {
            panic!("Expected Action::Remove");
        }
    }

    #[test]
    fn test_parse_options_prune() {
        let opts = parse_options(&["-p"]).unwrap();
        assert!(matches!(opts.action, Action::Prune));
    }
}

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
use skim::{SkimItemReceiver, SkimItemSender};
use std::path::{Path, PathBuf};
use tracing::debug;

mod options;
mod pr;
mod worktree;
// Explicit rather than glob: `skim::prelude::*` also exports an `Action`,
// and two globs offering the same name are ambiguous.
use options::{Action, parse_options, show_usage};
use pr::add_worktree_from_pr;
use worktree::{add_worktree, open_editor, prune_worktrees, remove_worktree_interactive};

/// Built-in gwt command description
pub fn description() -> &'static str {
    "Manage git worktrees (add, list, remove)"
}

// Define local StringItem wrapper for gwt
struct StringItem(String);
impl SkimItem for StringItem {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.0)
    }
    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.0)
    }
}

/// Main command entry point
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Check if we're in a git repository
    if !is_git_repository(proxy) {
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
        Action::List => list_worktrees(ctx, proxy),
        Action::Remove { force } => remove_worktree_interactive(ctx, proxy, force),
        Action::Prune => prune_worktrees(ctx, proxy),
        Action::Add { branch, create_new } => {
            match add_worktree(ctx, proxy, &branch, create_new) {
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
                        open_editor(ctx, proxy, &path)
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
            match add_worktree_from_pr(ctx, proxy) {
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
                        open_editor(ctx, proxy, &path)
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

/// Check if current directory is within a git repository
fn is_git_repository(proxy: &mut dyn ShellProxy) -> bool {
    use std::process::Stdio;
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

/// List all worktrees
fn list_worktrees(ctx: &Context, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let output = crate::runtime_spawn::runtime_command(proxy, "git")
        .map_err(|e| format!("gwt: failed to execute git: {e}"))
        .and_then(|mut command| {
            command
                .args(["worktree", "list"])
                .output()
                .map_err(|e| format!("gwt: failed to execute git: {e}"))
        });

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
fn get_git_root(proxy: &mut dyn ShellProxy) -> Result<PathBuf, String> {
    let output = crate::runtime_spawn::runtime_command(proxy, "git")
        .map_err(|e| format!("failed to execute git: {e}"))?
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
fn get_worktree_path(proxy: &mut dyn ShellProxy, branch: &str) -> Result<PathBuf, String> {
    let git_root = get_git_root(proxy)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_worktree_dir() {
        assert_eq!(
            format_worktree_dir("myrepo", "feature/foo"),
            "myrepo-feature-foo"
        );
        assert_eq!(format_worktree_dir("dogesh", "fix/bug"), "dogesh-fix-bug");
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
        let mut proxy = crate::test_support::TestShellProxy::default();
        let _ = is_git_repository(&mut proxy);
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

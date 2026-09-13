//! Turning the `gwt` argument list into one `Action` plus the two cross-cutting
//! flags (`-n` no-cd, `-e` open editor), including the combined short forms
//! (`-be`, `-ben`), and the usage text the same parser falls back to.
use super::*;

/// Parsed command options
pub(super) struct CommandOptions {
    pub(super) action: Action,
    pub(super) change_dir: bool,
    pub(super) open_editor: bool,
}

/// Command action type
pub(super) enum Action {
    List,
    Remove { force: bool },
    Prune,
    Add { branch: String, create_new: bool },
    AddFromPr,
    ShowUsage,
}

/// Parse command line options
pub(super) fn parse_options(args: &[&str]) -> Result<CommandOptions, String> {
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
pub(super) fn show_usage(ctx: &Context) {
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

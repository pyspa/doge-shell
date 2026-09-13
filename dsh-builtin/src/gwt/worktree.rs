//! The worktree operations themselves: creating one for a branch, the skim
//! picker for removal, listing the linked worktrees git knows about, removal by
//! path, pruning, and handing the result to the user's editor.
use super::*;

/// Add a worktree for a branch
pub(super) fn add_worktree(
    ctx: &Context,
    branch: &str,
    create_new: bool,
) -> Result<PathBuf, String> {
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
pub(super) fn remove_worktree_interactive(ctx: &Context, force: bool) -> ExitStatus {
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
        .prompt("Select worktree to remove> ".to_string())
        .bind(vec!["Enter:accept".to_string()])
        .build()
        .map_err(|e| format!("failed to build skim options: {}", e));

    let options = match options {
        Ok(o) => o,
        Err(e) => {
            let _ = ctx.write_stderr(&format!("gwt: {}\n", e));
            return ExitStatus::ExitedWith(1);
        }
    };

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
    for wt in worktrees {
        let _ = tx_item.send(vec![Arc::new(StringItem(wt))]);
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
        return ExitStatus::ExitedWith(0);
    }

    let worktree_path = selected[0].output().to_string();
    remove_worktree(ctx, &worktree_path, force)
}

/// Get list of linked worktrees (excluding main worktree)
pub(super) fn get_linked_worktrees() -> Result<Vec<String>, String> {
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
pub(super) fn prune_worktrees(ctx: &Context) -> ExitStatus {
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
pub(super) fn open_editor(ctx: &Context, path: &Path) -> ExitStatus {
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

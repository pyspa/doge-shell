use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::borrow::Cow;
use std::process::{Command, Stdio};
use std::sync::Arc;

pub fn description() -> &'static str {
    "Interactive git add selection"
}

#[derive(Debug, Clone)]
struct GitFileItem {
    path: String,
    display: String,
    index: usize,
}

impl SkimItem for GitFileItem {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.display)
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.path)
    }

    fn preview(&self, _context: PreviewContext) -> ItemPreview {
        let output = Command::new("git")
            .args(["diff", "--color=always", "--", &self.path])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_else(|_| "".to_string());
        ItemPreview::AnsiText(output)
    }

    fn get_index(&self) -> usize {
        self.index
    }

    fn set_index(&mut self, index: usize) {
        self.index = index;
    }
}

pub fn command(ctx: &Context, _argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    if !is_git_repository() {
        ctx.write_stderr("ga: not a git repository").ok();
        return ExitStatus::ExitedWith(1);
    }

    let files = match get_git_status() {
        Ok(f) => f,
        Err(e) => {
            ctx.write_stderr(&format!("ga: failed to get status: {}", e))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    if files.is_empty() {
        return ExitStatus::ExitedWith(0);
    }

    // Prepare items with index
    let skim_items: Vec<Arc<dyn SkimItem>> = files
        .into_iter()
        .enumerate()
        .map(|(i, mut f)| {
            f.index = i;
            Arc::new(f) as Arc<dyn SkimItem>
        })
        .collect();

    let options = SkimOptionsBuilder::default()
        .multi(true)
        .prompt("Git Add> ".to_string())
        .bind(vec!["Enter:accept".to_string(), "Space:toggle".to_string()])
        .preview(Some("".to_string())) // Preview handled by ItemPreview
        .preview_window("right:60%".to_string())
        .build()
        .unwrap();

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
    for item in skim_items {
        let _ = tx_item.send(item);
    }
    drop(tx_item);

    let selected_items = Skim::run_with(&options, Some(rx_item))
        .map(|out| out.selected_items)
        .unwrap_or_default();

    if selected_items.is_empty() {
        return ExitStatus::ExitedWith(0);
    }

    let mut added_files = Vec::new();
    for item in selected_items {
        let path = item.output().to_string();
        added_files.push(path);
    }

    if added_files.is_empty() {
        return ExitStatus::ExitedWith(0);
    }

    // Run git add
    let mut args = vec!["add"];
    args.extend(added_files.iter().map(|s| s.as_str()));

    match Command::new("git").args(&args).output() {
        Ok(output) => {
            if output.status.success() {
                ctx.write_stdout(&format!("Added {} files.", added_files.len()))
                    .ok();
                ExitStatus::ExitedWith(0)
            } else {
                let error = String::from_utf8_lossy(&output.stderr);
                ctx.write_stderr(&format!("ga: failed to add files: {}", error.trim()))
                    .ok();
                ExitStatus::ExitedWith(1)
            }
        }
        Err(e) => {
            ctx.write_stderr(&format!("ga: failed to execute git: {}", e))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
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

fn get_git_status() -> Result<Vec<GitFileItem>, String> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .map_err(|e| format!("{}", e))?;

    if !output.status.success() {
        return Err("git status failed".to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();

    for line in stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        let path = &line[3..];

        entries.push(GitFileItem {
            path: path.to_string(),
            display: line.to_string(),
            index: 0, // Will be set later
        });
    }

    Ok(entries)
}

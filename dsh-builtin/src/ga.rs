use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use skim::prelude::*;
use skim::{Skim, SkimItemReceiver, SkimItemSender};
use std::borrow::Cow;
use std::process::{Command, Stdio};

pub fn description() -> &'static str {
    "Interactive git add selection"
}

#[derive(Debug, Clone)]
struct FileStatus {
    path: String,
    #[allow(dead_code)]
    status: String,
    display: String,
}

impl SkimItem for FileStatus {
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.display)
    }

    fn output(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.path)
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
        ctx.write_stdout("No modified files.").ok();
        return ExitStatus::ExitedWith(0);
    }

    let options = SkimOptionsBuilder::default()
        .multi(true)
        .prompt("Git Add> ".to_string())
        .bind(vec!["Enter:accept".to_string(), "Space:toggle".to_string()])
        .build()
        .unwrap();

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
    for file in files {
        let _ = tx_item.send(Arc::new(file));
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

fn get_git_status() -> Result<Vec<FileStatus>, String> {
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
        let status = &line[0..2];
        let path = &line[3..];

        entries.push(FileStatus {
            path: path.to_string(),
            status: status.to_string(),
            display: line.to_string(),
        });
    }

    Ok(entries)
}

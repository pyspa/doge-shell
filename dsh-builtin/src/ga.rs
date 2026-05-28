use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use skim::prelude::*;
use skim::{SkimItemReceiver, SkimItemSender};
use std::borrow::Cow;
use std::process::{Command, Stdio};
use std::sync::Arc;

pub fn description() -> &'static str {
    "Interactively select files to stage with git add"
}

const GA_SKIM_BINDINGS: &[&str] = &[
    "enter:accept",
    "space:toggle+down",
    "tab:toggle+down",
    "btab:toggle+up",
];

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

    let options = match build_ga_skim_options() {
        Ok(o) => o,
        Err(e) => {
            let _ = ctx.write_stderr(&format!("ga: {}\n", e));
            return ExitStatus::ExitedWith(1);
        }
    };

    let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();

    // item is already Arc<dyn SkimItem>, just wrap in vec
    for item in skim_items {
        let _ = tx_item.send(vec![item]);
    }
    drop(tx_item); // Close sender

    let selected = crate::skim_runner::run_skim_with(options, Some(rx_item))
        .map(|out| out.selected_items)
        .unwrap_or_default();

    if selected.is_empty() {
        return ExitStatus::ExitedWith(0);
    }

    let mut added_files = Vec::new();
    for item in selected {
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

fn build_ga_skim_options() -> Result<SkimOptions, String> {
    SkimOptionsBuilder::default()
        .multi(true)
        .prompt("Git Add> ".to_string())
        .bind(
            GA_SKIM_BINDINGS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
        .preview("".to_string()) // Preview handled by ItemPreview
        // .preview_window("right:60%") // Disabled
        .build()
        .map_err(|e| format!("failed to build skim options: {}", e))
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

#[cfg(test)]
mod tests {
    use super::*;
    use skim::binds::parse_key;

    #[test]
    fn ga_skim_options_enable_multi_select() {
        let options = build_ga_skim_options().expect("ga skim options should build");

        assert!(options.multi);
    }

    #[test]
    fn ga_skim_options_register_multi_select_bindings() {
        let options = build_ga_skim_options().expect("ga skim options should build");

        for key in ["space", "tab", "btab", "enter"] {
            let key_event = parse_key(key).expect("test key should parse");
            assert!(
                options.keymap.contains_key(&key_event),
                "expected binding for {key}"
            );
        }

        let space = parse_key("space").expect("space should parse");
        let space_actions = options.keymap.get(&space).expect("space should be bound");
        assert_eq!(format!("{space_actions:?}"), "[Toggle, Down(1)]");
    }

    #[test]
    fn ga_skim_bindings_use_lowercase_key_names() {
        for binding in GA_SKIM_BINDINGS {
            let (key, _) = binding
                .split_once(':')
                .expect("ga skim binding should include an action");
            assert_eq!(key, key.to_ascii_lowercase());
        }
    }
}

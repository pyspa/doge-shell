use super::ShellProxy;
use anyhow::{Context as _, Result};
use dsh_types::{Context, ExitStatus};
use std::process::Command;

pub fn description() -> &'static str {
    "Open current project in Emacs"
}

pub fn command(ctx: &Context, _argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    match run_eproject(ctx) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            let _ = ctx.write_stderr(&format!("eproject: {}", e));
            ExitStatus::ExitedWith(1)
        }
    }
}

fn run_eproject(ctx: &Context) -> Result<()> {
    // 1. Try to find git root
    let root = match get_git_root() {
        Ok(path) => path,
        Err(_) => {
            // Fallback to current directory
            std::env::current_dir()?.to_string_lossy().to_string()
        }
    };

    // 2. Call emacsclient -n <root>
    // -n: return immediately, don't wait for emacs to "finish" buffer
    let status = Command::new("emacsclient")
        .arg("-n")
        .arg(&root)
        .status()
        .context("failed to launch emacsclient")?;

    if !status.success() {
        return Err(anyhow::anyhow!("emacsclient exited with non-zero status"));
    }

    let _ = ctx.write_stdout(&format!("Opened project: {}", root));

    Ok(())
}

fn get_git_root() -> Result<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()?;

    if output.status.success() {
        let s = String::from_utf8(output.stdout)?;
        Ok(s.trim().to_string())
    } else {
        Err(anyhow::anyhow!("Not a git repository"))
    }
}

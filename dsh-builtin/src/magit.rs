use super::ShellProxy;
use anyhow::{Context as _, Result};
use dsh_types::{Context, ExitStatus};
use std::process::Command;

pub fn description() -> &'static str {
    "Open Magit status for the current directory"
}

pub fn command(ctx: &Context, _argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    match run_magit() {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            let _ = ctx.write_stderr(&format!("magit: {}", e));
            ExitStatus::ExitedWith(1)
        }
    }
}

fn run_magit() -> Result<()> {
    // We assume emacs server is running.
    // We use -nw to run in terminal mode.
    // We execute (magit-status) which uses default-directory (inherited CWD).
    let status = Command::new("emacsclient")
        .arg("-nw")
        .arg("--eval")
        .arg("(magit-status)")
        .status()
        .context("failed to launch emacsclient (is Emacs server running?)")?;

    if !status.success() {
        return Err(anyhow::anyhow!("emacsclient exited with non-zero status"));
    }

    Ok(())
}

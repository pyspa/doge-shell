use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in history command description
pub fn description() -> &'static str {
    "Show command history"
}

/// Built-in history command implementation
/// Displays the command history by delegating to the shell's history system
/// This command shows previously executed commands for user reference
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Delegate history display to the shell's internal history management
    match proxy.dispatch(ctx, "history", argv) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(err) => {
            let _ = ctx.write_stderr(&format!("history: {err}"));
            ExitStatus::ExitedWith(1)
        }
    }
}

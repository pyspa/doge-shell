use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in history command implementation
/// Displays the command history by delegating to the shell's history system
/// This command shows previously executed commands for user reference
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Delegate history display to the shell's internal history management
    proxy.dispatch(ctx, "history", argv).unwrap();
    ExitStatus::ExitedWith(0)
}

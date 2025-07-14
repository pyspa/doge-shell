use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in bg (background) command implementation
/// Resumes a suspended job in the background, allowing it to continue execution
/// Part of the shell's job control system for managing process execution
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match proxy.dispatch(ctx, "bg", argv) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            // Report any errors that occur during job backgrounding
            ctx.write_stderr(&format!("bg: {e}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

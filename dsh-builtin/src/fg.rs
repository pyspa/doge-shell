use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in fg (foreground) command implementation
/// Brings a background job to the foreground for interactive execution
/// Part of the shell's job control system for managing process execution
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match proxy.dispatch(ctx, "fg", argv) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            // Report any errors that occur during job foregrounding
            ctx.write_stderr(&format!("fg: {e}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

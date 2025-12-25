use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in var command description
pub fn description() -> &'static str {
    "Manage shell variables"
}

/// Built-in var command implementation
/// Displays or manages shell variables
/// Delegates to the shell's variable management system for listing and manipulation
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Delegate variable operations to the shell's variable management system
    if let Err(e) = proxy.dispatch(ctx, "var", argv) {
        let _ = ctx.write_stderr(&format!("Error: {}\n", e));
        return ExitStatus::ExitedWith(1);
    }
    ExitStatus::ExitedWith(0)
}

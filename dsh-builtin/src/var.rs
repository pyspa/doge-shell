use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in var command implementation
/// Displays or manages shell variables
/// Delegates to the shell's variable management system for listing and manipulation
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Delegate variable operations to the shell's variable management system
    proxy.dispatch(ctx, "var", argv).unwrap();
    ExitStatus::ExitedWith(0)
}

use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in read command description
pub fn description() -> &'static str {
    "Read a line from standard input"
}

/// Built-in read command implementation
/// Reads input from stdin and stores it in shell variables
/// Commonly used in shell scripts for interactive input collection
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Delegate input reading to the shell's input handling system
    proxy.dispatch(ctx, "read", argv).unwrap();
    ExitStatus::ExitedWith(0)
}

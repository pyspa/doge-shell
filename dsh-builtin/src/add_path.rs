use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in add_path command description
pub fn description() -> &'static str {
    "Add paths to the PATH environment variable"
}

/// Built-in add_path command implementation
/// Adds a directory to the beginning of the PATH environment variable
/// Supports tilde expansion for home directory references
pub fn command(_ctx: &Context, args: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Expand tilde (~) to home directory path if present
    let path = shellexpand::tilde(&args[1]);

    // Insert the path at the beginning of PATH (index 0 = highest priority)
    proxy.insert_path(0, &path);
    ExitStatus::ExitedWith(0)
}

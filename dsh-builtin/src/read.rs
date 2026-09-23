use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in read command description
pub fn description() -> &'static str {
    "Read a line from standard input"
}

/// Built-in read command implementation
/// Reads one line from stdin and stores it in a shell variable.
/// Only `read NAME` is supported; anything else is a usage error.
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // `read` owns its exit status (success vs. EOF vs. usage), so it goes
    // through `ReadCapability` rather than `CoreShellAction::Read`.
    match proxy.read_shell_line(ctx, argv) {
        Ok(status) => status,
        Err(e) => {
            let _ = ctx.write_stderr(&format!("read: {}", e));
            ExitStatus::ExitedWith(1)
        }
    }
}

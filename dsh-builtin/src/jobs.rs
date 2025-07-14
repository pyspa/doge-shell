use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

/// Built-in jobs command implementation
/// Lists all active background jobs in the current shell session
/// Shows job IDs, status, and command information for job control
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match proxy.dispatch(ctx, "jobs", argv) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            // Report any errors that occur during job listing
            ctx.write_stderr(&format!("jobs: {e}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use tracing::debug;

/// Built-in z command description
pub fn description() -> &'static str {
    "Jump to frequently used directories"
}

/// Built-in z command implementation
/// Provides frecency-based directory navigation similar to the z utility
/// Allows users to quickly jump to frequently and recently visited directories
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    debug!("call z");
    // Delegate to shell's frecency-based directory navigation system
    match proxy.dispatch(ctx, "z", argv) {
        Ok(()) => ExitStatus::ExitedWith(0),
        Err(e) => {
            debug!("z command failed: {}", e);
            ctx.write_stderr(&format!("z: failed to change directory: {}", e))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

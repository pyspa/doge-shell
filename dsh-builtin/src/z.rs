use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use tracing::debug;

/// Built-in z command implementation
/// Provides frecency-based directory navigation similar to the z utility
/// Allows users to quickly jump to frequently and recently visited directories
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    debug!("call z");
    // Delegate to shell's frecency-based directory navigation system
    proxy.dispatch(ctx, "z", argv).unwrap();
    ExitStatus::ExitedWith(0)
}

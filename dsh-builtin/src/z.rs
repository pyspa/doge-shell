use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use tracing::debug;

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    debug!("call z");
    proxy.dispatch(ctx, "z", argv).unwrap();
    ExitStatus::ExitedWith(0)
}

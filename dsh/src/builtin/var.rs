use crate::builtin::ShellProxy;
use dsh_types::{Context, ExitStatus};

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    proxy.dispatch(ctx, "var", argv).unwrap();
    ExitStatus::ExitedWith(0)
}

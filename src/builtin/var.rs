use crate::builtin::ShellProxy;
use crate::context::Context;
use crate::exitstatus::ExitStatus;

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    proxy.run_builtin(ctx, "var", argv).unwrap();
    ExitStatus::ExitedWith(0)
}

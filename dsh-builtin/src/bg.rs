use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match proxy.dispatch(ctx, "bg", argv) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            ctx.write_stderr(&format!("bg: {e}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

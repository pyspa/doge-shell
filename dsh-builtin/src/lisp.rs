use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        ctx.write_stderr("lisp: missing s-expression").ok();
    } else {
        proxy.dispatch(ctx, "lisp", argv).unwrap();
    }
    ExitStatus::ExitedWith(0)
}

pub fn run(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    match proxy.dispatch(ctx, "lisp-run", argv) {
        Ok(_) => ExitStatus::ExitedWith(0),
        _ => ExitStatus::ExitedWith(1),
    }
}

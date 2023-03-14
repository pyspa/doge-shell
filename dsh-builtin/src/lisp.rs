use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        println!("lisp: missing s-expression");
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

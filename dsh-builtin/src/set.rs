use super::ShellProxy;
use dsh_types::{Context, ExitStatus};

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 3 {
        println!("set variable");
        println!("set KEY VALUE");
    } else {
        proxy.dispatch(ctx, "set", argv).unwrap();
    }
    ExitStatus::ExitedWith(0)
}

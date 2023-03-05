use crate::builtin::ShellProxy;
use crate::context::Context;
use crate::exitstatus::ExitStatus;

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 3 {
        println!("set variable");
        println!("set KEY VALUE");
    } else {
        proxy.run_builtin(ctx, "set", argv).unwrap();
    }
    ExitStatus::ExitedWith(0)
}

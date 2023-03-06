use crate::builtin::ShellProxy;
use dsh_types::{Context, ExitStatus};

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        println!("lisp: missing s-expression");
    } else {
        proxy.run_builtin(ctx, "lisp", argv).unwrap();
    }
    ExitStatus::ExitedWith(0)
}

// pub fn run(_ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
//     let mut argv = argv;
//     let cmd = argv.remove(0);

//     match shell.lisp_engine.borrow().run_func(cmd.as_str(), argv) {
//         Ok(val) => {
//             debug!("{}", val);
//         }
//         Err(err) => {
//             eprintln!("{}", err);
//         }
//     }

//     ExitStatus::ExitedWith(0)
// }

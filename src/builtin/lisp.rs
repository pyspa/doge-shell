use crate::process::{Context, ExitStatus};
use crate::shell::Shell;
use tracing::debug;

pub fn command(_ctx: &Context, argv: Vec<String>, shell: &mut Shell) -> ExitStatus {
    if argv.len() < 2 {
        println!("lisp: missing s-expression");
    } else {
        match shell.environment.lisp_engine.borrow().run(argv[1].as_str()) {
            Ok(val) => {
                debug!("{}", val);
            }
            Err(err) => {
                eprintln!("{}", err);
            }
        }
    }
    ExitStatus::ExitedWith(0)
}

pub fn run(_ctx: &Context, argv: Vec<String>, shell: &mut Shell) -> ExitStatus {
    let mut argv = argv;
    let cmd = argv.remove(0);

    match shell
        .environment
        .lisp_engine
        .borrow()
        .run_func(cmd.as_str(), argv)
    {
        Ok(val) => {
            debug!("{}", val);
        }
        Err(err) => {
            eprintln!("{}", err);
        }
    }

    ExitStatus::ExitedWith(0)
}

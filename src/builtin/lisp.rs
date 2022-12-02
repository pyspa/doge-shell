use crate::process::{Context, ExitStatus};
use crate::shell::Shell;

pub fn command(_ctx: &Context, argv: Vec<String>, shell: &mut Shell) -> ExitStatus {
    if argv.len() < 2 {
        println!("lisp: missing s-expression");
    } else {
        match shell.environment.lisp_engine.borrow().run(argv[1].as_str()) {
            Ok(val) => {
                println!("{}", val);
            }
            Err(err) => {
                eprintln!("{}", err);
            }
        }
    }
    ExitStatus::ExitedWith(0)
}

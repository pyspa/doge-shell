use crate::process::{Context, ExitStatus};
use crate::shell::Shell;

pub fn command(_ctx: &Context, argv: Vec<String>, shell: &mut Shell) -> ExitStatus {
    if argv.len() < 3 {
        println!("set variable");
        println!("set KEY VALUE");
    } else {
        let key = format!("${}", argv[1]);
        let val = argv[2].to_string();
        shell.environment.borrow_mut().variables.insert(key, val);
    }
    ExitStatus::ExitedWith(0)
}

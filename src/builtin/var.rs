use crate::process::{Context, ExitStatus};
use crate::shell::Shell;
use tabled::{Table, Tabled};

#[derive(Tabled)]
struct Var {
    key: String,
    value: String,
}

pub fn command(_ctx: &Context, _argv: Vec<String>, shell: &mut Shell) -> ExitStatus {
    let vars: Vec<Var> = shell
        .environment
        .borrow()
        .variables
        .iter()
        .map(|x| Var {
            key: x.0.to_owned(),
            value: x.1.to_owned(),
        })
        .collect();
    let table = Table::new(vars).to_string();
    shell.print_stdout(table);

    ExitStatus::ExitedWith(0)
}

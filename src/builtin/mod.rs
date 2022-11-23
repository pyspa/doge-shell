use crate::process::{Context, ExitStatus};
use crate::shell::Shell;
use once_cell::sync::Lazy;
use std::collections::HashMap;

pub mod cd;
pub mod history;
mod z;

pub type BuiltinCommand = fn(ctx: &Context, argv: Vec<String>, shell: &mut Shell) -> ExitStatus;

pub static BUILTIN_COMMAND: Lazy<HashMap<&str, BuiltinCommand>> = Lazy::new(|| {
    let mut builtin = HashMap::new();

    // Add builtin command
    builtin.insert("exit", exit as BuiltinCommand);
    builtin.insert("cd", cd::command as BuiltinCommand);
    builtin.insert("history", history::command as BuiltinCommand);
    builtin.insert("z", z::command as BuiltinCommand);
    builtin
});

pub fn exit(_ctx: &Context, _argv: Vec<String>, shell: &mut Shell) -> ExitStatus {
    shell.exit();
    ExitStatus::ExitedWith(0)
}

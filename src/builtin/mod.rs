use crate::process::{Context, ExitStatus};
use crate::shell::Shell;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

pub mod cd;
pub mod history;
mod jobs;
mod lisp;
mod z;

pub type BuiltinCommand = fn(ctx: &Context, argv: Vec<String>, shell: &mut Shell) -> ExitStatus;

pub static BUILTIN_COMMAND: Lazy<Mutex<HashMap<&str, BuiltinCommand>>> = Lazy::new(|| {
    let mut builtin = HashMap::new();

    // Add builtin command
    builtin.insert("exit", exit as BuiltinCommand);
    builtin.insert("cd", cd::command as BuiltinCommand);
    builtin.insert("history", history::command as BuiltinCommand);
    builtin.insert("z", z::command as BuiltinCommand);
    builtin.insert("jobs", jobs::command as BuiltinCommand);
    builtin.insert("lisp", lisp::command as BuiltinCommand);
    Mutex::new(builtin)
});

pub fn get_command(name: &str) -> Option<BuiltinCommand> {
    if let Ok(builtin) = BUILTIN_COMMAND.lock() {
        builtin.get(name).copied()
    } else {
        None
    }
}

pub fn exit(_ctx: &Context, _argv: Vec<String>, shell: &mut Shell) -> ExitStatus {
    shell.exit();
    ExitStatus::ExitedWith(0)
}

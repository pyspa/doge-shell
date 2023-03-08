use anyhow::Result;
use dsh_types::{Context, ExitStatus};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

pub mod cd;
pub mod history;
pub mod jobs;
pub mod lisp;
mod read;
mod set;
mod var;
mod z;

pub trait ShellProxy {
    fn dispatch(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()>;
    fn save_path_history(&mut self, path: &str);
    fn changepwd(&mut self, path: &str);
}

pub type BuiltinCommand =
    fn(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus;

pub static BUILTIN_COMMAND: Lazy<Mutex<HashMap<&str, BuiltinCommand>>> = Lazy::new(|| {
    let mut builtin = HashMap::new();

    // Add builtin command
    builtin.insert("exit", exit as BuiltinCommand);
    builtin.insert("cd", cd::command as BuiltinCommand);
    builtin.insert("history", history::command as BuiltinCommand);
    builtin.insert("z", z::command as BuiltinCommand);
    builtin.insert("jobs", jobs::command as BuiltinCommand);
    builtin.insert("lisp", lisp::command as BuiltinCommand);
    builtin.insert("set", set::command as BuiltinCommand);
    builtin.insert("var", var::command as BuiltinCommand);
    builtin.insert("read", read::command as BuiltinCommand);

    Mutex::new(builtin)
});

pub fn get_command(name: &str) -> Option<BuiltinCommand> {
    if let Ok(builtin) = BUILTIN_COMMAND.lock() {
        builtin.get(name).copied()
    } else {
        None
    }
}

pub fn exit(ctx: &Context, _argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let _ret = proxy.dispatch(ctx, "exit", Vec::new());
    ExitStatus::ExitedWith(0)
}

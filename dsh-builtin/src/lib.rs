use anyhow::Result;
use dsh_types::{Context, ExitStatus};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

mod add_path;
pub mod cd;
mod chatgpt;
mod history;
mod jobs;
pub mod lisp;
mod read;
mod set;
mod uuid;
mod var;
mod z;

pub trait ShellProxy {
    fn exit_shell(&mut self);
    fn dispatch(&mut self, ctx: &Context, cmd: &str, argv: Vec<String>) -> Result<()>;
    fn save_path_history(&mut self, path: &str);
    fn changepwd(&mut self, path: &str) -> Result<()>;
    fn insert_path(&mut self, index: usize, path: &str);
    fn get_var(&mut self, key: &str) -> Option<String>;
    fn set_var(&mut self, key: String, value: String);
    fn set_env_var(&mut self, key: String, value: String);
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
    builtin.insert("chat", chatgpt::chat as BuiltinCommand);
    builtin.insert("chat_prompt", chatgpt::chat_prompt as BuiltinCommand);
    builtin.insert("add_path", add_path::command as BuiltinCommand);
    builtin.insert("uuid", uuid::command as BuiltinCommand);

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

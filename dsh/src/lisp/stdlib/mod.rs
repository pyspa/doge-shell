pub mod core;
pub mod io;
pub mod list;
pub mod math;
pub mod mcp;
pub mod string;
pub mod table;

use crate::lisp::model::Env;

pub fn register(env: &mut Env) {
    core::register(env);
    io::register(env);
    list::register(env);
    math::register(env);
    mcp::register(env);
    string::register(env);
    table::register(env);
}

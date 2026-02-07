pub mod io;
pub mod math;
pub mod string;
// pub mod mcp; // Will add later
// pub mod core; // Will add later

use crate::lisp::model::Env;

pub fn register(env: &mut Env) {
    io::register(env);
    math::register(env);
    string::register(env);
}

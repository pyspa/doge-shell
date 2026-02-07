use crate::lisp::model::{Env, Symbol, Value};
use crate::lisp::utils::require_arg;

pub fn register(env: &mut Env) {
    env.define(
        Symbol::from("print"),
        Value::NativeFunc(|_env, args| {
            let expr = require_arg("print", &args, 0)?;

            println!("{}", &expr);
            Ok(expr.clone())
        }),
    );
}

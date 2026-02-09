use crate::lisp::model::{Env, Symbol, Value};
use crate::lisp::utils::require_arg;

pub fn register(env: &mut Env) {
    env.define(
        Symbol::from("is_null"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_null", &args, 0)?;

            Ok(Value::from(*val == Value::NIL))
        }),
    );

    env.define(
        Symbol::from("is_symbol"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_symbol", &args, 0)?;

            Ok(match val {
                Value::Symbol(_) => Value::True,
                _ => Value::NIL,
            })
        }),
    );

    env.define(
        Symbol::from("is_boolean"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_boolean", &args, 0)?;

            Ok(match val {
                Value::True => Value::True,
                Value::False => Value::True,
                _ => Value::NIL,
            })
        }),
    );

    env.define(
        Symbol::from("is_procedure"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_procedure", &args, 0)?;

            Ok(match val {
                Value::Lambda(_) => Value::True,
                Value::NativeFunc(_) => Value::True,
                _ => Value::NIL,
            })
        }),
    );

    env.define(
        Symbol::from("is_pair"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_pair", &args, 0)?;

            Ok(match val {
                Value::List(_) => Value::True,
                _ => Value::NIL,
            })
        }),
    );

    env.define(
        Symbol::from("is_table"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_table", &args, 0)?;
            Ok(match val {
                Value::Table(_) => Value::True,
                _ => Value::NIL,
            })
        }),
    );
}

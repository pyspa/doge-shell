use crate::lisp::model::{Env, IntType, RuntimeError, Symbol, Value};
use crate::lisp::utils::{require_arg, require_typed_arg};

use std::convert::TryInto;

pub fn register(env: &mut Env) {
    env.define(
        Symbol::from("is_number"),
        Value::NativeFunc(|_env, args| {
            let val = require_arg("is_number", &args, 0)?;

            Ok(match val {
                Value::Int(_) => Value::True,
                Value::Float(_) => Value::True,
                _ => Value::NIL,
            })
        }),
    );

    env.define(
        Symbol::from("+"),
        Value::NativeFunc(|_env, args| {
            let first_arg = require_arg("+", &args, 1)?;

            let mut total = match first_arg {
                Value::Int(_) => Ok(Value::Int(IntType::default())),
                Value::Float(_) => Ok(Value::Float(0.0)),
                Value::String(_) => Ok(Value::String("".into())),
                _ => Err(RuntimeError {
                    msg: format!(
                        "Function \"+\" requires arguments to be numbers or strings; found {first_arg}"
                    ),
                }),
            }?;

            for arg in args {
                total = (&total + &arg).map_err(|_| RuntimeError {
                    msg: format!(
                        "Function \"+\" requires arguments to be numbers or strings; found {arg}"
                    ),
                })?;
            }

            Ok(total)
        }),
    );

    env.define(
        Symbol::from("-"),
        Value::NativeFunc(|_env, args| {
            let a = require_arg("-", &args, 0)?;
            let b = require_arg("-", &args, 1)?;

            (a - b).map_err(|_| RuntimeError {
                msg: String::from("Function \"-\" requires arguments to be numbers"),
            })
        }),
    );

    env.define(
        Symbol::from("*"),
        Value::NativeFunc(|_env, args| {
            let mut product = Value::Int(IntType::from(1));

            for arg in args {
                product = (&product * &arg).map_err(|_| RuntimeError {
                    msg: format!("Function \"*\" requires arguments to be numbers; found {arg}"),
                })?;
            }

            Ok(product)
        }),
    );

    env.define(
        Symbol::from("/"),
        Value::NativeFunc(|_env, args| {
            let a = require_arg("/", &args, 0)?;
            let b = require_arg("/", &args, 1)?;

            (a / b).map_err(|_| RuntimeError {
                msg: String::from("Function \"/\" requires arguments to be numbers"),
            })
        }),
    );

    env.define(
        Symbol::from("truncate"),
        Value::NativeFunc(|_env, args| {
            let a = require_arg("truncate", &args, 0)?;
            let b = require_arg("truncate", &args, 1)?;

            if let (Ok(a), Ok(b)) = (
                TryInto::<IntType>::try_into(a),
                TryInto::<IntType>::try_into(b),
            ) {
                return Ok(Value::Int(a / b));
            }

            Err(RuntimeError {
                msg: String::from("Function \"truncate\" requires arguments to be integers"),
            })
        }),
    );

    env.define(
        Symbol::from("range"),
        Value::NativeFunc(|_env, args| {
            let start = require_typed_arg::<IntType>("range", &args, 0)?;
            let end = require_typed_arg::<IntType>("range", &args, 1)?;

            let mut current = start;

            Ok(Value::List(
                std::iter::from_fn(move || {
                    if current == end {
                        None
                    } else {
                        #[cfg(feature = "bigint")]
                        let res = Some(current.clone());
                        #[cfg(not(feature = "bigint"))]
                        let res = Some(current);

                        current += IntType::from(1);

                        res
                    }
                })
                .map(Value::from)
                .collect(),
            ))
        }),
    );
}

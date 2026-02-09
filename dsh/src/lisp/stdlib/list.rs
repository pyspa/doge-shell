use crate::lisp::model::{Env, IntType, List, RuntimeError, Symbol, Value};
use crate::lisp::utils::{require_arg, require_typed_arg};
use std::convert::TryInto;

pub fn register(env: &mut Env) {
    env.define(
        Symbol::from("car"),
        Value::NativeFunc(|_env, args| {
            let list = require_typed_arg::<&List>("car", &args, 0)?;

            list.car()
        }),
    );

    env.define(
        Symbol::from("cdr"),
        Value::NativeFunc(|_env, args| {
            let list = require_typed_arg::<&List>("cdr", &args, 0)?;

            Ok(Value::List(list.cdr()))
        }),
    );

    env.define(
        Symbol::from("cons"),
        Value::NativeFunc(|_env, args| {
            let car = require_arg("cons", &args, 0)?;
            let cdr = require_typed_arg::<&List>("cons", &args, 1)?;

            Ok(Value::List(cdr.cons(car.clone())))
        }),
    );

    env.define(
        Symbol::from("list"),
        Value::NativeFunc(|_env, args| Ok(Value::List(args.iter().collect::<List>()))),
    );

    env.define(
        Symbol::from("nth"),
        Value::NativeFunc(|_env, args| {
            let index = require_typed_arg::<IntType>("nth", &args, 0)?;
            let list = require_typed_arg::<&List>("nth", &args, 1)?;

            let index = TryInto::<usize>::try_into(index).map_err(|_| RuntimeError {
                msg: "Failed converting to `usize`".to_owned(),
            })?;

            Ok(list.into_iter().nth(index).unwrap_or(Value::NIL))
        }),
    );

    env.define(
        Symbol::from("sort"),
        Value::NativeFunc(|_env, args| {
            let list = require_typed_arg::<&List>("sort", &args, 0)?;

            let mut v: Vec<Value> = list.into_iter().collect();

            v.sort();

            Ok(Value::List(v.into_iter().collect()))
        }),
    );

    env.define(
        Symbol::from("reverse"),
        Value::NativeFunc(|_env, args| {
            let list = require_typed_arg::<&List>("reverse", &args, 0)?;

            let mut v: Vec<Value> = list.into_iter().collect();

            v.reverse();

            Ok(Value::List(v.into_iter().collect()))
        }),
    );
}

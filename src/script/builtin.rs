use crate::config;
use rust_lisp::default_env;
use rust_lisp::model::{Env, ForeignValue, RuntimeError, Symbol, Value};
use rust_lisp::utils::require_typed_arg;
use std::convert::From;
use std::process::Command;
use std::{cell::RefCell, rc::Rc};

#[derive(Debug)]
struct ConfigWrapper {
    pub config: Rc<RefCell<config::Config>>,
}

impl ForeignValue for ConfigWrapper {
    fn command(
        &mut self,
        _env: Rc<RefCell<Env>>,
        command: &str,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        match command {
            "get-alias" => {
                let alias = require_typed_arg::<&String>("get-alias", args, 0)?;
                if let Some(v) = self.config.borrow().alias.get(alias) {
                    Ok(Value::String(v.to_string()))
                } else {
                    Ok(Value::NIL)
                }
            }
            "set-alias" => {
                let alias = require_typed_arg::<&String>("set-alias", args, 0)?;
                let command = require_typed_arg::<&String>("set-alias", args, 1)?;
                if let Some(cmd) = self
                    .config
                    .borrow_mut()
                    .alias
                    .insert(alias.to_string(), command.to_string())
                {
                    Ok(Value::String(cmd))
                } else {
                    Ok(Value::NIL)
                }
            }
            _ => Err(RuntimeError {
                msg: format!("Unexpected command {}", command),
            }),
        }
    }
}

pub fn make_env(config: Rc<RefCell<config::Config>>) -> Rc<RefCell<Env>> {
    let env = Rc::new(RefCell::new(default_env()));

    let wrapper = Rc::new(RefCell::new(ConfigWrapper { config }));

    // add global object
    env.borrow_mut()
        .define(Symbol::from("config"), Value::Foreign(wrapper));

    // add builtin functions
    env.borrow_mut()
        .define(Symbol::from("debug"), Value::NativeFunc(print));
    env.borrow_mut()
        .define(Symbol::from("print"), Value::NativeFunc(print));
    env.borrow_mut()
        .define(Symbol::from("alias"), Value::NativeFunc(alias));
    env.borrow_mut()
        .define(Symbol::from("command"), Value::NativeFunc(command));
    // TODO add shell env
    env
}

fn print(_env: Rc<RefCell<Env>>, args: &[Value]) -> Result<Value, RuntimeError> {
    args.iter().for_each(|x| println!("print: {:?}", x));
    Ok(Value::True)
}

fn alias(env: Rc<RefCell<Env>>, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Some(Value::Foreign(val)) = env.borrow_mut().get(&Symbol::from("config")) {
        val.borrow_mut().command(env.clone(), "set-alias", args)
    } else {
        Ok(Value::NIL)
    }
}

fn command(_env: Rc<RefCell<Env>>, args: &[Value]) -> Result<Value, RuntimeError> {
    let mut cmd_args: Vec<String> = Vec::new();
    for arg in args {
        if let Value::String(val) = arg {
            val.split_whitespace()
                .for_each(|x| cmd_args.push(x.to_string()));
        }
    }

    let cmd = cmd_args.remove(0);

    match Command::new(cmd).args(cmd_args).output() {
        Ok(output) => {
            if output.status.success() {
                let stdout = String::from_utf8(output.stdout).expect("fail get stdout");

                // let mut out: Vec<String> = Vec::new();
                // stdout.split('\n').for_each(|x| out.push(x.to_string()));
                // match make_list(out) {
                //     Ok(lst) => Ok(lst),
                //     Err(err) => Err(RuntimeError {
                //         msg: err.to_string(),
                //     }),
                // }

                Ok(Value::String(stdout))
            } else {
                Ok(Value::NIL)
            }
        }
        Err(err) => Err(RuntimeError {
            msg: err.to_string(),
        }),
    }
}

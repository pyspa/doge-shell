use super::util;
use rust_lisp::model::{Env, RuntimeError, Symbol, Value};
use std::convert::From;
use std::process::Command;
use std::{cell::RefCell, rc::Rc};

pub fn alias(env: Rc<RefCell<Env>>, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Some(Value::Foreign(val)) = env.borrow_mut().get(&Symbol::from("engine")) {
        val.borrow_mut().command(env.clone(), "set-alias", args)
    } else {
        Ok(Value::NIL)
    }
}

pub fn command(_env: Rc<RefCell<Env>>, args: &[Value]) -> Result<Value, RuntimeError> {
    let mut cmd_args: Vec<String> = Vec::new();
    for arg in args {
        if let Value::String(val) = arg {
            cmd_args.push(val.to_string());
        }
    }
    let cmd = cmd_args.remove(0);
    match Command::new(cmd).args(cmd_args).output() {
        Ok(output) => {
            let stdout = String::from_utf8(output.stdout)
                .expect("fail get stdout")
                .trim_end()
                .to_string();

            let stderr = String::from_utf8(output.stderr)
                .expect("fail get stdout")
                .trim_end()
                .to_string();

            if !stdout.is_empty() {
                Ok(Value::String(stdout))
            } else {
                Ok(Value::String(stderr))
            }
        }
        Err(err) => Err(RuntimeError {
            msg: err.to_string(),
        }),
    }
}

pub fn sh(_env: Rc<RefCell<Env>>, args: &[Value]) -> Result<Value, RuntimeError> {
    let mut cmd_args: Vec<String> = Vec::new();
    cmd_args.push("-c".to_string());
    for arg in args {
        let val = arg.to_string();
        cmd_args.push(util::unquote(val.as_str()));
    }
    // TODO use own shell
    match Command::new("sh").args(cmd_args).output() {
        Ok(output) => {
            let stdout = String::from_utf8(output.stdout)
                .expect("fail get stdout")
                .trim_end()
                .to_string();

            let stderr = String::from_utf8(output.stderr)
                .expect("fail get stdout")
                .trim_end()
                .to_string();

            if !stdout.is_empty() {
                Ok(Value::String(stdout))
            } else {
                Ok(Value::String(stderr))
            }
        }
        Err(err) => Err(RuntimeError {
            msg: err.to_string(),
        }),
    }
}

#[cfg(test)]
mod test {

    use super::*;
    use crate::environment::Environment;
    use crate::script::LispEngine;

    fn init() {
        tracing_subscriber::fmt::init();
    }

    #[test]
    fn test_lisp_sh() {
        let env = Environment::new();
        let engine = LispEngine::new(env);

        let args = vec![Value::String("ls -al".to_string())];
        let res = sh(Rc::clone(&engine.borrow().env), args.as_slice());
        assert!(res.is_ok());
        println!("{}", res.unwrap());

        // let args = vec![Value::String("cargo build".to_string())];
        // let res = sh(Rc::clone(&engine.borrow().env), args.as_slice());
        // assert!(res.is_ok());
        // println!("{}", res.unwrap());
    }
}

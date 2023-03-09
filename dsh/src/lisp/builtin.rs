use crate::direnv::DirEnvironment;
use crate::lisp::model::{Env, RuntimeError, Value};
use crate::shell::Shell;
use dsh_types::Context;
use nix::sys::termios::tcgetattr;
use nix::unistd::pipe;
use std::fs::File;
use std::io::prelude::*;
use std::os::unix::io::FromRawFd;
use std::process::Command;
use std::{cell::RefCell, rc::Rc};
use tracing::debug;

pub fn set_variable(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let key = &args[0];
    let val = &args[1];
    debug!("set variable {} {}", &key, &val);
    env.borrow()
        .shell_env
        .borrow_mut()
        .variables
        .insert(key.to_string(), val.to_string());
    Ok(Value::NIL)
}

pub fn alias(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let alias = &args[0];
    let command = &args[1];
    env.borrow()
        .shell_env
        .borrow_mut()
        .alias
        .insert(alias.to_string(), command.to_string());
    Ok(Value::NIL)
}

pub fn allow_direnv(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    for arg in args {
        let root = arg.to_string();
        let root = shellexpand::tilde(root.as_str());

        env.borrow()
            .shell_env
            .borrow_mut()
            .direnv_roots
            .push(DirEnvironment::new(root.to_string()));
    }
    Ok(Value::NIL)
}

pub fn command(_env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
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

pub fn sh(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let mut cmd_args: Vec<String> = Vec::new();

    for arg in args {
        let val = arg.to_string();
        cmd_args.push(val);
    }
    let input = cmd_args.join(" ");

    debug!("var {:?}", &env.borrow().shell_env.borrow().variables);
    let mut shell = Shell::new(Rc::clone(&env.borrow().shell_env));
    shell.set_signals();
    let shell_tmode = match tcgetattr(0) {
        Ok(tmode) => tmode,
        Err(err) => {
            eprintln!("error: {}", err);
            return Err(RuntimeError {
                msg: err.to_string(),
            });
        }
    };

    let mut ctx = Context::new(shell.pid, shell.pgid, shell_tmode, true);
    let (pout, pin) = match pipe() {
        Ok(p) => p,
        Err(err) => {
            eprintln!("error: {}", err);
            return Err(RuntimeError {
                msg: err.to_string(),
            });
        }
    };

    ctx.captured_out = Some(pin);
    if let Err(err) = shell.eval_str(ctx, input, false) {
        eprintln!("error: {}", err);
        return Err(RuntimeError {
            msg: err.to_string(),
        });
    }

    let mut raw_stdout = Vec::new();
    unsafe { File::from_raw_fd(pout).read_to_end(&mut raw_stdout).ok() };

    let output = match std::str::from_utf8(&raw_stdout) {
        Ok(str) => str.trim_matches('\n').to_owned(),
        Err(err) => {
            eprintln!("error: {}", err);
            return Err(RuntimeError {
                msg: err.to_string(),
            });
        }
    };
    debug!("'{}'", output);
    Ok(Value::String(output))
}

#[cfg(test)]
mod test {

    use super::*;
    use crate::environment::Environment;
    use crate::lisp::LispEngine;

    fn init() {
        tracing_subscriber::fmt::init();
    }

    #[test]
    fn test_lisp_sh() {
        let env = Environment::new();
        let engine = LispEngine::new(env);

        let args = vec![Value::String("ls -al".to_string())];
        let res = sh(Rc::clone(&engine.borrow().env), args.to_vec());
        assert!(res.is_ok());
        println!("{}", res.unwrap());

        // let args = vec![Value::String("cargo build".to_string())];
        // let res = sh(Rc::clone(&engine.borrow().env), args.as_slice());
        // assert!(res.is_ok());
        // println!("{}", res.unwrap());
    }
}

use rust_lisp::model::{Env, RuntimeError, Symbol, Value};
use std::convert::From;
use std::process::Command;
use std::{cell::RefCell, rc::Rc};
pub fn print(_env: Rc<RefCell<Env>>, args: &[Value]) -> Result<Value, RuntimeError> {
    args.iter().for_each(|x| println!("print: {:?}", x));
    Ok(Value::True)
}

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

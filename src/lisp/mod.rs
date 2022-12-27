use crate::environment::{self, Environment};
use crate::lisp::default_environment::default_env;
use crate::lisp::interpreter::eval;
use crate::lisp::model::{Env, List, RuntimeError, Symbol, Value};
use crate::lisp::parser::parse;
use crate::lisp::utils::require_typed_arg;
use crate::shell::APP_NAME;
use anyhow::Context as _;
use std::{cell::RefCell, rc::Rc};

mod builtin;
mod default_environment;
mod interpreter;
mod macros;
mod model;
mod parser;
mod utils;

pub const CONFIG_FILE: &str = "config.lisp";

#[derive(Debug)]
pub struct LispEngine {
    pub env: Rc<RefCell<Env>>,
    shell_env: Rc<RefCell<Environment>>,
}

impl LispEngine {
    pub fn new(shell_env: Rc<RefCell<Environment>>) -> Rc<RefCell<Self>> {
        let env = make_env(Rc::clone(&shell_env));
        let engine = Rc::new(RefCell::new(LispEngine {
            shell_env: Rc::clone(&shell_env),
            env: Rc::clone(&env),
        }));

        engine
    }

    pub fn run_config_lisp(&self) -> anyhow::Result<()> {
        let xdg_dir =
            xdg::BaseDirectories::with_prefix(APP_NAME).context("failed get xdg directory")?;
        let file_path = xdg_dir
            .place_config_file(CONFIG_FILE)
            .context("failed get path")?;
        let config_lisp: String = std::fs::read_to_string(file_path)?.trim().to_string();
        let _ = self.run(format!("(begin {} )", config_lisp).as_str());
        Ok(())
    }

    pub fn run(&self, src: &str) -> anyhow::Result<Value> {
        let mut ast_iter = parse(src);

        if let Some(expr) = ast_iter.next() {
            match expr {
                Ok(expr) => {
                    let res = eval(Rc::clone(&self.env), &expr)?;
                    return Ok(res);
                }
                Err(err) => {
                    eprintln!("{}", err)
                }
            }
        }
        // TODO return value
        Ok(Value::NIL)
    }

    pub fn run_func(&self, name: &str, args: Vec<String>) -> anyhow::Result<Value> {
        // to args
        let args: Vec<Value> = args.iter().map(|x| Value::String(x.to_string())).collect();
        // apply
        self.run_func_values(name, args)
    }

    pub fn run_func_values(&self, name: &str, args: Vec<Value>) -> anyhow::Result<Value> {
        // get func
        let func = self.run(name)?;
        // apply
        let res = func.apply(self.env.clone(), args)?;

        Ok(res)
    }

    pub fn has(&self, name: &str) -> bool {
        if let Ok(v) = self.run(name) {
            v != Value::NIL
        } else {
            false
        }
    }
}

pub fn make_env(environment: Rc<RefCell<Environment>>) -> Rc<RefCell<Env>> {
    let env = Rc::new(RefCell::new(default_env(environment)));

    // add builtin functions
    env.borrow_mut()
        .define(Symbol::from("alias"), Value::NativeFunc(builtin::alias));
    env.borrow_mut()
        .define(Symbol::from("command"), Value::NativeFunc(builtin::command));
    env.borrow_mut()
        .define(Symbol::from("sh"), Value::NativeFunc(builtin::sh));

    // TODO add shell env
    env
}

trait Applicable {
    fn apply(&self, env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError>;
}

impl Applicable for Value {
    fn apply(&self, env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
        match self {
            val @ Value::Lambda(_) => {
                let params = List::from_iter(args);
                eval(env, &Value::List(params.cons(val.clone())))
            }
            _ => Ok(Value::NIL),
        }
    }
}

#[cfg(test)]
mod test {

    use super::*;

    fn init() {
        tracing_subscriber::fmt::init();
    }

    #[test]
    fn test_run_lisp() {
        let env = Environment::new();
        let engine = LispEngine::new(env);
        let _res = engine.borrow().run("(alias \"e\" \"emacs\")");
    }

    #[test]
    fn test_apply_fn() {
        let env = Environment::new();
        let engine = LispEngine::new(env);
        let res = engine.borrow().run(
            "
(begin
  (defun log (str)
    (print str)))",
        );
        assert!(res.is_ok());

        let func = engine.borrow().run("log").unwrap();
        let args = vec![Value::String("abcdefg".to_owned())];
        let res = func.apply(engine.borrow().env.clone(), args);
        assert!(res.is_ok());
    }

    #[test]
    fn test_call_fn() {
        let env = Environment::new();
        let engine = LispEngine::new(env);
        let res = engine.borrow().run(
            "
(begin
  (defun log (str)
    (print str))
  (defun adder (x y)
    (+ x y))
  (defun call ()
    (sh \"ls -al\"))
)
",
        );
        assert!(res.is_ok());

        let args = vec!["abcdefg".to_string()];
        let res = engine.borrow().run_func("log", args);
        assert!(res.is_ok());

        let args = vec![Value::Int(1), Value::Int(2)];
        let res = engine.borrow().run_func_values("adder", args);
        assert!(res.is_ok());
        println!("{:?}", res);

        let args = vec![];
        let res = engine.borrow().run_func_values("call", args);
        assert!(res.is_ok());
        println!("{:?}", res);
    }
}

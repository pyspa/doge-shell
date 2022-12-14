use crate::environment::Environment;
use crate::shell::APP_NAME;
use anyhow::Context as _;
use rust_lisp::default_env;
use rust_lisp::interpreter::eval;
use rust_lisp::model::{Env, ForeignValue, List, RuntimeError, Symbol, Value};
use rust_lisp::parser::parse;
use rust_lisp::utils::require_typed_arg;
use std::{cell::RefCell, rc::Rc};

mod builtin;
mod util;

pub const CONFIG_FILE: &str = "config.lisp";

#[derive(Debug)]
pub struct LispEngine {
    pub env: Rc<RefCell<Env>>,
    environment: Rc<RefCell<Environment>>,
}

impl LispEngine {
    pub fn new(environment: Rc<RefCell<Environment>>) -> Rc<RefCell<Self>> {
        let env = make_env();
        let engine = Rc::new(RefCell::new(LispEngine {
            environment,
            env: Rc::clone(&env),
        }));

        let wrapper = Rc::new(RefCell::new(Wrapper {
            engine: Rc::clone(&engine),
        }));

        // add global object
        // set self
        env.borrow_mut()
            .define(Symbol::from("engine"), Value::Foreign(wrapper));

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

#[derive(Debug)]
struct Wrapper {
    engine: Rc<RefCell<LispEngine>>,
}

impl ForeignValue for Wrapper {
    fn command(
        &mut self,
        _env: Rc<RefCell<Env>>,
        command: &str,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        match command {
            "get-alias" => {
                let alias = require_typed_arg::<&String>("get-alias", args, 0)?;
                if let Some(v) = self.engine.borrow().environment.borrow().alias.get(alias) {
                    Ok(Value::String(v.to_string()))
                } else {
                    Ok(Value::NIL)
                }
            }
            "set-alias" => {
                let alias = require_typed_arg::<&String>("set-alias", args, 0)?;
                let command = require_typed_arg::<&String>("set-alias", args, 1)?;
                if let Some(cmd) = self
                    .engine
                    .borrow()
                    .environment
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

pub fn make_env() -> Rc<RefCell<Env>> {
    let env = Rc::new(RefCell::new(default_env()));

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

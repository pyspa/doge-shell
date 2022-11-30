use crate::config;
use crate::shell::APP_NAME;
use anyhow::Context as _;
use log::debug;
use rust_lisp::default_env;
use rust_lisp::interpreter::eval;
use rust_lisp::model::{Env, ForeignValue, RuntimeError, Symbol, Value};
use rust_lisp::parser::parse;
use rust_lisp::utils::require_typed_arg;
use std::collections::HashMap;
use std::{cell::RefCell, rc::Rc};

mod builtin;
mod util;

pub const CONFIG_FILE: &str = "config.lisp";

#[derive(Debug)]
pub struct LispEngine {
    config: Rc<RefCell<config::Config>>,
    defuns: HashMap<String, Value>,
    env: Rc<RefCell<Env>>,
}

impl LispEngine {
    pub fn new(config: Rc<RefCell<config::Config>>) -> Rc<RefCell<Self>> {
        let env = make_env();
        let defuns: HashMap<String, Value> = HashMap::new();
        let engine = Rc::new(RefCell::new(LispEngine {
            config,
            defuns,
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

    pub fn run(&self, src: &str) -> anyhow::Result<()> {
        let mut ast_iter = parse(src);

        if let Some(expr) = ast_iter.next() {
            match expr {
                Ok(expr) => {
                    let res = eval(Rc::clone(&self.env), &expr)?;
                    debug!("res {:?}", res);
                }
                Err(err) => {
                    eprintln!("{}", err)
                }
            }
        }
        // TODO return value
        Ok(())
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
                if let Some(v) = self.engine.borrow().config.borrow().alias.get(alias) {
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

pub fn make_env() -> Rc<RefCell<Env>> {
    let env = Rc::new(RefCell::new(default_env()));

    // add builtin functions
    env.borrow_mut()
        .define(Symbol::from("debug"), Value::NativeFunc(builtin::print));
    env.borrow_mut()
        .define(Symbol::from("print"), Value::NativeFunc(builtin::print));
    env.borrow_mut()
        .define(Symbol::from("alias"), Value::NativeFunc(builtin::alias));
    env.borrow_mut()
        .define(Symbol::from("command"), Value::NativeFunc(builtin::command));
    // TODO add shell env
    env
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn test_run_lisp() {
        let _ = env_logger::try_init();
        let config: Rc<RefCell<config::Config>> = Rc::new(RefCell::new(Default::default()));
        config
            .borrow_mut()
            .alias
            .insert("test".to_owned(), "value".to_owned());

        let engine = LispEngine::new(config);
        let _res = engine.borrow_mut().run("(alias \"e\" \"emacs\")");
    }
}

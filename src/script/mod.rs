use crate::config;
use crate::shell::APP_NAME;
use anyhow::Context as _;
use log::debug;
use rust_lisp::default_env;
use rust_lisp::interpreter::eval;
use rust_lisp::model::{Env, ForeignValue, RuntimeError, Symbol, Value};
use rust_lisp::parser::{parse, ParseError};
use rust_lisp::utils::require_typed_arg;
use std::convert::From;
use std::{cell::RefCell, rc::Rc};

type NativeFunc = fn(env: Rc<RefCell<Env>>, args: &Vec<Value>) -> Result<Value, RuntimeError>;

pub fn read_init_file(config: Rc<RefCell<config::Config>>) -> anyhow::Result<()> {
    let xdg_dir =
        xdg::BaseDirectories::with_prefix(APP_NAME).context("failed get xdg directory")?;
    let file_path = xdg_dir
        .place_config_file("init.lisp")
        .context("failed get path")?;
    let init_lisp: String = std::fs::read_to_string(file_path)?.trim().to_string();
    let _ = run(config, format!("(begin {} )", init_lisp).as_str());
    Ok(())
}

#[derive(Debug)]
struct ConfigWrapper {
    pub config: Rc<RefCell<config::Config>>,
}

impl ForeignValue for ConfigWrapper {
    fn command(
        &mut self,
        env: Rc<RefCell<Env>>,
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

fn alias(env: Rc<RefCell<Env>>, args: &[Value]) -> Result<Value, RuntimeError> {
    if let Some(Value::Foreign(val)) = env.borrow_mut().get(&Symbol::from("config")) {
        val.borrow_mut().command(env.clone(), "set-alias", args)
    } else {
        Ok(Value::NIL)
    }
}

fn make_env(config: Rc<RefCell<config::Config>>) -> Rc<RefCell<Env>> {
    let env = Rc::new(RefCell::new(default_env()));

    let wrapper = Rc::new(RefCell::new(ConfigWrapper { config }));

    env.borrow_mut()
        .define(Symbol::from("config"), Value::Foreign(wrapper));
    env.borrow_mut()
        .define(Symbol::from("alias"), Value::NativeFunc(alias));
    // TODO add shell env
    env
}

fn make_list(vec: Vec<String>) -> Result<Value, ParseError> {
    let mut buf = String::new();
    let list: Vec<String> = vec.iter().map(|x| format!("\"{}\"", x)).collect();
    let list = list.join(" ");
    buf.push('(');
    buf.push_str(list.as_str());
    buf.push(')');
    let x = parse(buf.as_str()).next().unwrap();
    x
}

pub fn run(config: Rc<RefCell<config::Config>>, src: &str) -> anyhow::Result<()> {
    let env = make_env(config);

    let mut ast_iter = parse(src);

    if let Some(expr) = ast_iter.next() {
        match expr {
            Ok(expr) => {
                let res = eval(env, &expr)?;
                debug!("res {:?}", res);
            }
            Err(err) => {
                eprintln!("{}", err)
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn test_make_list() {
        let args = make_list(vec!["aaa".to_string(), "bbb".to_string()]).ok();
        println!("list {:?}", args);
    }

    #[test]
    fn test_run_lisp() {
        let _ = env_logger::try_init();
        let config: Rc<RefCell<config::Config>> = Rc::new(RefCell::new(Default::default()));
        config
            .borrow_mut()
            .alias
            .insert("test".to_owned(), "value".to_owned());

        let res = run(config, "(alias \"e\" \"emacs\")");
    }
}

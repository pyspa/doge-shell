use crate::config;
use crate::shell::APP_NAME;
use anyhow::Context as _;
use log::debug;
use rust_lisp::interpreter::eval;
use rust_lisp::model::Env;
use rust_lisp::parser::parse;
use std::{cell::RefCell, rc::Rc};

mod builtin;
mod util;

pub const CONFIG_FILE: &str = "config.lisp";

#[derive(Debug)]
pub struct LispEngine {
    env: Rc<RefCell<Env>>,
}

impl LispEngine {
    pub fn new(config: Rc<RefCell<config::Config>>) -> Self {
        let env = builtin::make_env(Rc::clone(&config));
        Self { env }
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
        let _res = engine.run("(alias \"e\" \"emacs\")");
    }
}

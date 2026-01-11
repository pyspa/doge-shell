use crate::environment::{self, Environment};
use crate::lisp::default_environment::default_env;
use crate::lisp::interpreter::eval;
#[cfg(test)]
use crate::lisp::model::IntType;
pub use crate::lisp::model::Value;
pub use crate::lisp::model::{Env, Symbol};
use crate::lisp::model::{List, RuntimeError};
use crate::lisp::parser::parse;
use anyhow::Context;
use parking_lot::RwLock;
use std::sync::Arc;
use std::{cell::RefCell, rc::Rc};

mod builtin;
mod command_palette;
mod default_environment;
mod interpreter;
mod macros;
#[cfg(test)]
mod mcp_tests;
mod model;
mod parser;
mod utils;

pub const CONFIG_FILE: &str = "config.lisp";

#[derive(Debug)]
pub struct LispEngine {
    pub env: Rc<RefCell<Env>>,
    pub shell_env: Arc<RwLock<Environment>>,
}

impl LispEngine {
    pub fn new(shell_env: Arc<RwLock<Environment>>) -> Rc<RefCell<Self>> {
        let env = make_env(Arc::clone(&shell_env));
        Rc::new(RefCell::new(LispEngine {
            shell_env: Arc::clone(&shell_env),
            env: Rc::clone(&env),
        }))
    }

    pub fn run_config_lisp(&self) -> anyhow::Result<()> {
        let file_path = environment::get_config_file(CONFIG_FILE)?;
        let config_lisp: String = std::fs::read_to_string(&file_path)
            .with_context(|| format!("Failed to read config file: {}", file_path.display()))?
            .trim()
            .to_string();

        self.shell_env.write().clear_mcp_servers();

        let wrapped_config = format!("(begin {config_lisp}\n)");
        match self.run(&wrapped_config) {
            Ok(_) => {
                tracing::debug!("Successfully loaded config.lisp");
                Ok(())
            }
            Err(e) => {
                tracing::error!("Failed to execute config.lisp: {}", e);
                Err(e)
            }
        }
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
                    tracing::error!("Lisp parse error: {}", err);
                    return Err(anyhow::anyhow!("Parse error: {}", err));
                }
            }
        }
        // Return NIL if no expressions were evaluated
        Ok(Value::NIL)
    }

    pub fn run_func(&self, name: &str, args: Vec<String>) -> anyhow::Result<Value> {
        // to args
        let mut args: Vec<Value> = args.iter().map(|x| Value::String(x.to_string())).collect();
        // get func
        let func = self.run(name)?;
        if let Value::Lambda(lambda) = func {
            while lambda.argnames.len() > args.len() {
                args.push(Value::String("".to_string()));
            }
        }
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

    pub fn apply_func(&self, func: Value, args: Vec<Value>) -> anyhow::Result<Value> {
        // apply
        let res = func.apply(self.env.clone(), args)?;

        Ok(res)
    }

    /// Execute all functions in a hook list safely
    #[allow(dead_code)]
    pub fn execute_hook_list(&self, hook_list: &Value) -> anyhow::Result<()> {
        use crate::lisp::model::Value;
        use tracing::warn;

        if let Value::List(list) = hook_list {
            // Iterate through the hook list and execute each function
            for hook_func in list.into_iter() {
                match self.apply_func(hook_func.clone(), vec![]) {
                    Ok(_) => {
                        // Hook executed successfully
                    }
                    Err(e) => {
                        warn!("Hook function execution failed: {}", e);
                        // Continue with other hooks even if one fails
                    }
                }
            }
        }
        Ok(())
    }

    /// Get a hook list by name
    #[allow(dead_code)]
    pub fn get_hook_list(&self, hook_name: &str) -> anyhow::Result<Value> {
        let full_name = format!("*{}*", hook_name);
        match self.run(&full_name) {
            Ok(value) => Ok(value),
            Err(e) => {
                tracing::warn!("Failed to retrieve hook {}: {}", hook_name, e);
                Ok(Value::NIL) // Return empty list if hook doesn't exist
            }
        }
    }

    #[allow(dead_code)]
    pub fn has(&self, name: &str) -> bool {
        if let Ok(v) = self.run(name) {
            v != Value::NIL
        } else {
            false
        }
    }

    pub fn is_export(&self, name: &str) -> bool {
        if let Ok(Value::Lambda(l)) = self.run(name) {
            l.export
        } else {
            false
        }
    }
}

pub fn make_env(environment: Arc<RwLock<Environment>>) -> Rc<RefCell<Env>> {
    let env = Rc::new(RefCell::new(default_env(environment)));

    // add builtin functions
    env.borrow_mut()
        .define(Symbol::from("alias"), Value::NativeFunc(builtin::alias));
    env.borrow_mut()
        .define(Symbol::from("abbr"), Value::NativeFunc(builtin::abbr));
    env.borrow_mut()
        .define(Symbol::from("command"), Value::NativeFunc(builtin::command));
    env.borrow_mut()
        .define(Symbol::from("sh!"), Value::NativeFunc(builtin::block_sh));
    env.borrow_mut().define(
        Symbol::from("sh"),
        Value::NativeFunc(builtin::block_sh_no_cap),
    );
    env.borrow_mut().define(
        Symbol::from("allow-direnv"),
        Value::NativeFunc(builtin::allow_direnv),
    );
    env.borrow_mut().define(
        Symbol::from("vset"),
        Value::NativeFunc(builtin::set_variable),
    );
    env.borrow_mut().define(
        Symbol::from("add_path"),
        Value::NativeFunc(builtin::add_path),
    );
    env.borrow_mut()
        .define(Symbol::from("setenv"), Value::NativeFunc(builtin::set_env));
    env.borrow_mut().define(
        Symbol::from("safety-level"),
        Value::NativeFunc(builtin::safety_level),
    );
    env.borrow_mut().define(
        Symbol::from("pref-auto-pair"),
        Value::NativeFunc(builtin::pref_auto_pair),
    );
    env.borrow_mut().define(
        Symbol::from("pref-auto-notify"),
        Value::NativeFunc(builtin::pref_auto_notify),
    );

    // Secret management functions
    env.borrow_mut().define(
        Symbol::from("secret-add-pattern"),
        Value::NativeFunc(builtin::secret_add_pattern),
    );
    env.borrow_mut().define(
        Symbol::from("secret-add-keyword"),
        Value::NativeFunc(builtin::secret_add_keyword),
    );
    env.borrow_mut().define(
        Symbol::from("secret-list-patterns"),
        Value::NativeFunc(builtin::secret_list_patterns),
    );
    env.borrow_mut().define(
        Symbol::from("secret-history-mode"),
        Value::NativeFunc(builtin::secret_history_mode),
    );
    env.borrow_mut().define(
        Symbol::from("secret-set"),
        Value::NativeFunc(builtin::secret_set),
    );
    env.borrow_mut().define(
        Symbol::from("secret-get"),
        Value::NativeFunc(builtin::secret_get),
    );
    env.borrow_mut().define(
        Symbol::from("secret-clear"),
        Value::NativeFunc(builtin::secret_clear),
    );

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
mod tests {

    use super::*;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[test]
    fn test_run_lisp() {
        init();
        let env = Environment::new();
        let engine = LispEngine::new(env);
        let _res = engine.borrow().run("(alias \"e\" \"emacs\")");
    }

    #[test]
    fn test_apply_fn() {
        init();
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

    #[tokio::test]
    #[ignore = "requires shell execution context"]
    async fn test_call_fn() {
        init();
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

        let args = vec![Value::Int(IntType::from(1)), Value::Int(IntType::from(2))];
        let res = engine.borrow().run_func_values("adder", args);
        assert!(res.is_ok());
        println!("{res:?}");

        let args = vec![];
        let res = engine.borrow().run_func_values("call", args);
        assert!(res.is_ok());
        println!("{res:?}");
    }

    #[test]
    fn test_register_action() {
        init();
        let env = Environment::new();
        let engine = LispEngine::new(env);

        // 1. Defun a function
        let _ = engine
            .borrow()
            .run("(defun my-test-func () (print \"success\"))");

        // 2. Register it as an action
        let res = engine
            .borrow()
            .run("(register-action \"My Test Action\" \"A test action\" \"my-test-func\")");
        assert!(res.is_ok());

        // 3. Verify it's in the registry
        let registry = crate::command_palette::REGISTRY.read();
        let actions = registry.get_all();
        let action = actions
            .iter()
            .find(|a| a.name() == "My Test Action")
            .expect("Action not found in registry");
        assert_eq!(action.description(), "A test action");

        // 4. (Optional) Check if it works without real Shell if possible,
        // but since execute() needs &mut Shell, we'll stop here for unit test
        // or just verify it doesn't panic when we look it up.
    }
}

use crate::environment::{self, Environment};
use crate::lisp::default_environment::default_env;
use crate::lisp::interpreter::eval;
pub use crate::lisp::model::Value;
pub use crate::lisp::model::{CmpValue, FloatType, IntType, Record, Table, TableRc};
pub use crate::lisp::model::{Env, Symbol};
use crate::lisp::model::{List, RuntimeError};
use crate::lisp::parser::parse;
use crate::secrets::SecretManagerSnapshot;
use crate::suggestion::InputPreferences;
use anyhow::Context;
use dsh_builtin::McpRuntimeStateSnapshot;
use dsh_types::shell_options::ShellOptions;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::sync::Arc;
use std::{cell::RefCell, rc::Rc};

mod builtin;
mod command_palette;
mod cron;
mod default_environment;
mod interpreter;
mod keybind;
mod macros;
#[cfg(test)]
mod mcp_tests;
mod model;
mod parser;
pub mod stdlib;
mod stdlib_tests;
mod utils;

pub const CONFIG_FILE: &str = "config.lisp";

#[derive(Debug)]
pub struct LispEngine {
    pub env: Rc<RefCell<Env>>,
    pub shell_env: Arc<RwLock<Environment>>,
}

#[derive(Debug, Clone)]
struct EnvironmentSnapshot {
    alias: HashMap<String, String>,
    abbreviations: HashMap<String, String>,
    command_abbreviations: HashMap<String, HashMap<String, String>>,
    command_ledger_mode: crate::history::CommandLedgerMode,
    paths: Vec<String>,
    variables: HashMap<String, String>,
    exported_vars: HashSet<String>,
    direnv_roots: Vec<crate::direnv::DirEnvironment>,
    mcp_servers: Vec<dsh_types::mcp::McpServerConfig>,
    mcp_runtime_state: McpRuntimeStateSnapshot,
    execute_allowlist: Vec<String>,
    input_preferences: InputPreferences,
    safety_level: crate::safety::SafetyLevel,
    command_cache: HashMap<String, Option<String>>,
    executable_names: Vec<String>,
    z_exclude: Vec<String>,
    keybindings: crate::repl::keybind::KeyBindings,
    startup_mode: bool,
    secret_manager: SecretManagerSnapshot,
    shell_options: ShellOptions,
}

impl EnvironmentSnapshot {
    fn capture(env: &Environment) -> Self {
        Self {
            alias: env.variable_state.alias.clone(),
            abbreviations: env.variable_state.abbreviations.clone(),
            command_abbreviations: env.variable_state.command_abbreviations.clone(),
            command_ledger_mode: env.variable_state.command_ledger_mode,
            paths: env.variable_state.paths.clone(),
            variables: env.variable_state.variables.clone(),
            exported_vars: env.variable_state.exported_vars.clone(),
            direnv_roots: env.variable_state.direnv_roots.clone(),
            mcp_servers: env.mcp_servers().to_vec(),
            mcp_runtime_state: env
                .integration_state
                .mcp_manager
                .read()
                .snapshot_runtime_state(),
            execute_allowlist: env.policy_state.execute_allowlist.read().clone(),
            input_preferences: env.completion_state.input_preferences,
            safety_level: *env.policy_state.safety_level.read(),
            command_cache: env.completion_state.command_cache.read().clone(),
            executable_names: env.completion_state.executable_names.read().clone(),
            z_exclude: env.variable_state.z_exclude.clone(),
            keybindings: env.variable_state.keybindings.clone(),
            startup_mode: env.startup_mode,
            secret_manager: env.policy_state.secret_manager.snapshot(),
            shell_options: env.shell_options,
        }
    }
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

        let env_snapshot = {
            let env = self.shell_env.read();
            EnvironmentSnapshot::capture(&env)
        };
        let lisp_entries_snapshot = self.env.borrow().snapshot_entries();
        let lisp_autocompletion_snapshot = self.env.borrow().snapshot_autocompletion();
        let action_registry_snapshot = crate::command_palette::REGISTRY.read().snapshot_actions();
        let process_env_snapshot: HashMap<OsString, OsString> = std::env::vars_os().collect();

        {
            // Treat config evaluation as startup mode to avoid mutating active MCP connections
            // while the file is being parsed/evaluated.
            let mut env = self.shell_env.write();
            env.startup_mode = true;
            env.clear_mcp_servers();
        }

        let wrapped_config = format!("(begin {config_lisp}\n)");
        let run_result = self.run(&wrapped_config);

        match run_result {
            Ok(_) => {
                // Restore original startup mode regardless of success/failure.
                self.shell_env.write().startup_mode = env_snapshot.startup_mode;
                tracing::debug!("Successfully loaded config.lisp");
                Ok(())
            }
            Err(e) => {
                // Roll back shell environment and Lisp symbols on failure so partial
                // config evaluation does not leave the shell in a broken state.
                self.restore_environment_snapshot(env_snapshot);
                self.env.borrow_mut().restore_entries(lisp_entries_snapshot);
                self.env
                    .borrow()
                    .restore_autocompletion(lisp_autocompletion_snapshot);
                crate::command_palette::REGISTRY
                    .write()
                    .restore_actions(action_registry_snapshot);
                restore_process_env(process_env_snapshot);
                tracing::error!("Failed to execute config.lisp: {}", e);
                Err(e)
            }
        }
    }

    fn restore_environment_snapshot(&self, snapshot: EnvironmentSnapshot) {
        let mut env = self.shell_env.write();
        env.variable_state.alias = snapshot.alias;
        env.variable_state.abbreviations = snapshot.abbreviations;
        env.variable_state.command_abbreviations = snapshot.command_abbreviations;
        env.variable_state.command_ledger_mode = snapshot.command_ledger_mode;
        env.variable_state.paths = snapshot.paths;
        env.variable_state.variables = snapshot.variables;
        env.variable_state.exported_vars = snapshot.exported_vars;
        env.variable_state.direnv_roots = snapshot.direnv_roots;
        env.replace_mcp_servers(snapshot.mcp_servers);
        env.integration_state
            .mcp_manager
            .write()
            .restore_runtime_state(snapshot.mcp_runtime_state);
        *env.policy_state.execute_allowlist.write() = snapshot.execute_allowlist;
        env.completion_state.input_preferences = snapshot.input_preferences;
        *env.policy_state.safety_level.write() = snapshot.safety_level;
        *env.completion_state.command_cache.write() = snapshot.command_cache;
        *env.completion_state.executable_names.write() = snapshot.executable_names;
        env.variable_state.z_exclude = snapshot.z_exclude;
        env.variable_state.keybindings = snapshot.keybindings;
        env.startup_mode = snapshot.startup_mode;
        env.policy_state
            .secret_manager
            .restore(snapshot.secret_manager);
        env.shell_options = snapshot.shell_options;
        env.refresh_variable_projections();
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

    pub fn has(&self, name: &str) -> bool {
        if let Ok(v) = self.run(name) {
            v != Value::NIL
        } else {
            false
        }
    }

    /// Check if a symbol is bound and contains a non-empty list.
    /// This is an efficient check for hook lists without evaluating Lisp code.
    pub fn is_bound_nonempty_list(&self, name: &str) -> bool {
        let symbol = Symbol::from(name);
        if let Some(value) = self.env.borrow().get(&symbol) {
            matches!(&value, Value::List(list) if *list != List::NIL)
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

fn restore_process_env(snapshot: HashMap<OsString, OsString>) {
    let current_keys: Vec<OsString> = std::env::vars_os().map(|(key, _)| key).collect();

    for key in current_keys {
        if !snapshot.contains_key(&key) {
            unsafe {
                std::env::remove_var(&key);
            }
        }
    }

    for (key, value) in snapshot {
        unsafe {
            std::env::set_var(&key, &value);
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
    env.borrow_mut().define(
        Symbol::from("abbr-command"),
        Value::NativeFunc(builtin::abbr_command),
    );
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
    env.borrow_mut().define(
        Symbol::from("pref-ai-explanation"),
        Value::NativeFunc(builtin::pref_ai_explanation),
    );
    env.borrow_mut().define(
        Symbol::from("pref-status-line"),
        Value::NativeFunc(builtin::pref_status_line),
    );
    env.borrow_mut().define(
        Symbol::from("pref-failure-hint"),
        Value::NativeFunc(builtin::pref_failure_hint),
    );
    env.borrow_mut().define(
        Symbol::from("pref-diagnose-hint"),
        Value::NativeFunc(builtin::pref_diagnose_hint),
    );
    env.borrow_mut().define(
        Symbol::from("pref-command-ledger"),
        Value::NativeFunc(builtin::pref_command_ledger),
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
mod tests;

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
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::sync::Arc;
use std::{cell::RefCell, rc::Rc};

mod builtin;
mod command_palette;
mod default_environment;
mod interpreter;
mod keybind;
mod macros;
#[cfg(test)]
mod mcp_tests;
mod model;
mod parser;
mod sched;
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
    system_env_vars: HashMap<String, String>,
    input_preferences: InputPreferences,
    safety_level: crate::safety::SafetyLevel,
    command_cache: HashMap<String, Option<String>>,
    executable_names: Vec<String>,
    z_exclude: Vec<String>,
    keybindings: crate::repl::keybind::KeyBindings,
    startup_mode: bool,
    secret_manager: SecretManagerSnapshot,
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
            system_env_vars: env.variable_state.system_env_vars.clone(),
            input_preferences: env.completion_state.input_preferences,
            safety_level: env.policy_state.safety_level.read().clone(),
            command_cache: env.completion_state.command_cache.read().clone(),
            executable_names: env.completion_state.executable_names.read().clone(),
            z_exclude: env.variable_state.z_exclude.clone(),
            keybindings: env.variable_state.keybindings.clone(),
            startup_mode: env.startup_mode,
            secret_manager: env.policy_state.secret_manager.snapshot(),
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
        env.variable_state.system_env_vars = snapshot.system_env_vars;
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
mod tests {

    use super::*;
    use std::ffi::OsString;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    fn with_test_config_home<F>(test_fn: F)
    where
        F: FnOnce(),
    {
        let _guard = crate::test_env_lock();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("dsh-test-config-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();

        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", OsString::from(dir));
        }

        test_fn();

        match previous {
            Some(value) => unsafe {
                std::env::set_var("XDG_CONFIG_HOME", value);
            },
            None => unsafe {
                std::env::remove_var("XDG_CONFIG_HOME");
            },
        }
    }

    #[test]
    fn test_run_lisp() {
        init();
        let env = Environment::new();
        let engine = LispEngine::new(env);
        let _res = engine.borrow().run("(alias \"e\" \"emacs\")");
    }

    #[test]
    fn command_ledger_preference_is_off_by_default_and_configurable() {
        init();
        let env = Environment::new();
        let engine = LispEngine::new(env.clone());
        assert_eq!(
            env.read().variable_state.command_ledger_mode,
            crate::history::CommandLedgerMode::Off
        );
        engine
            .borrow()
            .run("(pref-command-ledger \"metadata\")")
            .unwrap();
        assert_eq!(
            env.read().variable_state.command_ledger_mode,
            crate::history::CommandLedgerMode::Metadata
        );
        assert!(
            engine
                .borrow()
                .run("(pref-command-ledger \"invalid\")")
                .is_err()
        );
    }

    #[test]
    fn lisp_can_define_command_scoped_abbreviation() {
        init();
        let env = Environment::new();
        let engine = LispEngine::new(env.clone());
        engine
            .borrow()
            .run("(abbr-command \"git\" \"co\" \"checkout\")")
            .unwrap();
        assert_eq!(
            env.read()
                .variable_state
                .command_abbreviations
                .get("git")
                .and_then(|entries| entries.get("co"))
                .map(String::as_str),
            Some("checkout")
        );
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

    #[test]
    fn bind_and_unbind_update_the_environment() {
        init();
        let env = Environment::new();
        let engine = LispEngine::new(env.clone());

        assert!(
            engine
                .borrow()
                .run("(bind \"ctrl-g\" \"cancel-completion\")")
                .is_ok()
        );
        assert!(
            env.read()
                .key_binding_descriptions()
                .contains(&"ctrl-g -> cancel-completion".to_string())
        );

        // An unknown action name is taken as a Lisp function, not an error:
        // the function may be defined later in config.lisp.
        assert!(engine.borrow().run("(bind \"ctrl-t\" \"my-fn\")").is_ok());
        assert!(
            env.read()
                .key_binding_descriptions()
                .contains(&"ctrl-t -> lisp:my-fn".to_string())
        );

        assert!(engine.borrow().run("(unbind \"ctrl-g\")").is_ok());
        assert!(
            !env.read()
                .key_binding_descriptions()
                .iter()
                .any(|line| line.starts_with("ctrl-g "))
        );
    }

    #[test]
    fn sched_add_registers_a_task_from_lisp() {
        init();
        let env = Environment::new();
        let engine = LispEngine::new(env.clone());

        let res = engine
            .borrow()
            .run("(sched-add \"fetch\" \"5m\" \"git fetch --all\" \"change\")");
        assert!(res.is_ok(), "{res:?}");

        let listed = env.read().sched_descriptions();
        assert_eq!(
            listed,
            vec!["fetch every 5m -> git fetch --all".to_string()]
        );

        assert!(engine.borrow().run("(sched-pause \"fetch\")").is_ok());
        assert!(env.read().sched_descriptions()[0].ends_with("(paused)"));

        assert!(engine.borrow().run("(sched-remove \"fetch\")").is_ok());
        assert!(env.read().sched_descriptions().is_empty());
    }

    #[test]
    fn sched_add_rejects_bad_arguments() {
        init();
        let env = Environment::new();
        let engine = LispEngine::new(env);

        // Interval below the 5s floor.
        assert!(
            engine
                .borrow()
                .run("(sched-add \"a\" \"1s\" \"true\")")
                .is_err()
        );
        assert!(
            engine
                .borrow()
                .run("(sched-add \"a\" \"5x\" \"true\")")
                .is_err()
        );
        assert!(
            engine
                .borrow()
                .run("(sched-add \"a\" \"5m\" \"true\" \"sometimes\")")
                .is_err()
        );
        assert!(engine.borrow().run("(sched-add \"a\" \"5m\")").is_err());
    }

    #[test]
    fn bind_rejects_an_unparseable_key() {
        init();
        let env = Environment::new();
        let engine = LispEngine::new(env);

        assert!(
            engine
                .borrow()
                .run("(bind \"ctrl-nope\" \"undo\")")
                .is_err()
        );
        assert!(engine.borrow().run("(bind \"ctrl-g\")").is_err());
    }

    /// A failed config must not leave half-applied bindings behind.
    #[test]
    fn config_rollback_restores_key_bindings() {
        init();
        with_test_config_home(|| {
            let env = Environment::new();
            let engine = LispEngine::new(env.clone());

            let config_path = environment::get_config_file(CONFIG_FILE).unwrap();
            std::fs::write(
                &config_path,
                "(bind \"ctrl-g\" \"cancel-completion\")\n(this-does-not-exist)\n",
            )
            .unwrap();

            assert!(engine.borrow().run_config_lisp().is_err());
            assert!(
                !env.read()
                    .key_binding_descriptions()
                    .iter()
                    .any(|line| line.starts_with("ctrl-g ")),
                "binding survived a failed config load"
            );
        });
    }

    #[test]
    fn run_config_lisp_rolls_back_state_on_error() {
        init();
        with_test_config_home(|| {
            let env = Environment::new();
            let engine = LispEngine::new(env.clone());
            let rollback_action_name = "ROLLBACK_TEST_ACTION_SHOULD_NOT_EXIST";
            let rollback_env_key = "ROLLBACK_TEST_TEMP_ENV";

            let config_path = crate::environment::get_config_file(CONFIG_FILE).unwrap();

            std::fs::write(
                &config_path,
                r#"
(mcp-clear)
(mcp-add-sse "stable" "https://example.com/stable" "stable")
(alias "stable-alias" "echo stable")
(vset "STABLE_VAR" "stable")
(chat-execute-clear)
(chat-execute-add "ls")
(secret-history-mode "redact")
(defun stable-func () "stable")
"#,
            )
            .unwrap();
            engine.borrow().run_config_lisp().unwrap();

            {
                let env_read = env.read();
                assert_eq!(env_read.mcp_servers().len(), 1);
                assert_eq!(env_read.mcp_servers()[0].label, "stable");
                assert!(!env_read.startup_mode);
                assert_eq!(
                    env_read.variable_state.alias.get("stable-alias"),
                    Some(&"echo stable".to_string())
                );
                assert_eq!(
                    env_read.variable_state.variables.get("STABLE_VAR"),
                    Some(&"stable".to_string())
                );
                let allowlist = env_read.policy_state.execute_allowlist.read().clone();
                assert_eq!(allowlist, vec!["ls".to_string()]);
                assert_eq!(
                    env_read.policy_state.secret_manager.history_mode(),
                    crate::secrets::SecretHistoryMode::Redact
                );
            }
            assert!(engine.borrow().has("stable-func"));

            let mcp_runtime_before = {
                let env_read = env.read();
                let manager = env_read.integration_state.mcp_manager.read();
                let mut snapshot = manager.snapshot_runtime_state();
                snapshot
                    .session_meta
                    .insert("stable".to_string(), std::time::Instant::now());
                snapshot
                    .connection_errors
                    .insert("stable".to_string(), "seeded".to_string());
                manager.restore_runtime_state(snapshot.clone());
                snapshot
            };

            std::fs::write(
                &config_path,
                format!(
                    r#"
(mcp-clear)
(mcp-add-sse "broken" "https://example.com/broken" "broken")
(mcp-disconnect-all)
(alias "broken-alias" "echo broken")
(vset "BROKEN_VAR" "broken")
(chat-execute-clear)
(chat-execute-add "rm -rf /")
(secret-history-mode "none")
(setenv "{rollback_env_key}" "broken")
(register-action "{rollback_action_name}" "Rollback test action" "stable-func")
(defun broken-func () "broken")
(this-function-does-not-exist)
            "#,
                ),
            )
            .unwrap();

            let err = engine.borrow().run_config_lisp().unwrap_err();
            assert!(err.to_string().contains("this-function-does-not-exist"));

            let env_read = env.read();
            assert_eq!(env_read.mcp_servers().len(), 1);
            assert_eq!(env_read.mcp_servers()[0].label, "stable");
            assert!(!env_read.startup_mode);
            assert_eq!(
                env_read.variable_state.alias.get("stable-alias"),
                Some(&"echo stable".to_string())
            );
            assert!(!env_read.variable_state.alias.contains_key("broken-alias"));
            assert_eq!(
                env_read.variable_state.variables.get("STABLE_VAR"),
                Some(&"stable".to_string())
            );
            assert!(!env_read.variable_state.variables.contains_key("BROKEN_VAR"));
            assert_eq!(
                env_read.policy_state.secret_manager.history_mode(),
                crate::secrets::SecretHistoryMode::Redact
            );
            let allowlist = env_read.policy_state.execute_allowlist.read().clone();
            assert_eq!(allowlist, vec!["ls".to_string()]);
            let mcp_runtime_after = env_read
                .integration_state
                .mcp_manager
                .read()
                .snapshot_runtime_state();
            assert_eq!(mcp_runtime_after, mcp_runtime_before);

            assert!(engine.borrow().has("stable-func"));
            assert!(!engine.borrow().has("broken-func"));
            assert!(std::env::var(rollback_env_key).is_err());
            let has_broken_action = crate::command_palette::REGISTRY
                .read()
                .get_all()
                .iter()
                .any(|action| action.name() == rollback_action_name);
            assert!(!has_broken_action);
        });
    }
}

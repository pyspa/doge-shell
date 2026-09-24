//! Lisp builtins bridging `config.lisp` to shell state.
//!
//! Native functions registered on the Lisp `Env` (`setenv`/`vset`, `alias`,
//! `add_path`, direnv roots, safety level, editor helpers). PATH-affecting
//! builtins route through the Environment's canonical mutation path so the
//! logical variable, command cache, and completion activation stay in sync.
//! Submodules hold the `sh` execution core (`exec`), input preferences
//! (`prefs`), and secret helpers (`secret`).

use crate::direnv::DirEnvironment;
use crate::lisp::model::{Env, RuntimeError, Value};
use crate::shell::Shell;
use crate::utils::editor::launch_editor;
use anyhow::Result;
use dsh_types::Context;
use nix::sys::termios::tcgetattr;
use nix::unistd::pipe;
use std::borrow::Cow;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::process::Command;
use std::sync::Arc;
use std::{cell::RefCell, rc::Rc};
use tracing::debug;

mod exec;
mod prefs;
mod secret;
#[cfg(test)]
use exec::sh_core;
pub use exec::{block_sh, block_sh_no_cap, command};
pub use prefs::{
    pref_ai_explanation, pref_auto_notify, pref_auto_pair, pref_command_ledger, pref_diagnose_hint,
    pref_failure_hint, pref_status_line,
};
pub use secret::{
    secret_add_keyword, secret_add_pattern, secret_clear, secret_get, secret_history_mode,
    secret_list_patterns, secret_set,
};
fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    key.contains("API_KEY")
        || key.ends_with("_KEY")
        || key.contains("TOKEN")
        || key.contains("SECRET")
        || key.contains("PASSWORD")
        || key.contains("PASSWD")
        || key.contains("PASSPHRASE")
        || key.contains("AUTH")
        || key.contains("COOKIE")
        || key.contains("SESSION")
}

fn redact_value_for_log<'a>(key: &str, value: &'a str) -> Cow<'a, str> {
    if is_sensitive_key(key) {
        Cow::Borrowed("<redacted>")
    } else {
        Cow::Borrowed(value)
    }
}

pub fn set_env(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::new("set-env requires at least 2 arguments"));
    }
    let key = &args[0];
    let key = key.to_string();

    // SAFETY CHECK
    {
        use crate::safety::{SafetyGuard, SafetyResult};
        let guard = SafetyGuard::new();
        let env_ref = env.borrow();
        let shell_env = env_ref.shell_env.read();
        let safety_level = shell_env.policy_state.safety_level.read();

        if args.len() > 1 {
            let val_str = &args[1].to_string(); // Approximate value check
            match guard.check_environment_modification(&key, val_str, &safety_level) {
                SafetyResult::Allowed => {}
                SafetyResult::Confirm(msg) => {
                    return Err(RuntimeError::new(&format!("SafetyGuard Blocked: {}", msg)));
                }
            }
        }
    }

    if key == "PATH" {
        let mut path_vec = vec![];
        for val in &args[1..] {
            let val = val.to_string();
            for val in val.split(':') {
                path_vec.push(val.to_string());
            }
        }
        let env_path = path_vec.join(":");
        let display_val = redact_value_for_log(&key, &env_path);
        debug!("set env {} {}", &key, display_val);
        env.borrow()
            .shell_env
            .write()
            .set_and_export_shell_var("PATH".to_string(), env_path);
    } else {
        let val = &args[1];
        let val_string = val.to_string();
        let display_val = redact_value_for_log(&key, &val_string);
        debug!("set env {} {}", &key, display_val);
        env.borrow()
            .shell_env
            .write()
            .set_and_export_shell_var(key.clone(), val_string);
    }
    Ok(Value::NIL)
}

pub fn set_variable(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::new("set requires 2 arguments"));
    }
    let key = args[0].to_string();
    let val = args[1].to_string();

    // SAFETY CHECK
    {
        use crate::safety::{SafetyGuard, SafetyResult};
        let guard = SafetyGuard::new();
        let env_ref = env.borrow();
        let shell_env = env_ref.shell_env.read();
        let safety_level = shell_env.policy_state.safety_level.read();

        match guard.check_environment_modification(&key, &val, &safety_level) {
            SafetyResult::Allowed => {}
            SafetyResult::Confirm(msg) => {
                return Err(RuntimeError::new(&format!("SafetyGuard Blocked: {}", msg)));
            }
        }
    }

    let display_val = redact_value_for_log(&key, &val);
    debug!("set variable {} {}", &key, display_val);
    env.borrow().shell_env.write().set_shell_var(key, val);
    Ok(Value::NIL)
}

pub fn alias(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::new(
            "alias requires 2 arguments: alias and command",
        ));
    }
    let alias = &args[0];
    let command = &args[1];
    debug!("set alias {} {}", alias, command);
    env.borrow()
        .shell_env
        .write()
        .variable_state
        .alias
        .insert(alias.to_string(), command.to_string());
    Ok(Value::NIL)
}

/// Built-in abbr function for Lisp
/// Sets abbreviations that expand in real-time during input
/// Usage: (abbr "name" "expansion")
pub fn abbr(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(RuntimeError::new(
            "abbr requires exactly 2 arguments: name and expansion",
        ));
    }

    let name = &args[0];
    let expansion = &args[1];
    env.borrow()
        .shell_env
        .write()
        .variable_state
        .abbreviations
        .insert(name.to_string(), expansion.to_string());
    Ok(Value::NIL)
}

/// Defines an abbreviation that is expanded only after a direct command.
/// Usage: (abbr-command "git" "co" "checkout")
pub fn abbr_command(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() != 3 {
        return Err(RuntimeError::new(
            "abbr-command requires exactly 3 arguments: command, name, and expansion",
        ));
    }

    let command = args[0].to_string();
    let name = args[1].to_string();
    let expansion = args[2].to_string();
    env.borrow()
        .shell_env
        .write()
        .variable_state
        .command_abbreviations
        .entry(command)
        .or_default()
        .insert(name, expansion);
    Ok(Value::NIL)
}

pub fn allow_direnv(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    for arg in args {
        let root = arg.to_string();
        let root = shellexpand::tilde(root.as_str());
        env.borrow()
            .shell_env
            .write()
            .variable_state
            .direnv_roots
            .push(DirEnvironment::new(root.to_string()));
    }
    Ok(Value::NIL)
}

pub fn add_path(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    for arg in args {
        let path = arg.to_string();
        env.borrow().shell_env.write().insert_path_entry(0, &path);
    }
    Ok(Value::NIL)
}

pub fn safety_level(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        let level = *env
            .borrow()
            .shell_env
            .read()
            .policy_state
            .safety_level
            .read();
        return Ok(Value::String(level.as_str().to_string()));
    }

    let level_str = args[0].to_string();
    let level: crate::safety::SafetyLevel = level_str
        .parse()
        .map_err(|e| RuntimeError::new(&format!("Error parsing safety level: {}", e)))?;

    debug!("setting safety level to {:?}", level);
    {
        let env_ref = env.borrow();
        let mut shell_env = env_ref.shell_env.write();
        *shell_env.policy_state.safety_level.write() = level;
        // The variable is the readable copy, not the source of truth: every
        // policy check reads `policy_state.safety_level` through
        // `ShellProxy::safety_level`.
        shell_env.set_shell_var("SAFETY_LEVEL".to_string(), level.as_str().to_string());
    }

    Ok(Value::NIL)
}

pub fn edit(_env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(RuntimeError {
            msg: "edit requires 1 argument".to_string(),
        });
    }

    let path_str = match &args[0] {
        Value::String(s) => s,
        _ => {
            return Err(RuntimeError {
                msg: "edit argument must be a string".to_string(),
            });
        }
    };

    let path = std::path::Path::new(path_str);
    launch_editor(path).map_err(|e| RuntimeError {
        msg: format!("Failed to launch editor: {}", e),
    })?;
    Ok(Value::True)
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::environment::Environment;
    use crate::lisp::LispEngine;

    fn init() {
        let _ = tracing_subscriber::fmt::try_init();
    }

    #[tokio::test]
    #[ignore] // Ignore this test as it requires a TTY environment
    async fn test_lisp_sh() {
        init();
        let env = Environment::new();
        let engine = LispEngine::new(env);

        // Skip TTY-dependent test in non-TTY environments
        if !nix::unistd::isatty(unsafe { BorrowedFd::borrow_raw(0) }).unwrap_or(false) {
            println!("Skipping TTY-dependent test");
            return;
        }

        let args = vec!["ls".to_string(), "-al".to_string()];
        let shell_env = Arc::clone(&engine.borrow().env.borrow().shell_env);
        let res = sh_core(shell_env, args).await;
        assert!(res.is_ok());
        if let Ok(result) = res {
            println!("{result}");
        }
    }

    fn shell_env_of(
        engine: &Rc<RefCell<crate::lisp::LispEngine>>,
    ) -> std::sync::Arc<parking_lot::RwLock<Environment>> {
        std::sync::Arc::clone(&engine.borrow().env.borrow().shell_env)
    }

    #[test]
    fn add_path_updates_the_logical_path_variable_and_projection() {
        init();
        let env = Environment::new();
        env.write()
            .set_shell_var("PATH".to_string(), "/old".to_string());
        let engine = LispEngine::new(env);

        engine
            .borrow()
            .run("(add_path \"/custom/bin\")")
            .expect("add_path runs");

        let shell_env = shell_env_of(&engine);
        let guard = shell_env.read();
        assert_eq!(
            guard.lookup_variable("PATH"),
            Some("/custom/bin:/old".to_string())
        );
        assert_eq!(guard.variable_state.paths[0], "/custom/bin".to_string());
    }

    #[test]
    fn add_path_uses_the_logical_home_for_tilde() {
        init();
        let _guard = crate::test_env_lock();
        let _stale = crate::ProcessEnvGuard::set("HOME", "/stale/process/home");
        let env = Environment::new();
        env.write()
            .set_shell_var("PATH".to_string(), "/old".to_string());
        let engine = LispEngine::new(env);

        engine
            .borrow()
            .run("(vset \"HOME\" \"/logical/home\")")
            .expect("vset HOME runs");
        engine
            .borrow()
            .run("(add_path \"~/bin\")")
            .expect("add_path runs");

        let shell_env = shell_env_of(&engine);
        let guard = shell_env.read();
        assert_eq!(
            guard.lookup_variable("PATH"),
            Some("/logical/home/bin:/old".to_string())
        );
        assert_eq!(
            guard.variable_state.paths[0],
            "/logical/home/bin".to_string()
        );
    }

    #[test]
    fn add_path_keeps_the_existing_prepend_order_for_multiple_args() {
        init();
        let env = Environment::new();
        env.write()
            .set_shell_var("PATH".to_string(), "/old".to_string());
        let engine = LispEngine::new(env);

        engine
            .borrow()
            .run("(add_path \"/a\" \"/b\")")
            .expect("add_path runs");

        let shell_env = shell_env_of(&engine);
        let guard = shell_env.read();
        assert_eq!(
            guard.lookup_variable("PATH"),
            Some("/b:/a:/old".to_string())
        );
        assert_eq!(
            guard.variable_state.paths,
            vec!["/b".to_string(), "/a".to_string(), "/old".to_string()]
        );
    }

    #[test]
    fn test_builtin_argument_length_checks() {
        init();
        let env = Environment::new();
        // LispEngine::new registers builtins using make_env
        let engine = LispEngine::new(env);

        // setenv requires 2 arguments
        let result = engine.borrow().run("(setenv \"VAR\")");
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("requires at least 2 arguments"));
        }

        // vset requires 2 arguments
        let result = engine.borrow().run("(vset \"VAR\")");
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("requires 2 arguments"));
        }

        // alias requires 2 arguments
        let result = engine.borrow().run("(alias \"ll\")");
        assert!(result.is_err());

        // abbr requires 2 arguments
        let result = engine.borrow().run("(abbr \"ll\")");
        assert!(result.is_err());
    }
}

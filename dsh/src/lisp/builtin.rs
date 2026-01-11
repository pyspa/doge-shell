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
use std::os::unix::io::FromRawFd;
use std::process::Command;
use std::sync::Arc;
use std::{cell::RefCell, rc::Rc};
use tracing::debug;

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
    let key = &args[0];
    let key = key.to_string();
    if key == "PATH" {
        let mut path_vec = vec![];
        for val in &args[1..] {
            let val = val.to_string();
            for val in val.split(':') {
                path_vec.push(val.to_string());
            }
        }
        let env_path = path_vec.join(":");
        unsafe { std::env::set_var("PATH", &env_path) };
        let display_val = redact_value_for_log(&key, &env_path);
        debug!("set env {} {}", &key, display_val);
        env.borrow().shell_env.write().paths = path_vec;
    } else {
        let val = &args[1];
        let val_string = val.to_string();
        unsafe { std::env::set_var(&key, &val_string) };
        let display_val = redact_value_for_log(&key, &val_string);
        debug!("set env {} {}", &key, display_val);
    }
    Ok(Value::NIL)
}

pub fn set_variable(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let key = args[0].to_string();
    let val = args[1].to_string();
    let display_val = redact_value_for_log(&key, &val);
    debug!("set variable {} {}", &key, display_val);
    env.borrow().shell_env.write().variables.insert(key, val);
    Ok(Value::NIL)
}

pub fn alias(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let alias = &args[0];
    let command = &args[1];
    debug!("set alias {} {}", alias, command);
    env.borrow()
        .shell_env
        .write()
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
        .abbreviations
        .insert(name.to_string(), expansion.to_string());
    Ok(Value::NIL)
}

pub fn allow_direnv(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    for arg in args {
        let root = arg.to_string();
        let root = shellexpand::tilde(root.as_str());
        // Create DirEnvironment with error handling
        match DirEnvironment::new(root.to_string()) {
            Ok(direnv) => {
                env.borrow().shell_env.write().direnv_roots.push(direnv);
            }
            Err(e) => {
                eprintln!("Warning: Failed to create direnv for {root}: {e}");
            }
        }
    }
    Ok(Value::NIL)
}

pub fn add_path(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    for arg in args {
        let path = arg.to_string();
        let path = shellexpand::tilde(path.as_str());
        env.borrow()
            .shell_env
            .write()
            .paths
            .insert(0, path.to_string());
    }
    Ok(Value::NIL)
}

pub fn command(_env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let mut cmd_args: Vec<String> = Vec::new();
    for arg in args {
        if let Value::String(val) = arg {
            cmd_args.push(val.to_string());
        }
    }
    let cmd = cmd_args.remove(0);
    match Command::new(cmd).args(cmd_args).output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_string();

            let stderr = String::from_utf8_lossy(&output.stderr)
                .trim_end()
                .to_string();

            if !stdout.is_empty() {
                Ok(Value::String(stdout))
            } else {
                Ok(Value::String(stderr))
            }
        }
        Err(err) => Err(RuntimeError {
            msg: err.to_string(),
        }),
    }
}

pub fn block_sh(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    tokio::task::block_in_place(move || tokio::runtime::Handle::current().block_on(sh(env, args)))
}

pub async fn sh(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let mut cmd_args: Vec<String> = Vec::new();

    for arg in args {
        let val = arg.to_string();
        cmd_args.push(val);
    }
    let input = cmd_args.join(" ");

    let mut shell = Shell::new(Arc::clone(&env.borrow().shell_env));
    shell.set_signals();
    let shell_tmode = match tcgetattr(0) {
        Ok(tmode) => tmode,
        Err(err) => {
            eprintln!("error: {err}");
            return Err(RuntimeError {
                msg: err.to_string(),
            });
        }
    };

    let mut ctx = Context::new(shell.pid, shell.pgid, Some(shell_tmode), true);
    let (pout, pin) = match pipe() {
        Ok(p) => p,
        Err(err) => {
            eprintln!("error: {err}");
            return Err(RuntimeError {
                msg: err.to_string(),
            });
        }
    };

    ctx.captured_out = Some(pin);
    if let Err(err) = shell.eval_str(&mut ctx, input, false).await {
        eprintln!("error: {err}");
        return Err(RuntimeError {
            msg: err.to_string(),
        });
    }

    let mut raw_stdout = Vec::new();
    unsafe { File::from_raw_fd(pout).read_to_end(&mut raw_stdout).ok() };

    let output = match std::str::from_utf8(&raw_stdout) {
        Ok(str) => str.trim_matches('\n').to_owned(),
        Err(err) => {
            eprintln!("error: {err}");
            return Err(RuntimeError {
                msg: err.to_string(),
            });
        }
    };
    debug!("'{}'", output);
    Ok(Value::String(output))
}

pub fn safety_level(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        let level = env.borrow().shell_env.read().safety_level.read().clone();
        return Ok(Value::String(format!("{:?}", level).to_lowercase()));
    }

    let level_str = args[0].to_string();
    let level: crate::safety::SafetyLevel = level_str
        .parse()
        .map_err(|e| RuntimeError::new(&format!("Error parsing safety level: {}", e)))?;

    debug!("setting safety level to {:?}", level);
    {
        let env_ref = env.borrow();
        let mut shell_env = env_ref.shell_env.write();
        *shell_env.safety_level.write() = level.clone();
        shell_env.variables.insert(
            "SAFETY_LEVEL".to_string(),
            format!("{:?}", level).to_lowercase(),
        );
    }

    Ok(Value::NIL)
}

pub fn pref_auto_pair(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Ok(Value::from(
            env.borrow().shell_env.read().input_preferences.auto_pair,
        ));
    }

    let enabled = bool::from(&args[0]);

    debug!("setting auto-pair to {:?}", enabled);
    env.borrow()
        .shell_env
        .write()
        .set_auto_pair_enabled(enabled);
    Ok(Value::NIL)
}

pub fn pref_auto_notify(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Ok(Value::from(
            env.borrow()
                .shell_env
                .read()
                .input_preferences
                .auto_notify_enabled,
        ));
    }

    let enabled = bool::from(&args[0]);

    debug!("setting auto-notify to {:?}", enabled);
    env.borrow()
        .shell_env
        .write()
        .set_auto_notify_enabled(enabled);
    Ok(Value::NIL)
}

pub fn block_sh_no_cap(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    tokio::task::block_in_place(move || {
        tokio::runtime::Handle::current().block_on(sh_no_cap(env, args))
    })
}

pub async fn sh_no_cap(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let mut cmd_args: Vec<String> = Vec::new();

    for arg in args {
        let val = arg.to_string();
        cmd_args.push(val);
    }
    let input = cmd_args.join(" ");

    let mut shell = Shell::new(Arc::clone(&env.borrow().shell_env));
    shell.set_signals();
    let shell_tmode = match tcgetattr(0) {
        Ok(tmode) => tmode,
        Err(err) => {
            eprintln!("error: {err}");
            return Err(RuntimeError {
                msg: err.to_string(),
            });
        }
    };

    let mut ctx = Context::new(shell.pid, shell.pgid, Some(shell_tmode), true);
    // ctx.captured_out = Some(pin);
    if let Err(err) = shell.eval_str(&mut ctx, input, false).await {
        eprintln!("error: {err}");
        return Err(RuntimeError {
            msg: err.to_string(),
        });
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

/// Add a regex pattern for secret detection
/// Usage: (secret-add-pattern "MY_CUSTOM_.*")
pub fn secret_add_pattern(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::new(
            "secret-add-pattern requires at least 1 argument: pattern",
        ));
    }

    let pattern = args[0].to_string();
    env.borrow()
        .shell_env
        .read()
        .secret_manager
        .add_pattern(&pattern)
        .map_err(|e| RuntimeError::new(&e))?;

    debug!("Added secret pattern: {}", pattern);
    Ok(Value::NIL)
}

/// Add a keyword for secret detection
/// Usage: (secret-add-keyword "MY_SECRET")
pub fn secret_add_keyword(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::new(
            "secret-add-keyword requires at least 1 argument: keyword",
        ));
    }

    let keyword = args[0].to_string();
    env.borrow()
        .shell_env
        .read()
        .secret_manager
        .add_keyword(&keyword);

    debug!("Added secret keyword: {}", keyword);
    Ok(Value::NIL)
}

/// List registered secret patterns
/// Usage: (secret-list-patterns)
pub fn secret_list_patterns(
    env: Rc<RefCell<Env>>,
    _args: Vec<Value>,
) -> Result<Value, RuntimeError> {
    let patterns = env.borrow().shell_env.read().secret_manager.list_patterns();

    let result: Vec<Value> = patterns.into_iter().map(Value::String).collect();
    Ok(Value::List(result.into_iter().collect()))
}

/// Set history mode for secrets
/// Usage: (secret-history-mode "skip") ; or "redact" or "none"
pub fn secret_history_mode(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    use crate::secrets::SecretHistoryMode;

    if args.is_empty() {
        // Return current mode
        let mode = env.borrow().shell_env.read().secret_manager.history_mode();
        let mode_str = match mode {
            SecretHistoryMode::Skip => "skip",
            SecretHistoryMode::Redact => "redact",
            SecretHistoryMode::None => "none",
        };
        return Ok(Value::String(mode_str.to_string()));
    }

    let mode_str = args[0].to_string();
    let mode: SecretHistoryMode = mode_str
        .parse()
        .map_err(|e| RuntimeError::new(&format!("Invalid mode: {}", e)))?;

    env.borrow()
        .shell_env
        .read()
        .secret_manager
        .set_history_mode(mode);

    debug!("Set secret history mode to: {}", mode_str);
    Ok(Value::NIL)
}

/// Set a session-only secret
/// Usage: (secret-set "DB_PASS" "secret123")
pub fn secret_set(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.len() < 2 {
        return Err(RuntimeError::new(
            "secret-set requires 2 arguments: key and value",
        ));
    }

    let key = args[0].to_string();
    let value = args[1].to_string();

    env.borrow()
        .shell_env
        .read()
        .secret_manager
        .set_session_secret(&key, &value);

    debug!("Set session secret: {}", key);
    Ok(Value::NIL)
}

/// Get a session-only secret
/// Usage: (secret-get "DB_PASS")
pub fn secret_get(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::new("secret-get requires 1 argument: key"));
    }

    let key = args[0].to_string();

    let value = env
        .borrow()
        .shell_env
        .read()
        .secret_manager
        .get_session_secret(&key);

    match value {
        Some(v) => Ok(Value::String(v)),
        None => Ok(Value::NIL),
    }
}

/// Clear all session secrets
/// Usage: (secret-clear)
pub fn secret_clear(env: Rc<RefCell<Env>>, _args: Vec<Value>) -> Result<Value, RuntimeError> {
    env.borrow()
        .shell_env
        .read()
        .secret_manager
        .clear_session_secrets();

    debug!("Cleared all session secrets");
    Ok(Value::NIL)
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
        if !nix::unistd::isatty(0).unwrap_or(false) {
            println!("Skipping TTY-dependent test");
            return;
        }

        let args = [Value::String("ls -al".to_string())];
        let env_clone = Rc::clone(&engine.borrow().env);
        let res = sh(env_clone, args.to_vec()).await;
        assert!(res.is_ok());
        if let Ok(result) = res {
            println!("{result}");
        }
    }
}

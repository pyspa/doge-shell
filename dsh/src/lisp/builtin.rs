use crate::direnv::DirEnvironment;
use crate::lisp::model::{Env, RuntimeError, Value};
use crate::shell::Shell;
use dsh_types::Context;
use nix::sys::termios::tcgetattr;
use nix::unistd::pipe;
use std::fs::File;
use std::io::prelude::*;
use std::os::unix::io::FromRawFd;
use std::process::Command;
use std::sync::Arc;
use std::{cell::RefCell, rc::Rc};
use tracing::debug;

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
        debug!("set env {} {}", &key, &env_path);
        env.borrow().shell_env.write().paths = path_vec;
    } else {
        let val = &args[1];
        unsafe { std::env::set_var(&key, val.to_string()) };
        debug!("set env {} {}", &key, &val);
    }
    Ok(Value::NIL)
}

pub fn set_variable(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let key = &args[0];
    let val = &args[1];
    debug!("set variable {} {}", &key, &val);
    env.borrow()
        .shell_env
        .write()
        .variables
        .insert(key.to_string(), val.to_string());
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
    tokio::runtime::Handle::current().block_on(sh(env, args))
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

    let mut ctx = Context::new(shell.pid, shell.pgid, shell_tmode, true);
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
        return Err(RuntimeError::new("safety-level requires 1 argument"));
    }

    let level_str = args[0].to_string();
    let level: crate::safety::SafetyLevel = level_str
        .parse()
        .map_err(|e| RuntimeError::new(&format!("Error parsing safety level: {}", e)))?;

    debug!("setting safety level to {:?}", level);
    env.borrow().shell_env.write().safety_level = level;

    Ok(Value::NIL)
}

pub fn block_sh_no_cap(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    tokio::runtime::Handle::current().block_on(sh_no_cap(env, args))
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

    let mut ctx = Context::new(shell.pid, shell.pgid, shell_tmode, true);
    // ctx.captured_out = Some(pin);
    if let Err(err) = shell.eval_str(&mut ctx, input, false).await {
        eprintln!("error: {err}");
        return Err(RuntimeError {
            msg: err.to_string(),
        });
    }

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

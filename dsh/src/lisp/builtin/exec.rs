//! Running external programs from Lisp: `(command ...)` capturing stdout
//! through a pipe, and the two `sh` forms -- captured (`block_sh`) and
//! pass-through (`block_sh_no_cap`) -- together with the async cores that drive
//! the shell's own evaluator for them.
use super::*;

pub fn command(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let mut cmd_args: Vec<String> = Vec::new();
    for arg in args {
        if let Value::String(val) = arg {
            cmd_args.push(val.to_string());
        }
    }
    if cmd_args.is_empty() {
        return Err(RuntimeError::new("command requires at least 1 argument"));
    }
    let cmd = cmd_args.remove(0);

    // SAFETY CHECK
    {
        use crate::safety::{SafetyGuard, SafetyResult};

        // We instantiate a transient SafetyGuard here because accessing the one in Shell is difficult from Lisp Env.
        // SafetyGuard::new() is lightweight.
        let guard = SafetyGuard::new();

        let env_ref = env.borrow();
        let shell_env = env_ref.shell_env.read();
        let safety_level = shell_env.policy_state.safety_level.read();
        let allowlist = shell_env.policy_state.execute_allowlist.read();

        match guard.check_command(&safety_level, &cmd, &cmd_args, &allowlist) {
            SafetyResult::Allowed => {
                // Check passed
            }
            SafetyResult::Confirm(msg) => {
                // In Lisp execution context (non-interactive usually), we block Confirm actions.
                return Err(RuntimeError::new(&format!(
                    "SafetyGuard Blocked: {} (You can change safety-level to 'loose' to bypass)",
                    msg
                )));
            }
        }
    }

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

// Helper core for sh
pub(super) async fn sh_core(
    shell_env: Arc<parking_lot::RwLock<crate::environment::Environment>>,
    args: Vec<String>,
) -> Result<String, RuntimeError> {
    let input = args.join(" ");

    let mut shell = Shell::new(shell_env);
    shell.set_signals();
    let shell_tmode = match tcgetattr(unsafe { BorrowedFd::borrow_raw(0) }) {
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

    ctx.captured_out = Some(pin.as_raw_fd());
    if let Err(err) = shell.eval_str(&mut ctx, input, false).await {
        eprintln!("error: {err}");
        return Err(RuntimeError {
            msg: err.to_string(),
        });
    }

    // Close write end in parent to avoid hang
    drop(pin);

    let mut raw_stdout = Vec::new();
    // use File::from(pout) to read and automatically close read end
    File::from(pout).read_to_end(&mut raw_stdout).ok();

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
    Ok(output)
}

pub fn block_sh(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let mut cmd_args: Vec<String> = Vec::new();
    for arg in args {
        cmd_args.push(arg.to_string());
    }
    let shell_env = Arc::clone(&env.borrow().shell_env);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| RuntimeError { msg: e.to_string() })?;
        rt.block_on(sh_core(shell_env, cmd_args))
    })
    .join()
    .map_err(|_| RuntimeError {
        msg: "Thread panicked".to_string(),
    })?
    .map(Value::String)
}

// Helper core for sh_no_cap
async fn sh_no_cap_core(
    shell_env: Arc<parking_lot::RwLock<crate::environment::Environment>>,
    args: Vec<String>,
) -> Result<(), RuntimeError> {
    let input = args.join(" ");

    let mut shell = Shell::new(shell_env);
    shell.set_signals();
    let shell_tmode = match tcgetattr(unsafe { BorrowedFd::borrow_raw(0) }) {
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

    Ok(())
}

pub fn block_sh_no_cap(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    let mut cmd_args: Vec<String> = Vec::new();
    for arg in args {
        cmd_args.push(arg.to_string());
    }
    let shell_env = Arc::clone(&env.borrow().shell_env);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| RuntimeError { msg: e.to_string() })?;
        rt.block_on(sh_no_cap_core(shell_env, cmd_args))
    })
    .join()
    .map_err(|_| RuntimeError {
        msg: "Thread panicked".to_string(),
    })?
    .map(|_| Value::NIL)
}

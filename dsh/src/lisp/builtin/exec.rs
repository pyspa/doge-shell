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

    // SafetyGuard first, then the runtime snapshot: resolve through the
    // logical PATH and spawn with the exported child environment only.
    // Process-global PATH and inherited environment are never consulted,
    // so an unexported logical PATH still resolves and a logically unset
    // variable stays unset.
    let snapshot = {
        let shell_env = env.borrow();
        let guard = shell_env.shell_env.read();
        let current_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        guard.command_runtime_snapshot(current_dir)
    };
    let mut spawn = snapshot.std_command(&cmd).ok_or_else(|| RuntimeError {
        msg: format!("command not found: {cmd}"),
    })?;
    match spawn.args(cmd_args).output() {
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

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::environment::Environment;

    fn write_script(dir: &std::path::Path, name: &str, body: &str) {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
    }

    fn lisp_env_for(
        shell_env: std::sync::Arc<parking_lot::RwLock<Environment>>,
    ) -> Rc<RefCell<Env>> {
        crate::lisp::make_env(shell_env)
    }

    fn string_arg(value: &str) -> Value {
        Value::String(value.to_string())
    }

    /// `(command "foo")` resolves through the unexported logical PATH:
    /// process PATH B holds a different `foo`, but A runs.
    #[test]
    fn lisp_command_uses_unexported_logical_path() {
        let _guard = crate::test_env_lock();
        let root = tempfile::tempdir().unwrap();
        let logical = root.path().join("logical-bin");
        let process = root.path().join("process-bin");
        std::fs::create_dir_all(&logical).unwrap();
        std::fs::create_dir_all(&process).unwrap();
        write_script(&logical, "foo", "#!/bin/sh\nprintf 'logical-foo'\n");
        write_script(&process, "foo", "#!/bin/sh\nprintf 'process-foo'\n");
        let _process_path = crate::ProcessEnvGuard::prepend_path(&process);

        let shell_env = Environment::new();
        {
            let mut env = shell_env.write();
            env.unset_shell_var("PATH");
            env.set_shell_var("PATH".to_string(), logical.to_string_lossy().into_owned());
            assert!(!env.variable_state.exported_vars.contains("PATH"));
            assert!(!env.child_process_env().contains_key("PATH"));
        }

        let env = lisp_env_for(shell_env);
        match command(env, vec![string_arg("foo")]) {
            Ok(Value::String(output)) => assert_eq!(output, "logical-foo"),
            other => panic!("expected logical-foo output, got {other:?}"),
        }
    }

    /// A logically unset variable stays unset in the Lisp child: no
    /// resurrection from the process environment.
    #[test]
    fn lisp_command_logical_unset_stays_unset() {
        let _guard = crate::test_env_lock();
        let root = tempfile::tempdir().unwrap();
        let logical = root.path().join("logical-bin");
        std::fs::create_dir_all(&logical).unwrap();
        write_script(
            &logical,
            "showfoo",
            "#!/bin/sh\nprintf '%s' \"${DOGESH_LISP_FOO-unset}\"\n",
        );
        let _stale = crate::ProcessEnvGuard::set("DOGESH_LISP_FOO", "stale-secret");

        let shell_env = Environment::new();
        {
            let mut env = shell_env.write();
            // Startup import saw the stale value; `unset` is final.
            assert!(env.lookup_variable("DOGESH_LISP_FOO").is_some());
            env.unset_shell_var("DOGESH_LISP_FOO");
            env.unset_shell_var("PATH");
            env.set_shell_var("PATH".to_string(), logical.to_string_lossy().into_owned());
        }

        let env = lisp_env_for(shell_env);
        match command(env, vec![string_arg("showfoo")]) {
            Ok(Value::String(output)) => assert_eq!(output, "unset"),
            other => panic!("expected unset output, got {other:?}"),
        }
    }
}

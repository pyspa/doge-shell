//! Running external programs from Lisp: `(command ...)` capturing stdout
//! through a pipe, `(command-result ...)` retaining status and both streams,
//! and the two `sh` forms -- captured (`block_sh`) and
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
    let output = execute_command(env, cmd_args)?;
    let stdout = String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string();
    let stderr = String::from_utf8_lossy(&output.stderr)
        .trim_end()
        .to_string();
    Ok(Value::String(if stdout.is_empty() {
        stderr
    } else {
        stdout
    }))
}

/// Execute literal argv and retain both streams and the actual termination.
/// `exit-code` is nil on signal termination; `signal` is nil on normal exit.
/// Strings use UTF-8 replacement for invalid bytes, without whitespace trimming.
pub fn command_result(env: Rc<RefCell<Env>>, args: Vec<Value>) -> Result<Value, RuntimeError> {
    if args.is_empty() {
        return Err(RuntimeError::new(
            "command-result requires an executable string",
        ));
    }
    let cmd_args: Vec<String> = args
        .into_iter()
        .map(|value| match value {
            Value::String(value) => Ok(value),
            _ => Err(RuntimeError::new(
                "command-result arguments must all be strings",
            )),
        })
        .collect::<Result<_, _>>()?;
    if cmd_args[0].is_empty() || cmd_args.iter().any(|arg| arg.contains('\0')) {
        return Err(RuntimeError::new(
            "command-result requires a nonempty executable and no NUL bytes",
        ));
    }
    let output = execute_command(env, cmd_args)?;
    use std::os::unix::process::ExitStatusExt;
    // Only immutable string keys are stored; no interior-mutable keys are used.
    #[allow(clippy::mutable_key_type)]
    let fields = std::collections::HashMap::from([
        (
            Value::String("exit-code".into()),
            output
                .status
                .code()
                .map(|n| Value::Int(n.into()))
                .unwrap_or(Value::NIL),
        ),
        (
            Value::String("signal".into()),
            output
                .status
                .signal()
                .map(|n| Value::Int(n.into()))
                .unwrap_or(Value::NIL),
        ),
        (
            Value::String("stdout".into()),
            Value::String(String::from_utf8_lossy(&output.stdout).into_owned()),
        ),
        (
            Value::String("stderr".into()),
            Value::String(String::from_utf8_lossy(&output.stderr).into_owned()),
        ),
    ]);
    Ok(Value::HashMap(Rc::new(RefCell::new(fields))))
}

/// Both Lisp APIs share one spawn/capture/wait owner and the same authorization.
/// Command::output owns and drains both pipes, and reaps its single child.
fn execute_command(
    env: Rc<RefCell<Env>>,
    mut cmd_args: Vec<String>,
) -> Result<std::process::Output, RuntimeError> {
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
    spawn.args(cmd_args).output().map_err(|err| RuntimeError {
        msg: err.to_string(),
    })
}

// Capability comes from Context; interactive intent comes from the invocation.
fn shell_context(shell: &Shell) -> Context {
    let mut ctx = Context::new_safe(shell.pid, shell.pgid, true);
    ctx.interactive &= shell.environment.read().invocation().interactive;
    ctx.save_history = ctx.interactive;
    ctx
}

// Helper core for sh
pub(super) async fn sh_core(
    shell_env: Arc<parking_lot::RwLock<crate::environment::Environment>>,
    args: Vec<String>,
) -> Result<String, RuntimeError> {
    let input = args.join(" ");

    let mut shell = Shell::new(shell_env);
    shell.set_signals();
    let mut ctx = shell_context(&shell);
    let (pout, pin) = match crate::process::io::cloexec_pipe() {
        Ok(p) => p,
        Err(err) => {
            eprintln!("error: {err}");
            return Err(RuntimeError {
                msg: err.to_string(),
            });
        }
    };

    // Start before eval: both external children and in-process builtins can
    // fill the pipe. The reader owns its fd and is joined on every return.
    let reader = super::capture::CaptureReader::new(pout).map_err(|err| RuntimeError {
        msg: format!("sh! capture: {err}"),
    })?;
    ctx.captured_out = Some(pin.as_raw_fd());
    let evaluation = shell.eval_str(&mut ctx, input, false).await;
    ctx.captured_out = None;
    drop(pin);
    let captured = reader.finish();
    evaluation.map_err(|err| RuntimeError {
        msg: err.to_string(),
    })?;
    let raw_stdout = captured.map_err(|err| RuntimeError {
        msg: format!("sh! capture: {err}"),
    })?;

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
    let mut ctx = shell_context(&shell);
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
#[path = "command_result_tests.rs"]
mod command_result_tests;

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

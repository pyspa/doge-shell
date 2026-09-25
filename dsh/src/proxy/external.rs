//! External command execution handler.

use crate::environment::Environment;
use anyhow::{Context as _, Result};
use dsh_types::Context;
use parking_lot::RwLock;
use std::sync::Arc;
use tracing::debug;

/// Internal interpreter for complex commands.
///
/// Never resolved through logical or process-global `PATH`: a
/// project-local `sh` must not become the command interpreter.
const SYSTEM_SHELL: &str = "/bin/sh";

/// Execute an external command.
///
/// This is the fallback handler when no builtin command matches.
/// Uses `std::process::Command` for synchronous execution.
pub fn execute(
    _ctx: &Context,
    cmd: &str,
    argv: Vec<String>,
    environment: Arc<RwLock<Environment>>,
) -> Result<()> {
    // For other commands, try to execute them as external commands
    // We use std::process::Command because we are in a sync context and cannot call async eval_str
    // Note: This bypasses shell aliases/functions for now, which is a limitation of sync proxy.
    debug!("Dispatching external command: {} {:?}", cmd, argv);

    // If the command contains shell metacharacters or argv is empty (implies potentially complex cmd string passed as one arg), use sh -c
    // Simple heuristic: if argv is empty AND cmd contains space or pipe, or if cmd contains pipe/redirects.
    // Generally safe-run might pass "curl | sh" as cmd with empty argv.
    let use_shell = argv.is_empty()
        && (cmd.contains(' ') || cmd.contains('|') || cmd.contains('>') || cmd.contains('&'));

    // One immutable runtime for lookup and spawn: the logical `PATH`
    // resolves the executable first (so an unexported logical `PATH` still
    // works), and the child sees exactly the exported environment. The
    // Environment lock is released before the child runs.
    let snapshot = {
        let env = environment.read();
        let current_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        env.command_runtime_snapshot(current_dir)
    };

    let status = if use_shell {
        debug!("Detected complex command, using sh -c");
        let mut command = snapshot
            .std_command(SYSTEM_SHELL)
            .context("failed to resolve the internal shell")?;
        command.arg("-c").arg(cmd);
        command.status()
    } else {
        let mut command = snapshot
            .std_command(cmd)
            .ok_or_else(|| anyhow::anyhow!("command not found: {cmd}"))?;
        command.args(argv);
        command.status()
    };

    match status {
        Ok(status) => {
            if !status.success() {
                // We return Err to signal failure to the caller (safe-run)
                // Since dispatch returns Result<()>, we use Err for non-zero exit status if we want safe-run to know.
                // However, safe-run might want to return the exact exit code.
                // But for now, returning Err is the only way to signal "something went wrong".
                return Err(anyhow::anyhow!("Command exited with status: {}", status));
            }
        }
        Err(e) => {
            return Err(anyhow::anyhow!("Failed to execute '{}': {}", cmd, e));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn unexported_logical_path_still_resolves() {
        let _guard = crate::test_env_lock();
        let root = tempfile::tempdir().unwrap();
        let logical = root.path().join("logical-bin");
        let process = root.path().join("process-bin");
        std::fs::create_dir_all(&logical).unwrap();
        std::fs::create_dir_all(&process).unwrap();
        write_script(&logical, "foo", "#!/bin/sh\nexit 0\n");
        // A process-PATH `foo` that fails: success proves the logical one ran.
        write_script(&process, "foo", "#!/bin/sh\nexit 3\n");
        let _process_path = crate::ProcessEnvGuard::prepend_path(&process);

        let environment = Environment::new();
        {
            let mut env = environment.write();
            // Logical PATH points at A and stays unexported: the shell
            // resolves here, children never see it. `set_shell_var`
            // preserves the inherited export bit, so drop it first.
            env.unset_shell_var("PATH");
            env.set_shell_var("PATH".to_string(), logical.to_string_lossy().into_owned());
            assert!(!env.variable_state.exported_vars.contains("PATH"));
            // The child of the logical `foo` must not see any PATH at all.
            assert!(!env.child_process_env().contains_key("PATH"));
        }

        let ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), false);
        let result = execute(&ctx, "foo", vec![], Arc::clone(&environment));
        assert!(
            result.is_ok(),
            "unexported logical PATH must resolve: {result:?}"
        );
    }

    #[test]
    fn complex_commands_use_the_fixed_system_shell() {
        let environment = Environment::new();
        let ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), false);
        let result = execute(&ctx, "echo complex-ok", vec![], Arc::clone(&environment));
        assert!(result.is_ok(), "{result:?}");
    }
}

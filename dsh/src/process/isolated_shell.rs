//! Isolated `/bin/sh` builder for AI-adjacent subprocesses.
//!
//! Ordinary interactive children (`command |!`, `ShellProxy::capture_command`,
//! interactive `!` execute) materialize their environment from the shell's
//! logical `Environment` (`child_process_env`), never by inheriting the
//! process-global `std::env`. This helper owns only the interpreter boundary
//! and the environment isolation (`/bin/sh` + `env_clear` + explicit env).
//! stdio, cwd, process groups, timeouts, and job lifecycle stay with callers.
use std::collections::HashMap;
use std::process::Command;

/// Fixed system shell for internal command-line interpretation.
///
/// Not a user command lookup: it is never resolved through logical or
/// process-global PATH, so a project-local fake `sh` cannot become the AI
/// execution engine. `/bin/sh` exists on both supported platforms
/// (Linux/macOS).
const SYSTEM_SHELL: &str = "/bin/sh";

pub(crate) fn command(line: &str, child_env: &HashMap<String, String>) -> Command {
    let mut command = Command::new(SYSTEM_SHELL);
    command.arg("-c").arg(line).env_clear().envs(child_env);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_fixed_system_shell() {
        let env = HashMap::new();
        let command = command("true", &env);
        assert_eq!(command.get_program(), std::ffi::OsStr::new("/bin/sh"));
    }

    #[test]
    fn isolated_shell_does_not_inherit_process_only_environment() {
        let _lock = crate::test_env_lock();
        let _process_only = crate::ProcessEnvGuard::set("DOGESH_PROCESS_ONLY", "secret");

        let mut child_env = HashMap::new();
        child_env.insert("DOGESH_VISIBLE".to_string(), "logical".to_string());

        let output = command(
            "printf '%s|%s' \"$DOGESH_VISIBLE\" \"${DOGESH_PROCESS_ONLY-unset}\"",
            &child_env,
        )
        .output()
        .expect("isolated shell runs");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "logical|unset");
    }
}

use crate::ShellProxy;
use dsh_types::{Context, ExitStatus};
use std::collections::HashMap;
use std::process::Command;

const PROTECTED_ENV_VARS: &[&str] = &[
    "HOME", "PATH", "PWD", "OLDPWD", "SHELL", "TERM", "USER", "LOGNAME", "LANG",
];

fn shell_escape_single(input: &str) -> String {
    format!("'{}'", input.replace('\'', r"'\''"))
}

pub fn description() -> &'static str {
    "Execute a bash script and import its environment variables"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        let _ = ctx.write_stderr("usage: include <script-file> [args...]\n");
        return ExitStatus::ExitedWith(1);
    }

    let script = &argv[1];
    let args = &argv[2..];

    // Construct the command to source the script and dump environment
    // We use env -0 to handle values with newlines correctly
    let mut bash_cmd = Command::new("bash");
    bash_cmd.arg("-c");

    // We need to source the script with arguments if provided
    let escaped_script = shell_escape_single(script);
    let source_cmd = if args.is_empty() {
        format!("source {} && env -0", escaped_script)
    } else {
        // To pass arguments to the sourced script, we can use:
        // set -- arg1 arg2 ...; source script
        let args_str = args
            .iter()
            .map(|a| shell_escape_single(a))
            .collect::<Vec<_>>()
            .join(" ");
        format!("set -- {}; source {} && env -0", args_str, escaped_script)
    };

    bash_cmd.arg(&source_cmd);

    // Capture the output
    match bash_cmd.output() {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let _ = ctx.write_stderr(&format!("include: bash execution failed:\n{}", stderr));
                return ExitStatus::ExitedWith(output.status.code().unwrap_or(1));
            }

            // Parse the output (null-separated key=value pairs)
            let stdout = output.stdout;
            let mut new_env: HashMap<String, String> = HashMap::new();

            // Split by null byte
            for part in stdout.split(|&b| b == 0) {
                if part.is_empty() {
                    continue;
                }
                if let Ok(s) = std::str::from_utf8(part)
                    && let Some((key, value)) = s.split_once('=')
                {
                    new_env.insert(key.to_string(), value.to_string());
                }
            }

            // Get current environment (we can't easily get ALL current env vars from proxy without listing them)
            // But we can just set all new ones and unset ones that are missing?
            // Wait, unsetting missing ones might be dangerous if 'env -0' doesn't return everything.
            // 'env' usually returns the FULL environment.
            // So if a variable is NOT in new_env, it means it was unset/removed by the script OR it wasn't there.
            // However, we should only unset variables that WERE present in the parent shell but are NOT in the new env.
            // Since we can't easily iterate all env vars in proxy (no get_all_env_vars),
            // maybe we can improve ShellProxy interface or just rely on 'set' for now?
            //
            // "For keys in the old environment not present in the new map, call proxy.unset_env_var"
            // To do this, we need to know the *current* environment.
            // The `Context` might not have it all? `Context` has `env` but it's passed from main.
            // `env` command generally outputs the WHOLE environment of the subshell.
            // So `new_env` represents the desired state of the environment.
            //
            // If we blindly unset everything not in `new_env`, we might unset internal variables if `bash -c env` doesn't inherit them?
            // `Command::new` inherits environment by default. So `new_env` should contain everything + changes.
            //
            // So:
            // 1. Iterate over `new_env`, set key=value.
            // 2. We need to find keys that are currently set but MISSING in `new_env`.
            //    To do this, we really need `std::env::vars()` from the current process context.
            //    Wait, `Command::new` is spawned from the current process, so it inherits `std::env::vars()`.
            //    So `new_env` is a superset (or subset if unset) of `std::env::vars()`.
            //
            //    So we can iterate `std::env::vars()` of the *current* process.
            //    For each key in `std::env::vars()`:
            //      If not in `new_env`, unset it.

            // Apply changes
            for (key, value) in &new_env {
                // Determine if we need to update
                // We can just call set_env_var, it's cheap enough.
                // Optimization: check if value changed?
                if std::env::var(key).unwrap_or_default() != *value {
                    proxy.set_env_var(key.clone(), value.clone());
                }
            }

            // Handle Unset
            // We iterate over CURRENT environment variables
            for (key, _) in std::env::vars() {
                if !new_env.contains_key(&key)
                    && !PROTECTED_ENV_VARS.contains(&key.as_str())
                    && !key.starts_with("DSH_")
                {
                    // It was removed in the subshell
                    proxy.unset_env_var(&key);
                }
            }

            ExitStatus::ExitedWith(0)
        }
        Err(e) => {
            let _ = ctx.write_stderr(&format!("include: failed to execute bash: {}\n", e));
            ExitStatus::ExitedWith(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestShellProxy;
    use std::sync::{LazyLock, Mutex};

    // Mutex to prevent test races on environment variables
    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn env_mirroring_proxy() -> TestShellProxy {
        TestShellProxy {
            confirm_result: true,
            // This suite intentionally checks that `include`d `export`/`unset`
            // reach the real process environment, so it opts into
            // TestShellProxy's real-env mirroring rather than only asserting
            // on the fake's own state.
            mutate_real_env: true,
            capture_command_response: Some((0, String::new(), String::new())),
            open_editor_response: Some(String::new()),
            ..TestShellProxy::default()
        }
    }

    #[test]
    fn test_include_command() {
        let _lock = ENV_LOCK.lock().unwrap();
        use std::io::Write;
        let mut proxy = env_mirroring_proxy();
        let ctx = Context::new_safe(
            nix::unistd::Pid::from_raw(0),
            nix::unistd::Pid::from_raw(0),
            true,
        );

        // Create a temporary script
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "export TEST_INCLUDE_VAR='Hello from Test'").unwrap();
        let path = file.path().to_str().unwrap().to_string();

        let argv = vec!["include".to_string(), path];
        let status = command(&ctx, argv, &mut proxy);

        assert!(matches!(status, ExitStatus::ExitedWith(0)));
        assert_eq!(
            std::env::var("TEST_INCLUDE_VAR").unwrap(),
            "Hello from Test"
        );
        // Clean up
        unsafe { std::env::remove_var("TEST_INCLUDE_VAR") };
    }

    #[test]
    fn test_include_unset() {
        let _lock = ENV_LOCK.lock().unwrap();
        use std::io::Write;
        let mut proxy = env_mirroring_proxy();
        let ctx = Context::new_safe(
            nix::unistd::Pid::from_raw(0),
            nix::unistd::Pid::from_raw(0),
            true,
        );

        // Set variable first
        unsafe { std::env::set_var("TEST_UNSET_VAR", "Should be gone") };
        proxy.set_env_var("TEST_UNSET_VAR".to_string(), "Should be gone".to_string());

        // Create a temporary script
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "unset TEST_UNSET_VAR").unwrap();
        let path = file.path().to_str().unwrap().to_string();

        let argv = vec!["include".to_string(), path];
        let status = command(&ctx, argv, &mut proxy);

        assert!(matches!(status, ExitStatus::ExitedWith(0)));
        assert!(std::env::var("TEST_UNSET_VAR").is_err());
    }
}

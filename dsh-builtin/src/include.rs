use crate::ShellProxy;
use dsh_types::{Context, ExitStatus};
use std::collections::HashMap;

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
        let _ = ctx.write_stderr("usage: include <script-file> [args...]");
        return ExitStatus::ExitedWith(1);
    }

    let script = &argv[1];
    let args = &argv[2..];

    // Shell-runtime environment only: bash must not implicitly inherit the
    // dogesh process-global environment.
    let before = proxy.child_process_environment();

    // `bash` resolves through the logical runtime (never the
    // process-global PATH); the snapshot already isolates the environment,
    // and the explicit env below keeps the before/after diff source obvious.
    let mut bash_cmd = match crate::runtime_spawn::runtime_command(proxy, "bash") {
        Ok(command) => command,
        Err(e) => {
            let _ = ctx.write_stderr(&format!("include: bash execution failed:\n{e}"));
            return ExitStatus::ExitedWith(1);
        }
    };
    bash_cmd.env_clear().envs(&before);
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

            // Diff against the shell-runtime snapshot bash was given:
            // added/changed keys are published as exported shell variables,
            // missing keys are logically unset. `PROTECTED_ENV_VARS` and
            // `DOGESH_*` are never unset by a sourced script.
            for (key, value) in &new_env {
                if before.get(key) != Some(value) {
                    proxy.set_env_var(key.clone(), value.clone());
                }
            }

            // Handle Unset: keys bash was given but no longer reports.
            for key in before.keys() {
                if !new_env.contains_key(key)
                    && !PROTECTED_ENV_VARS.contains(&key.as_str())
                    && !key.starts_with("DOGESH_")
                {
                    // It was removed in the subshell
                    proxy.unset_env_var(key);
                }
            }

            ExitStatus::ExitedWith(0)
        }
        Err(e) => {
            let _ = ctx.write_stderr(&format!("include: failed to execute bash: {}", e));
            ExitStatus::ExitedWith(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell_capabilities::ProcessEnvironmentCapability;
    use crate::test_support::TestShellProxy;

    fn fake_proxy() -> TestShellProxy {
        TestShellProxy {
            confirm_result: true,
            capture_command_response: Some((0, String::new(), String::new())),
            open_editor_response: Some(String::new()),
            // `bash` resolves through the proxy's logical PATH: point it
            // at the runner's own PATH so the real `bash` is found, without
            // letting production code read the process environment.
            command_search_paths: std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            )
            .collect(),
            ..TestShellProxy::default()
        }
    }

    fn test_ctx() -> Context {
        Context::new_safe(
            nix::unistd::Pid::from_raw(0),
            nix::unistd::Pid::from_raw(0),
            true,
        )
    }

    #[test]
    fn test_include_command() {
        use std::io::Write;
        let mut proxy = fake_proxy();
        let ctx = test_ctx();

        // Create a temporary script
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "export TEST_INCLUDE_VAR='Hello from Test'").unwrap();
        let path = file.path().to_str().unwrap().to_string();

        let argv = vec!["include".to_string(), path];
        let status = command(&ctx, argv, &mut proxy);

        assert!(matches!(status, ExitStatus::ExitedWith(0)));
        assert_eq!(
            proxy.get_var("TEST_INCLUDE_VAR").as_deref(),
            Some("Hello from Test")
        );
        assert_eq!(
            proxy.child_process_environment().get("TEST_INCLUDE_VAR"),
            Some(&"Hello from Test".to_string())
        );
    }

    #[test]
    fn test_include_unset() {
        use std::io::Write;
        let mut proxy = fake_proxy();
        let ctx = test_ctx();

        // Set variable first (exported, as the shell would hold it).
        proxy.set_env_var("TEST_UNSET_VAR".to_string(), "Should be gone".to_string());

        // Create a temporary script
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "unset TEST_UNSET_VAR").unwrap();
        let path = file.path().to_str().unwrap().to_string();

        let argv = vec!["include".to_string(), path];
        let status = command(&ctx, argv, &mut proxy);

        assert!(matches!(status, ExitStatus::ExitedWith(0)));
        assert!(proxy.get_var("TEST_UNSET_VAR").is_none());
        assert!(
            !proxy
                .child_process_environment()
                .contains_key("TEST_UNSET_VAR")
        );
    }

    #[test]
    fn include_receives_a_shell_runtime_exported_variable() {
        use std::io::Write;
        let mut proxy = fake_proxy();
        let ctx = test_ctx();
        proxy.set_env_var("DOGESH_INCLUDE_SEED".to_string(), "seed-value".to_string());

        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "echo \"seed=$DOGESH_INCLUDE_SEED\"").unwrap();
        // `include` only imports the environment; assert the seed reached
        // bash by echoing it into an exported name.
        writeln!(file, "export DOGESH_INCLUDE_SEEN=\"$DOGESH_INCLUDE_SEED\"").unwrap();
        let path = file.path().to_str().unwrap().to_string();

        let status = command(&ctx, vec!["include".to_string(), path], &mut proxy);
        assert!(matches!(status, ExitStatus::ExitedWith(0)));
        assert_eq!(
            proxy.get_var("DOGESH_INCLUDE_SEEN").as_deref(),
            Some("seed-value")
        );
    }

    #[test]
    fn include_export_reaches_child_environment() {
        use std::io::Write;
        let mut proxy = fake_proxy();
        let ctx = test_ctx();

        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "export DOGESH_INCLUDE_CHILD='child-value'").unwrap();
        let path = file.path().to_str().unwrap().to_string();

        let status = command(&ctx, vec!["include".to_string(), path], &mut proxy);
        assert!(matches!(status, ExitStatus::ExitedWith(0)));
        assert_eq!(
            proxy.get_var("DOGESH_INCLUDE_CHILD").as_deref(),
            Some("child-value")
        );
        assert_eq!(
            proxy
                .child_process_environment()
                .get("DOGESH_INCLUDE_CHILD"),
            Some(&"child-value".to_string())
        );
    }

    #[test]
    fn process_global_env_does_not_leak_into_include() {
        let _lock = crate::chatgpt::tool::execute::tests::env_lock();
        use std::io::Write;
        // A process-global variable added after the shell started must not
        // leak into the `bash` `include` spawns (it uses `env_clear`).
        unsafe { std::env::set_var("DOGESH_INCLUDE_STALE", "stale") };
        let mut proxy = fake_proxy();
        let ctx = test_ctx();

        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            file,
            "if [ -z \"${{DOGESH_INCLUDE_STALE:-}}\" ]; then export DOGESH_INCLUDE_PROBE=absent; else export DOGESH_INCLUDE_PROBE=leaked; fi"
        )
        .unwrap();
        let path = file.path().to_str().unwrap().to_string();

        let status = command(&ctx, vec!["include".to_string(), path], &mut proxy);
        unsafe { std::env::remove_var("DOGESH_INCLUDE_STALE") };
        assert!(matches!(status, ExitStatus::ExitedWith(0)));
        assert_eq!(
            proxy.get_var("DOGESH_INCLUDE_PROBE").as_deref(),
            Some("absent")
        );
    }
}

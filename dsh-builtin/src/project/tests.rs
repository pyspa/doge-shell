//! Tests for `pm` subcommands: dotenv/envrc parsing, activation safety gates, mise provider detection, and the JSON status shape.
use super::*;
use crate::test_support::{ProcessEnvGuard, TestShellProxy as TestProxy};
use dsh_types::observed_output::ObservedOutput;
use std::io::Write;
use std::os::fd::IntoRawFd;
use std::os::unix::fs::PermissionsExt;

fn observed_context() -> (Context, dsh_types::observed_output::SharedOutputObserver) {
    let mut ctx = Context::new_safe(nix::unistd::getpid(), nix::unistd::getpid(), false);
    let observer = ObservedOutput::shared(8192);
    ctx.output_observer = Some(observer.clone());
    ctx.outfile = std::fs::File::create("/dev/null").unwrap().into_raw_fd();
    ctx.errfile = std::fs::File::create("/dev/null").unwrap().into_raw_fd();
    (ctx, observer)
}

fn observed_stdout(observer: &dsh_types::observed_output::SharedOutputObserver) -> String {
    observer.lock().unwrap().snapshot().stdout
}

#[test]
fn dotenv_parser_accepts_export_and_quotes() {
    assert_eq!(
        parse_assignment_line("export FOO=\"bar baz\""),
        Some(("FOO".to_string(), "bar baz".to_string()))
    );
    assert_eq!(
        parse_assignment_line("BAD-NAME=value"),
        None,
        "invalid shell env names should be skipped"
    );
}

#[test]
fn envrc_parser_only_collects_safe_forms() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "export FOO=bar").unwrap();
    writeln!(file, "path_add ./bin").unwrap();
    writeln!(file, "source ./danger.sh").unwrap();

    let activation = parse_envrc_file(file.path()).unwrap();
    assert_eq!(
        activation,
        EnvrcActivation {
            vars: vec![("FOO".to_string(), "bar".to_string())],
            path_adds: vec!["./bin".to_string()],
        }
    );
}

#[test]
fn project_name_from_path_falls_back_to_project() {
    assert_eq!(project_name_from_path(Path::new("/tmp/demo")), "demo");
    assert_eq!(project_name_from_path(Path::new("/")), "project");
}

#[test]
fn activation_safety_detects_sensitive_env_and_outside_path() {
    assert!(env_assignment_requires_confirmation("API_KEY", "abc123"));
    assert!(env_assignment_requires_confirmation(
        "LD_PRELOAD",
        "/tmp/hook.so"
    ));
    assert!(!env_assignment_requires_confirmation("APP_MODE", "dev"));
    assert!(activation_path_outside_root(
        Path::new("/tmp/project"),
        "../bin"
    ));
    assert!(!activation_path_outside_root(
        Path::new("/tmp/project"),
        "./bin"
    ));
}

#[test]
fn task_source_counts_groups_by_source() {
    let tasks = vec![
        task::TaskInfo::new("cargo", "test", "cargo test", "/tmp"),
        task::TaskInfo::new("cargo", "check", "cargo check", "/tmp"),
        task::TaskInfo::new("npm", "build", "npm run build", "/tmp"),
    ];

    let counts = task_source_counts(&tasks);
    assert_eq!(counts.get("cargo"), Some(&2));
    assert_eq!(counts.get("npm"), Some(&1));
}

#[test]
fn help_text_mentions_onboarding_commands() {
    let help = help_text();
    assert!(help.contains("pm init"));
    assert!(help.contains("status"));
    assert!(help.contains("activate"));
    assert!(help.contains("--dry-run"));
}

#[test]
fn activate_dry_run_masks_values_and_does_not_mutate_environment() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{\"name\":\"demo\"}").unwrap();
    std::fs::write(dir.path().join(".env"), "API_KEY=secret\nAPP_MODE=dev\n").unwrap();
    std::fs::write(
        dir.path().join(".envrc"),
        "export SERVICE_TOKEN=token\npath_add ../bin\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join(".venv/bin")).unwrap();

    let mut proxy = TestProxy {
        current_dir: dir.path().to_path_buf(),
        direnv_allowed: true,
        ..TestProxy::default()
    };
    let (ctx, observer) = observed_context();

    let status = command(
        &ctx,
        vec![
            "pm".to_string(),
            "activate".to_string(),
            "--dry-run".to_string(),
        ],
        &mut proxy,
    );

    assert_eq!(status, ExitStatus::ExitedWith(0));
    assert_eq!(proxy.set_env_calls, 0);
    assert_eq!(proxy.insert_path_calls, 0);

    let output = observed_stdout(&observer);
    assert!(output.contains(".env set API_KEY=*** confirm"));
    assert!(output.contains(".env set APP_MODE=dev"));
    assert!(output.contains(".envrc set SERVICE_TOKEN=*** confirm"));
    assert!(output.contains("confirm-outside-root"));
    assert!(output.contains("venv path_add"));
    assert!(output.contains("activation safety env_vars=3 confirm_vars=2"));
    assert!(!output.contains("secret"));
    assert!(!output.contains("SERVICE_TOKEN=token"));
}

#[test]
fn mise_activation_checks_trust_disables_hooks_and_dry_run_does_not_mutate() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("mise.toml"), "[tools]\nnode = '22'\n").unwrap();
    let executable = dir.path().join("mise-fake");
    std::fs::write(
        &executable,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$PWD/mise-args.log"
if [ "$1" = "trust" ]; then printf '%s\n' "$PWD: trusted"; exit 0; fi
if [ "$1" = "--no-hooks" ] && [ "$2" = "ls" ]; then printf '%s\n' '[{"name":"python"}]'; exit 0; fi
if [ "$1" = "--no-hooks" ] && [ "$2" = "env" ]; then
  printf '%s\n' '{"PATH":"/tmp/mise/bin","API_KEY":"secret"}'
  exit 0
fi
exit 1
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&executable, permissions).unwrap();

    let status = MiseStatus::detect_with_executable(dir.path(), Some(executable));
    assert_eq!(status.trust, "trusted");
    assert_eq!(status.missing_tools, vec!["python"]);

    let mut proxy = TestProxy {
        current_dir: dir.path().to_path_buf(),
        direnv_allowed: false,
        ..TestProxy::default()
    };
    let (ctx, observer) = observed_context();
    activate_mise(&ctx, &mut proxy, dir.path(), &status, true).unwrap();
    assert_eq!(proxy.set_env_calls, 0);
    let output = observed_stdout(&observer);
    assert!(output.contains("API_KEY=***"));
    assert!(!output.contains("API_KEY=secret"));
    let args = std::fs::read_to_string(dir.path().join("mise-args.log")).unwrap();
    assert!(args.contains("trust --show"));
    assert!(args.contains("--no-hooks ls --missing --json"));
    assert!(args.contains("--no-hooks env --json"));
    assert!(
        !args
            .lines()
            .any(|line| line.starts_with("trust") && line != "trust --show")
    );
}

#[test]
fn safe_mise_config_requires_only_plain_tools_and_non_templated_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("mise.toml");
    std::fs::write(
        &config,
        "min_version = '2026.1.0'\n[tools]\nnode = ['22', '24']\n[tasks.test]\nrun = 'cargo test'\n",
    )
    .unwrap();
    assert!(mise_config_is_safe(dir.path()));

    std::fs::write(&config, "[env]\nTOKEN = 'secret'\n").unwrap();
    assert!(!mise_config_is_safe(dir.path()));

    std::fs::write(&config, "[tasks.test]\nrun = 'echo {{env.HOME}}'\n").unwrap();
    assert!(!mise_config_is_safe(dir.path()));

    std::fs::write(&config, "[tasks.test.tools]\nnode = '22'\n").unwrap();
    assert!(!mise_config_is_safe(dir.path()));
}

#[test]
fn missing_mise_tools_accepts_current_object_json_shape() {
    let mut tools = Vec::new();
    collect_missing_tool_names(
        &serde_json::json!({
            "node": [{"version": "22", "installed": false}],
            "python": []
        }),
        &mut tools,
    );
    assert_eq!(tools, vec!["node"]);
}

#[test]
fn project_status_json_reports_provider_lock_and_dev_container_shape() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{\"name\":\"demo\"}").unwrap();
    std::fs::create_dir_all(dir.path().join(".devcontainer")).unwrap();
    std::fs::write(dir.path().join(".devcontainer/devcontainer.json"), "{}").unwrap();
    let context = project_context::resolve_project_context(dir.path());
    let status = build_project_status(&context, &[]);
    let value = serde_json::to_value(status).unwrap();
    assert_eq!(value["provider"], "native");
    assert_eq!(value["trust"], "not-configured");
    assert!(value["missing_tools"].is_array());
    assert!(
        value["dev_container"]
            .as_str()
            .unwrap()
            .ends_with(".devcontainer/devcontainer.json")
    );
}

/// A logically unset shell `PATH` stays unset: activation prepends onto the
/// shell value alone and must not resurrect a stale process `PATH`.
#[test]
fn prepend_path_does_not_resurrect_a_process_only_path() {
    let _lock = crate::chatgpt::tool::execute::tests::env_lock();
    let _guard = ProcessEnvGuard::set("PATH", "/old/process/path");
    let mut proxy = TestProxy::default();
    let root = Path::new("/proj");

    assert!(prepend_path(&mut proxy, root, "/project/bin"));
    assert_eq!(proxy.vars.get("PATH"), Some(&"/project/bin".to_string()));
}

/// A shell `PATH` is the base activation prepends onto.
#[test]
fn prepend_path_builds_on_the_shell_path() {
    let _lock = crate::chatgpt::tool::execute::tests::env_lock();
    let _guard = ProcessEnvGuard::set("PATH", "/old/process/path");
    let mut proxy = TestProxy::default();
    proxy
        .vars
        .insert("PATH".to_string(), "/shell/path".to_string());
    let root = Path::new("/proj");

    assert!(prepend_path(&mut proxy, root, "/project/bin"));
    assert_eq!(
        proxy.vars.get("PATH"),
        Some(&"/project/bin:/shell/path".to_string())
    );
}

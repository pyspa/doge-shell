//! User-facing provider/model selection without real credentials or requests.
mod common;
use common::process::{DEFAULT_CASE_TIMEOUT, contract_env, wait_child_with_output_deadline};
use std::{
    io::Write,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
};

fn run(script: &str, interactive: bool, extra: &[(&str, &str)]) -> std::process::Output {
    let _guard = common::serial_guard();
    let temp = tempfile::tempdir().unwrap();
    // macOS temporary roots can pass through /var -> /private/var. Auth
    // storage rejects symlink ancestors, so use the physical fixture paths.
    let mut environment = contract_env(&temp);
    for (key, value) in &mut environment {
        if key == "HOME" || key.starts_with("XDG_") {
            *value = std::fs::canonicalize(&*value)
                .unwrap()
                .to_string_lossy()
                .into();
        }
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_dogesh"));
    command
        .env_clear()
        .envs(environment)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .envs(extra.iter().copied())
        .current_dir(temp.path())
        .process_group(0)
        .stdin(if interactive {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !interactive {
        command.args(["-c", script]);
    }
    let mut child = command.spawn().unwrap();
    if interactive {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{script}\nexit\n").as_bytes())
            .unwrap();
    }
    let pgid = nix::unistd::Pid::from_raw(child.id() as i32);
    wait_child_with_output_deadline(child, pgid, DEFAULT_CASE_TIMEOUT).unwrap()
}

#[test]
fn subscription_selection_reaches_interactive_chat_readiness() {
    let script =
        "chat_provider chatgpt\nchat_model mock-catalog-slug\nchat_provider\nchat_model\n! hello";
    let output = run(script, true, &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("AI provider: chatgpt_subscription"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Current OpenAI model: mock-catalog-slug"),
        "{stdout}"
    );
    assert!(stderr.contains("chat_auth login"), "{stderr}");
    assert!(!stderr.contains("Set AI_CHAT_API_KEY"), "{stderr}");
}

#[test]
fn subscription_environment_alias_reaches_noninteractive_chat_without_api_fallback() {
    let output = run(
        "! hello",
        false,
        &[
            ("AI_CHAT_PROVIDER", "openai-subscription"),
            ("AI_CHAT_SUBSCRIPTION_MODEL", "mock-catalog-slug"),
            ("OPENAI_API_KEY", "mock-must-not-fallback"),
        ],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("chat_auth login"), "{stderr}");
    assert!(!stderr.contains("Set AI_CHAT_API_KEY"), "{stderr}");
}

#[test]
fn provider_and_model_commands_work_in_noninteractive_shell() {
    let output = run(
        "chat_provider chatgpt; chat_model mock-catalog-slug; chat_provider; chat_model",
        false,
        &[],
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("AI provider: chatgpt_subscription"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Current OpenAI model: mock-catalog-slug"),
        "{stdout}"
    );
}

#[test]
fn auth_status_is_available_when_subscription_transport_is_invalid() {
    let output = run(
        "set AI_CHAT_PROVIDER chatgpt_subscription; set AI_CHAT_BASE_URL https://custom.example/v1; chat_auth status",
        false,
        &[],
    );
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("chat_auth login"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("custom base"));
}

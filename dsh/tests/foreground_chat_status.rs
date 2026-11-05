//! Public evaluation status with an isolated, offline chat provider.
mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

fn run_worker(outcome: &str) {
    let _guard = common::serial_guard();
    let root = tempfile::tempdir().unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "foreground_chat_status_worker",
            "--ignored",
            "--nocapture",
        ])
        .env_clear()
        .envs(common::process::contract_env(&root))
        .env("DOGESH_CHAT_STATUS_OUTCOME", outcome)
        .env("AI_CHAT_API_KEY", "synthetic-offline-key")
        .env("AI_CHAT_PROVIDER", "api_key")
        .env("AI_CHAT_API", "chat_completions")
        .env("AI_CHAT_MODEL", "offline-model")
        .env("AI_CHAT_ALLOW_INSECURE_HTTP", "1")
        .env("AI_CHAT_STREAM", "0")
        .env("AI_CHAT_HOOKS", "off")
        .env("AI_CHAT_SKILL_REFLECT", "0")
        .current_dir(root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let pgid = nix::unistd::Pid::from_raw(child.id() as i32);
    let output =
        common::process::wait_child_with_output_deadline(child, pgid, Duration::from_secs(20))
            .unwrap();
    assert!(
        output.status.success(),
        "{outcome}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn foreground_chat_success_replaces_failure_status() {
    run_worker("success");
}

#[test]
fn foreground_chat_failure_replaces_success_status() {
    run_worker("failure");
}

#[test]
#[ignore = "runs only in an isolated child with synthetic credentials"]
fn foreground_chat_status_worker() {
    let success = std::env::var("DOGESH_CHAT_STATUS_OUTCOME").unwrap() == "success";
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let provider = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() < 64 * 1024, "fixture header limit");
        }
        let headers = String::from_utf8(request).unwrap().to_ascii_lowercase();
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse()
            .unwrap();
        assert!(length < 2 * 1024 * 1024, "fixture body limit");
        stream.read_exact(&mut vec![0; length]).unwrap();
        let (status, body) = if success {
            (
                "200 OK",
                r#"{"choices":[{"message":{"role":"assistant","content":"offline answer"},"finish_reason":"stop"}]}"#,
            )
        } else {
            (
                "401 Unauthorized",
                r#"{"error":{"message":"offline failure"}}"#,
            )
        };
        write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let environment = doge_shell::environment::Environment::new();
        environment
            .write()
            .set_shell_var("AI_CHAT_BASE_URL".into(), endpoint);
        let mut shell = doge_shell::shell::Shell::new(environment);
        let mut ctx = dsh_types::Context::new_safe(shell.pid, shell.pgid, false);
        ctx.interactive = false;
        let (seed, prior, expected) = if success {
            (common::false_path().into(), 1, 0)
        } else {
            ("PRIOR=success".into(), 0, 1)
        };
        assert_eq!(shell.eval_str(&mut ctx, seed, false).await.unwrap(), prior);
        let code = shell
            .eval_str(&mut ctx, "  ! offline status".into(), false)
            .await
            .unwrap();
        assert_eq!(code, expected, "existing normalized return code");
        assert_eq!(
            shell
                .eval_str(&mut ctx, "OBSERVED=$?".into(), false)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            shell.environment.read().get_var("OBSERVED"),
            Some(expected.to_string())
        );
        assert_eq!(
            shell
                .eval_str(&mut ctx, "AFTER=$?".into(), false)
                .await
                .unwrap(),
            0
        );
        assert_eq!(shell.environment.read().get_var("AFTER"), Some("0".into()));
    });
    provider.join().unwrap();
}

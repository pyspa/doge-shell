//! Tests for AI features.

use super::*;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Mutex;

struct MockAiService {
    response: String,
    last_messages: Mutex<Vec<Value>>,
}

impl MockAiService {
    fn new(response: &str) -> Self {
        Self {
            response: response.to_string(),
            last_messages: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl AiService for MockAiService {
    async fn send_request(
        &self,
        messages: Vec<Value>,
        _temperature: Option<f64>,
    ) -> Result<String> {
        *self.last_messages.lock().unwrap() = messages;
        Ok(self.response.clone())
    }
}

#[tokio::test]
async fn test_expand_smart_pipe() {
    let response = r#"{"command": "grep", "args": ["foo"]}"#;
    let service = MockAiService::new(response);
    let result = expand_smart_pipe(&service, "extract foo").await.unwrap();
    assert_eq!(result, "grep foo");

    let messages = service.last_messages.lock().unwrap();
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "extract foo");
}

#[tokio::test]
async fn test_run_generative_command() {
    let response = r#"{"command": "git", "args": ["status"]}"#;
    let service = MockAiService::new(response);
    let result = run_generative_command(&service, "check status")
        .await
        .unwrap();
    assert_eq!(result, "git status");
}

#[test]
fn test_sanitize_code_block() {
    assert_eq!(sanitize_code_block("ls -l"), "ls -l");
    assert_eq!(sanitize_code_block("`ls -l`"), "ls -l");
    assert_eq!(sanitize_code_block("```bash\nls -l\n```"), "ls -l\n");
}

#[test]
fn test_sanitize_code_block_edge_cases() {
    // Empty string
    assert_eq!(sanitize_code_block(""), "");

    // Only backticks
    assert_eq!(sanitize_code_block("``"), "");
    assert_eq!(sanitize_code_block("````"), "");

    // Mixed backticks without bash prefix
    assert_eq!(sanitize_code_block("```\necho test\n```"), "\necho test\n");

    // Single backtick at start
    assert_eq!(sanitize_code_block("`echo hello"), "echo hello");

    // Command with special characters
    assert_eq!(
        sanitize_code_block("`find . -name '*.rs'`"),
        "find . -name '*.rs'"
    );

    // Multiline command
    assert_eq!(
        sanitize_code_block("```bash\necho line1\necho line2\n```"),
        "echo line1\necho line2\n"
    );
}

#[tokio::test]
async fn test_run_generative_command_complex() {
    let response = r#"{"command": "find", "args": [".", "-name", "*.rs", "-exec", "grep", "-l", "TODO", "{}", "+"]}"#;
    let service = MockAiService::new(response);
    let result = run_generative_command(&service, "find all rust files with TODO")
        .await
        .unwrap();
    assert_eq!(result, "find . -name *.rs -exec grep -l TODO {} +");
}

#[tokio::test]
async fn test_run_generative_command_japanese() {
    let response = r#"{"command": "git", "args": ["reset", "--soft", "HEAD~1"]}"#;
    let service = MockAiService::new(response);
    let result = run_generative_command(&service, "最後のコミットを取り消す")
        .await
        .unwrap();
    assert_eq!(result, "git reset --soft HEAD~1");
}

#[tokio::test]
async fn test_fix_command() {
    let response = r#"{"command": "ls", "args": ["-la"]}"#;
    let service = MockAiService::new(response);
    let result = fix_command(&service, "lss -la", 127, "").await.unwrap();
    assert_eq!(result, "ls -la");

    let messages = service.last_messages.lock().unwrap();
    assert_eq!(messages[1]["role"], "user");
    assert!(messages[1]["content"].as_str().unwrap().contains("lss -la"));
}

#[tokio::test]
async fn test_fix_command_with_code_block() {
    // AI might return wrapped in backticks
    let response = r#"```json
{"command": "git", "args": ["status"]}
```"#;
    let service = MockAiService::new(response);
    let result = fix_command(&service, "gti status", 127, "").await.unwrap();
    assert_eq!(result, "git status");
}

#[tokio::test]
async fn test_diagnose_output_truncation() {
    let service = MockAiService::new("Output was truncated due to length");
    let long_output = "x".repeat(5000);
    let result = diagnose_output(&service, "cat file", &long_output, 0).await;

    assert!(result.is_ok());

    let messages = service.last_messages.lock().unwrap();
    let content = messages[1]["content"].as_str().unwrap();
    // Output should be truncated at 4000 chars
    assert!(content.contains("...(truncated)"));
}

#[tokio::test]
async fn test_explain_command() {
    let service = MockAiService::new("This command lists files");
    let result = explain_command(&service, "ls -la").await.unwrap();
    assert_eq!(result, "This command lists files");

    let messages = service.last_messages.lock().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert!(messages[1]["content"].as_str().unwrap().contains("ls -la"));
}

#[tokio::test]
async fn test_check_safety() {
    let service = MockAiService::new("**Dangerous**: This command will delete all files");
    let result = check_safety(&service, "rm -rf /").await.unwrap();
    assert!(result.contains("Dangerous"));

    let messages = service.last_messages.lock().unwrap();
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("rm -rf /")
    );
}

#[tokio::test]
async fn test_diagnose_output() {
    let service = MockAiService::new("Command not found. Try installing git.");
    let result = diagnose_output(&service, "gti status", "command not found: gti", 127)
        .await
        .unwrap();
    assert!(result.contains("not found"));

    let messages = service.last_messages.lock().unwrap();
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("Exit code: 127")
    );
}

#[tokio::test]
async fn test_analyze_output() {
    let service =
        MockAiService::new("The connection was refused because the server is not running.");
    let result = analyze_output(
        &service,
        "curl http://localhost:8080",
        "curl: (7) Failed to connect to localhost port 8080: Connection refused",
        "何が問題か教えて",
    )
    .await
    .unwrap();
    assert!(result.contains("refused"));

    let messages = service.last_messages.lock().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("curl http://localhost:8080")
    );
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("Connection refused")
    );
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("何が問題か教えて")
    );
}

#[tokio::test]
async fn test_analyze_output_truncation() {
    let service = MockAiService::new("Output was truncated due to length");

    // Create a very long output > 8000 chars
    let long_output = "x".repeat(10000);
    let result = analyze_output(&service, "cat largefile", &long_output, "summarize").await;

    assert!(result.is_ok());

    // Verify that the output was truncated in the request
    let messages = service.last_messages.lock().unwrap();
    let content = messages[1]["content"].as_str().unwrap();
    assert!(content.contains("...(truncated)"));
    // Should be shorter than original
    assert!(content.len() < 10000);
}

#[tokio::test]
async fn test_analyze_output_empty() {
    let service = MockAiService::new("The command produced no output");
    let result = analyze_output(&service, "true", "", "why is there no output?").await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "The command produced no output");

    let messages = service.last_messages.lock().unwrap();
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("why is there no output?")
    );
}

#[tokio::test]
async fn test_analyze_output_with_stderr() {
    let service = MockAiService::new("The error suggests a permission problem with /etc/hosts");
    let result = analyze_output(
        &service,
        "cat /etc/shadow",
        "cat: /etc/shadow: Permission denied",
        "what went wrong?",
    )
    .await;

    assert!(result.is_ok());
    let messages = service.last_messages.lock().unwrap();
    assert!(
        messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("Permission denied")
    );
}

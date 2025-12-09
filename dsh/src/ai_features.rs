use anyhow::Result;
use async_trait::async_trait;
use dsh_openai::ChatGptClient;
use serde_json::{Value, json};

#[async_trait]
pub trait AiService: Send + Sync {
    async fn send_request(&self, messages: Vec<Value>, temperature: Option<f64>) -> Result<String>;
}

pub struct LiveAiService {
    client: ChatGptClient,
}

impl LiveAiService {
    pub fn new(client: ChatGptClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl AiService for LiveAiService {
    async fn send_request(&self, messages: Vec<Value>, temperature: Option<f64>) -> Result<String> {
        let response = self
            .client
            .send_chat_request(&messages, temperature, None, None)?;

        response
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(|s| s.trim().to_string())
            .ok_or_else(|| anyhow::anyhow!("Failed to parse AI response"))
    }
}

pub fn sanitize_code_block(content: &str) -> String {
    let content = content.trim_matches(|c| c == '`');
    if let Some(stripped) = content.strip_prefix("bash\n") {
        stripped.to_string()
    } else {
        content.to_string()
    }
}

pub async fn expand_smart_pipe<S: AiService + ?Sized>(service: &S, query: &str) -> Result<String> {
    let system_prompt = "You are a shell command expert. The user wants to extend a shell pipeline. \
    Given the user's natural language query, output ONLY the next command(s) in the pipeline starting with a command name (e.g. 'grep', 'awk', 'jq'). \
    Do not output the pipe symbol '|' at the beginning. Do not output markdown code blocks. Output ONLY the command code.";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    let content = service.send_request(messages, Some(0.1)).await?;
    Ok(sanitize_code_block(&content))
}

pub async fn run_generative_command<S: AiService + ?Sized>(
    service: &S,
    query: &str,
) -> Result<String> {
    let system_prompt = "You are a shell command expert. Convert the following natural language request into a single-line shell command. \
    Output ONLY the command code. Do not include markdown code blocks. Do not include explanations.";

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    let content = service.send_request(messages, Some(0.1)).await?;
    Ok(sanitize_code_block(&content))
}

pub async fn fix_command<S: AiService + ?Sized>(
    service: &S,
    command: &str,
    exit_code: i32,
) -> Result<String> {
    let system_prompt = "You are a shell command expert. The user executed a command that failed. \
    Given the failed command and its exit code, output ONLY the corrected command. \
    Do not output markdown code blocks. Output ONLY the command code. \
    If you are unsure, output the original command.";

    let query = format!("Command: `{}`\nExit Code: {}", command, exit_code);

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": query}),
    ];

    let content = service.send_request(messages, Some(0.1)).await?;
    Ok(sanitize_code_block(&content))
}

pub async fn generate_commit_message<S: AiService + ?Sized>(
    service: &S,
    diff: &str,
) -> Result<String> {
    let system_prompt = "You are an expert developer. Generate a concise and descriptive git commit message based on the provided diff. \
    Follow the Conventional Commits specification if possible (e.g., 'feat: ...', 'fix: ...'). \
    Output ONLY the commit message. Do not include markdown code blocks. \
    The message should be a single line if it's short, or a summary line followed by a blank line and details if complex. \
    However, for this interaction, prefer a single line summary.";

    // Truncate diff if it's too long to avoid token limits (rudimentary handling)
    let truncated_diff = if diff.len() > 8000 {
        format!("{}\n...(truncated)", &diff[..8000])
    } else {
        diff.to_string()
    };

    let messages = vec![
        json!({"role": "system", "content": system_prompt}),
        json!({"role": "user", "content": truncated_diff}),
    ];

    let content = service.send_request(messages, Some(0.3)).await?;
    Ok(sanitize_code_block(&content))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let service = MockAiService::new("grep foo");
        let result = expand_smart_pipe(&service, "extract foo").await.unwrap();
        assert_eq!(result, "grep foo");

        let messages = service.last_messages.lock().unwrap();
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "extract foo");
    }

    #[tokio::test]
    async fn test_run_generative_command() {
        let service = MockAiService::new("git status");
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

    #[tokio::test]
    async fn test_fix_command() {
        let service = MockAiService::new("ls -la");
        let result = fix_command(&service, "lss -la", 127).await.unwrap();
        assert_eq!(result, "ls -la");

        let messages = service.last_messages.lock().unwrap();
        assert_eq!(messages[1]["role"], "user");
        assert!(messages[1]["content"].as_str().unwrap().contains("lss -la"));
    }

    #[tokio::test]
    async fn test_generate_commit_message() {
        let service = MockAiService::new("feat: add new feature");
        let diff = "diff --git a/file.txt b/file.txt...";
        let result = generate_commit_message(&service, diff).await.unwrap();
        assert_eq!(result, "feat: add new feature");
    }
}

use super::ShellProxy;
use crate::chatgpt::load_openai_config;
use dsh_openai::ChatGptClient;
use dsh_types::{Context, ExitStatus};
use serde_json::json;
use std::io::{self, Write};
use std::process::Command;

pub fn description() -> &'static str {
    "Generate git commit message using AI"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // 1. Check for staged changes
    let diff = match get_staged_diff(None) {
        Ok(d) if d.trim().is_empty() => {
            ctx.write_stderr("ai-commit: no staged changes. Run 'git add' first.")
                .ok();
            return ExitStatus::ExitedWith(1);
        }
        Ok(d) => d,
        Err(e) => {
            ctx.write_stderr(&format!("ai-commit: failed to get git diff: {}", e))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    // 2. Generate message using AI
    ctx.write_stdout("Generating commit message...").ok();
    let config = load_openai_config(proxy);
    if config.api_key().is_none() {
        ctx.write_stderr("ai-commit: AI_CHAT_API_KEY not found.")
            .ok();
        return ExitStatus::ExitedWith(1);
    }

    let client = match ChatGptClient::try_from_config(&config) {
        Ok(cl) => cl,
        Err(e) => {
            ctx.write_stderr(&format!(
                "ai-commit: failed to initialize AI client: {:?}",
                e
            ))
            .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    let model_override = if argv.len() > 1 {
        Some(argv[1].clone())
    } else {
        None
    };

    let mut message = match generate_commit_message(&client, &diff, model_override) {
        Ok(m) => m,
        Err(e) => {
            ctx.write_stderr(&format!("ai-commit: failed to generate message: {}", e))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    // 3. User Review Loop
    loop {
        ctx.write_stdout("\nGenerated Commit Message:\n").ok();
        ctx.write_stdout("----------------------------------------\n")
            .ok();
        ctx.write_stdout(&message).ok();
        ctx.write_stdout("\n----------------------------------------\n")
            .ok();
        ctx.write_stdout("Commit with this message? [y/n/e(dit)]: ")
            .ok();

        // Flush stdout to ensure prompt appears
        let _ = io::stdout().flush();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            return ExitStatus::ExitedWith(1);
        }

        match input.trim().to_lowercase().as_str() {
            "y" | "yes" => {
                // Execute commit
                match run_git_commit(&message, None) {
                    Ok(_) => {
                        ctx.write_stdout("Commit successful.").ok();
                        return ExitStatus::ExitedWith(0);
                    }
                    Err(e) => {
                        ctx.write_stderr(&format!("ai-commit: commit failed: {}", e))
                            .ok();
                        return ExitStatus::ExitedWith(1);
                    }
                }
            }
            "n" | "no" => {
                ctx.write_stdout("Commit aborted.").ok();
                return ExitStatus::ExitedWith(0);
            }
            "e" | "edit" => {
                // Use a temporary file for editing pattern if open_editor requires a file,
                // but ShellProxy::open_editor signature in lib.rs takes content and extension.
                // fn open_editor(&mut self, _content: &str, _extension: &str) -> Result<String>
                // Assuming this returns the edited content.
                match proxy.open_editor(&message, "COMMIT_EDITMSG") {
                    Ok(edited) => {
                        message = edited.trim().to_string();
                        // Loop continues to show new message
                    }
                    Err(e) => {
                        ctx.write_stderr(&format!("ai-commit: failed to open editor: {}", e))
                            .ok();
                        // Fallback to basic input? Or just continue loop?
                        // Continuing loop allows user to try again or abort.
                    }
                }
            }
            _ => {
                ctx.write_stderr("Invalid option. Please enter 'y', 'n', or 'e'.")
                    .ok();
            }
        }
    }
}

fn get_staged_diff(cwd: Option<&std::path::Path>) -> Result<String, String> {
    let mut command = Command::new("git");
    command.args(["diff", "--cached"]);

    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    let output = command.output().map_err(|e| format!("{}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn generate_commit_message(
    client: &ChatGptClient,
    diff: &str,
    model_override: Option<String>,
) -> Result<String, String> {
    let system_prompt = r#"You are an AI assistant that writes git commit messages.
Generate a commit message in the Conventional Commits format based on the provided git diff.

Rules:
1. Format: <type>(<scope>): <subject>
   <BLANK LINE>
   <body>
   <BLANK LINE>
   <footer>
2. Types: feat, fix, docs, style, refactor, perf, test, build, ci, chore, revert.
3. Keep the subject line under 50 characters if possible, never over 72.
4. The body should explain *what* and *why*, not *how*.
5. Be concise but descriptive.
6. Output ONLY the commit message. No other text or markdown code blocks.
"#;

    let messages = vec![
        json!({
            "role": "system",
            "content": system_prompt
        }),
        json!({
            "role": "user",
            "content": diff
        }),
    ];

    let response = client
        .send_chat_request(
            &messages,
            Some(0.3), // Lower temperature for more deterministic output
            model_override,
            None,
            None, // No cancellation callback for now, or could pass one if available
        )
        .map_err(|e| format!("{:?}", e))?;

    let content = response["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("Invalid response from AI")?
        .to_string();

    Ok(content.trim().to_string())
}

fn run_git_commit(message: &str, cwd: Option<&std::path::Path>) -> Result<(), String> {
    let mut command = Command::new("git");
    command.args(["commit", "-m", message]);

    if let Some(dir) = cwd {
        command.current_dir(dir);
    }

    let output = command.output().map_err(|e| format!("{}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::TempDir;

    fn setup_git_repo() -> TempDir {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let path = temp_dir.path();

        Command::new("git")
            .arg("init")
            .current_dir(path)
            .output()
            .expect("failed to init git repo");

        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output()
            .expect("failed to set git user email");

        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(path)
            .output()
            .expect("failed to set git user name");

        temp_dir
    }

    #[test]
    fn test_get_staged_diff_empty() {
        let temp_dir = setup_git_repo();
        let diff = get_staged_diff(Some(temp_dir.path())).unwrap();
        assert!(diff.trim().is_empty());
    }

    #[test]
    fn test_get_staged_diff_with_changes() {
        let temp_dir = setup_git_repo();
        let path = temp_dir.path();

        // Create a file
        let file_path = path.join("test.txt");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "hello world").unwrap();

        // Stage it
        Command::new("git")
            .args(["add", "test.txt"])
            .current_dir(path)
            .output()
            .expect("failed to stage file");

        let diff = get_staged_diff(Some(path)).unwrap();
        assert!(diff.contains("hello world"));
        assert!(diff.contains("diff --git"));
    }

    #[test]
    fn test_run_git_commit() {
        let temp_dir = setup_git_repo();
        let path = temp_dir.path();

        // Create and stage file
        let file_path = path.join("test.txt");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "hello").unwrap();

        Command::new("git")
            .args(["add", "test.txt"])
            .current_dir(path)
            .output()
            .expect("failed to stage file");

        // Run commit
        run_git_commit("feat: test commit", Some(path)).unwrap();

        // Verify log
        let output = Command::new("git")
            .args(["log", "-1", "--pretty=%B"])
            .current_dir(path)
            .output()
            .unwrap();

        let msg = String::from_utf8_lossy(&output.stdout);
        assert_eq!(msg.trim(), "feat: test commit");
    }
}

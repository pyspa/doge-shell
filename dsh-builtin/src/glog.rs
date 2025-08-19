use super::ShellProxy;
use dsh_types::{Context, ExitStatus};
use std::io::Write;
use std::process::{Command, Stdio};

/// Git Log Interactive - Enhanced git log with interactive commit selection and checkout
///
/// Features:
/// - Beautiful git log display with graph, author, date, and message
/// - Interactive commit selection using skim (sk) or fzf
/// - Direct checkout to selected commit (detached HEAD)
/// - Configurable log format and number of commits
/// - Safe checkout with confirmation for detached HEAD state
/// - Support for different log views (oneline, detailed, graph)
pub fn command(ctx: &Context, argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Check if we're in a git repository
    if !is_git_repository() {
        ctx.write_stderr("glog: not a git repository").ok();
        return ExitStatus::ExitedWith(1);
    }

    // Parse command line arguments
    let options = parse_arguments(&argv);

    // Get git log entries
    let log_entries = match get_git_log(&options) {
        Ok(entries) => entries,
        Err(err) => {
            ctx.write_stderr(&format!("glog: failed to get git log: {err}"))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    if log_entries.is_empty() {
        ctx.write_stderr("glog: no commits found").ok();
        return ExitStatus::ExitedWith(1);
    }

    // Interactive commit selection
    if let Some(selected_line) = interactive_commit_selection(&log_entries) {
        // Extract commit hash from selected line
        if let Some(commit_hash) = extract_commit_hash(&selected_line) {
            checkout_commit(ctx, &commit_hash)
        } else {
            ctx.write_stderr("glog: failed to extract commit hash from selection")
                .ok();
            ExitStatus::ExitedWith(1)
        }
    } else {
        // User cancelled selection
        ExitStatus::ExitedWith(0)
    }
}

/// Configuration options for git log display
#[derive(Debug, Clone)]
struct LogOptions {
    /// Number of commits to show (default: 50)
    limit: usize,
    /// Show graph (default: true)
    graph: bool,
    /// Show all branches (default: false)
    all_branches: bool,
    /// Oneline format (default: false)
    oneline: bool,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            limit: 50,
            graph: true,
            all_branches: false,
            oneline: false,
        }
    }
}

/// Parse command line arguments to configure log options
fn parse_arguments(argv: &[String]) -> LogOptions {
    let mut options = LogOptions::default();
    let mut i = 1;

    while i < argv.len() {
        match argv[i].as_str() {
            "-n" | "--number" => {
                if i + 1 < argv.len()
                    && let Ok(limit) = argv[i + 1].parse::<usize>()
                {
                    options.limit = limit;
                    i += 1;
                }
            }
            "--no-graph" => {
                options.graph = false;
            }
            "-a" | "--all" => {
                options.all_branches = true;
            }
            "--oneline" => {
                options.oneline = true;
            }
            _ => {
                // Ignore unknown arguments
            }
        }
        i += 1;
    }

    options
}

/// Check if current directory is within a git repository
fn is_git_repository() -> bool {
    Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Get formatted git log entries
fn get_git_log(options: &LogOptions) -> Result<Vec<String>, String> {
    let mut args = vec!["log"];

    // Add limit
    let limit_str = format!("{}", options.limit);
    args.push("-n");
    args.push(&limit_str);

    // Configure format based on options
    if options.oneline {
        args.push("--oneline");
        if options.graph {
            args.push("--graph");
        }
    } else {
        // Custom format for better readability
        args.push(
            "--pretty=format:%C(yellow)%h%C(reset) %C(blue)%ad%C(reset) %C(green)%an%C(reset) %s",
        );
        args.push("--date=short");
        if options.graph {
            args.push("--graph");
        }
    }

    // Add all branches if requested
    if options.all_branches {
        args.push("--all");
    }

    let output = Command::new("git")
        .args(&args)
        .output()
        .map_err(|e| format!("failed to execute git: {e}"))?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(error.trim().to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let entries: Vec<String> = stdout
        .lines()
        .map(|line| line.to_string())
        .filter(|line| !line.trim().is_empty())
        .collect();

    Ok(entries)
}

/// Interactive commit selection using skim or fzf
fn interactive_commit_selection(log_entries: &[String]) -> Option<String> {
    let log_content = log_entries.join("\n");

    // Try skim (sk) first with git-specific options
    if let Ok(mut child) = Command::new("sk")
        .args([
            "--ansi",    // Support ANSI color codes
            "--reverse", // Reverse layout (newer commits at top)
            "--preview", // Enable preview
            r#"echo {} | sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' | sed 's/^[*|\\/ -]*//' | awk '{print $1}' | xargs -I {} sh -c 'git show --color=always --format=fuller "$1" 2>/dev/null || echo "Error: Invalid commit hash"' _ {}"#, // Preview command
            "--preview-window",
            "right:60%",
            "--header",
            "Select commit to checkout (ESC/Ctrl+C/Ctrl+G to cancel)",
            "--bind",
            "ctrl-c:abort",
            "--bind",
            "ctrl-g:abort",
            "--bind",
            "esc:abort",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(log_content.as_bytes());
            drop(stdin);
        }

        if let Ok(output) = child.wait_with_output() {
            if output.status.success() {
                let selected = String::from_utf8_lossy(&output.stdout);
                let trimmed = selected.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            } else {
                // Check if process was interrupted (Ctrl+C, Ctrl+G, ESC)
                if let Some(exit_code) = output.status.code() {
                    if exit_code == 130 || exit_code == 1 {
                        // User cancelled with Ctrl+C (130) or ESC (1)
                        return None;
                    }
                } else {
                    // Process was terminated by signal (e.g., SIGINT)
                    return None;
                }
            }
        }
    }

    // Fallback to fzf
    if let Ok(mut child) = Command::new("fzf")
        .args([
            "--ansi",
            "--reverse",
            "--preview",
            r#"echo {} | sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' | sed 's/^[*|\\/ -]*//' | awk '{print $1}' | xargs -I {} sh -c 'git show --color=always --format=fuller "$1" 2>/dev/null || echo "Error: Invalid commit hash"' _ {}"#,
            "--preview-window", "right:60%",
            "--header", "Select commit to checkout (ESC/Ctrl+C/Ctrl+G to cancel)",
            "--bind", "ctrl-c:abort",
            "--bind", "ctrl-g:abort",
            "--bind", "esc:abort",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(log_content.as_bytes());
            drop(stdin);
        }

        if let Ok(output) = child.wait_with_output() {
            if output.status.success() {
                let selected = String::from_utf8_lossy(&output.stdout);
                let trimmed = selected.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            } else {
                // Check if process was interrupted (Ctrl+C, Ctrl+G, ESC)
                if let Some(exit_code) = output.status.code() {
                    if exit_code == 130 || exit_code == 1 {
                        // User cancelled with Ctrl+C (130) or ESC (1)
                        return None;
                    }
                } else {
                    // Process was terminated by signal (e.g., SIGINT)
                    return None;
                }
            }
        }
    }

    // Final fallback to simple numbered selection
    numbered_commit_selection(log_entries)
}

/// Extract commit hash from a git log line
fn extract_commit_hash(log_line: &str) -> Option<String> {
    // Remove ANSI color codes and graph characters
    let cleaned = strip_ansi_codes(log_line);

    // Find the first word that looks like a commit hash (7+ hex characters)
    for word in cleaned.split_whitespace() {
        let word = word.trim_start_matches(['*', '|', '\\', '/', '-', ' ']);
        if word.len() >= 7 && word.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(word.to_string());
        }
    }

    None
}

/// Strip ANSI color codes from a string
fn strip_ansi_codes(input: &str) -> String {
    let mut result = String::new();
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            // Skip ANSI escape sequence
            chars.next(); // consume '['
            for ch in chars.by_ref() {
                if ch.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Fallback numbered selection when interactive tools are not available
fn numbered_commit_selection(log_entries: &[String]) -> Option<String> {
    use std::io::{self, Write};

    println!("Interactive tools not available. Select commit by number:");
    println!();

    for (i, entry) in log_entries.iter().enumerate() {
        let cleaned = strip_ansi_codes(entry);
        println!("  {}: {}", i + 1, cleaned);
    }

    println!();
    print!("Enter commit number (or 'q' to quit, Ctrl+C to cancel): ");
    io::stdout().flush().ok()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok()?;

    let input = input.trim();

    if input == "q" || input == "quit" {
        return None;
    }

    if let Ok(num) = input.parse::<usize>()
        && num > 0
        && num <= log_entries.len()
    {
        return Some(log_entries[num - 1].clone());
    }

    println!("Invalid selection");
    None
}

/// Checkout to a specific commit (detached HEAD)
fn checkout_commit(ctx: &Context, commit_hash: &str) -> ExitStatus {
    use std::io;
    // Warn user about detached HEAD state
    ctx.write_stdout(&format!(
        "⚠️  Checking out commit {commit_hash} will put you in 'detached HEAD' state."
    ))
    .ok();
    ctx.write_stdout("You can look around, make experimental changes and commit them,")
        .ok();
    ctx.write_stdout("and you can discard any commits you make in this state without")
        .ok();
    ctx.write_stdout("impacting any branches by performing another checkout.")
        .ok();
    ctx.write_stdout("").ok();

    // Ask for confirmation
    ctx.write_stdout("Do you want to proceed? (y/N): ").ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        ctx.write_stderr("glog: failed to read input").ok();
        return ExitStatus::ExitedWith(1);
    }

    let input = input.trim().to_lowercase();
    if input != "y" && input != "yes" {
        ctx.write_stdout("Checkout cancelled.").ok();
        return ExitStatus::ExitedWith(0);
    }

    // Perform the checkout
    let output = Command::new("git").args(["checkout", commit_hash]).output();

    match output {
        Ok(result) => {
            if result.status.success() {
                let stdout = String::from_utf8_lossy(&result.stdout);
                if !stdout.trim().is_empty() {
                    ctx.write_stdout(&stdout).ok();
                }
                ctx.write_stdout(&format!("✓ Successfully checked out commit {commit_hash}"))
                    .ok();
                ctx.write_stdout("💡 To return to a branch, use: git checkout <branch_name>")
                    .ok();
                ExitStatus::ExitedWith(0)
            } else {
                let error = String::from_utf8_lossy(&result.stderr);
                ctx.write_stderr(&format!("glog: {}", error.trim())).ok();
                ExitStatus::ExitedWith(1)
            }
        }
        Err(err) => {
            ctx.write_stderr(&format!("glog: failed to execute git: {err}"))
                .ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_arguments() {
        // Default options
        let argv = vec!["glog".to_string()];
        let options = parse_arguments(&argv);
        assert_eq!(options.limit, 50);
        assert!(options.graph);
        assert!(!options.all_branches);
        assert!(!options.oneline);

        // Custom limit
        let argv = vec!["glog".to_string(), "-n".to_string(), "20".to_string()];
        let options = parse_arguments(&argv);
        assert_eq!(options.limit, 20);

        // Multiple options
        let argv = vec![
            "glog".to_string(),
            "--no-graph".to_string(),
            "-a".to_string(),
            "--oneline".to_string(),
        ];
        let options = parse_arguments(&argv);
        assert!(!options.graph);
        assert!(options.all_branches);
        assert!(options.oneline);
    }

    #[test]
    fn test_extract_commit_hash() {
        // Test with colored output
        let line = "\x1b[33ma1b2c3d\x1b[0m 2023-12-01 Author Name Commit message";
        assert_eq!(extract_commit_hash(line), Some("a1b2c3d".to_string()));

        // Test with graph characters
        let line = "* | \\ a1b2c3d 2023-12-01 Author Name Commit message";
        assert_eq!(extract_commit_hash(line), Some("a1b2c3d".to_string()));

        // Test oneline format
        let line = "a1b2c3d Commit message";
        assert_eq!(extract_commit_hash(line), Some("a1b2c3d".to_string()));

        // Test invalid line
        let line = "No commit hash here";
        assert_eq!(extract_commit_hash(line), None);
    }

    #[test]
    fn test_strip_ansi_codes() {
        let input = "\x1b[33mHello\x1b[0m \x1b[32mWorld\x1b[0m";
        let expected = "Hello World";
        assert_eq!(strip_ansi_codes(input), expected);

        let input = "No ANSI codes here";
        assert_eq!(strip_ansi_codes(input), input);
    }

    #[test]
    fn test_is_git_repository() {
        // This test will depend on the test environment
        let _result = is_git_repository();
        // Environment-dependent; ensure function is callable without panic
        // No assertion on value to keep test stable across environments
    }
}

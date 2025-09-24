use super::ShellProxy;
use dirs;
use dsh_types::{Context, ExitStatus};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;

// Pre-compiled regex patterns for efficient path processing
// These patterns are compiled once and reused for all cd operations
static ABSOLUTE_PATH_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^/").unwrap());
static HOME_PATH_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^~").unwrap());

/// Built-in cd command description
pub fn description() -> &'static str {
    "Change the current working directory"
}

/// Built-in cd (change directory) command implementation
/// Supports various path formats:
/// - Absolute paths (starting with /)
/// - Home directory paths (starting with ~)
/// - Relative paths
/// - No argument (defaults to home directory)
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Get current directory for relative path resolution
    let current_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(err) => {
            ctx.write_stderr(&format!("cd: failed to get current directory: {err}"))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    // Determine target directory based on argument
    let dir = match argv.get(1).map(|s| s.as_str()) {
        // Handle absolute paths (starting with /)
        Some(dir) if ABSOLUTE_PATH_REGEX.is_match(dir) => dir.to_string(),

        // Handle home directory paths (starting with ~)
        Some(dir) if HOME_PATH_REGEX.is_match(dir) => shellexpand::tilde(dir).to_string(),

        // Handle relative paths
        Some(dir) => {
            let res = Path::new(&current_dir).join(dir).canonicalize();

            match res {
                Ok(res) => res.to_string_lossy().into_owned(),
                Err(err) => {
                    ctx.write_stderr(&format!("cd: {err}: {dir}")).ok();
                    return ExitStatus::ExitedWith(1);
                }
            }
        }

        // No argument provided - default to home directory
        None => {
            if let Some(home_dir) = dirs::home_dir() {
                home_dir.to_string_lossy().into_owned()
            } else {
                // Fallback to root directory if home directory cannot be determined
                String::from("/")
            }
        }
    };

    // Attempt to change directory through shell proxy
    match proxy.changepwd(&dir) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(err) => {
            ctx.write_stderr(&format!("cd: {err}: {dir}")).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

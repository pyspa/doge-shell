use super::ShellProxy;
use dirs;
use dsh_types::{Context, ExitStatus};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;

// Pre-compiled regex patterns for path processing
static ABSOLUTE_PATH_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^/").unwrap());
static HOME_PATH_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^~").unwrap());

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let current_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(err) => {
            ctx.write_stderr(&format!("cd: failed to get current directory: {}", err))
                .ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    let dir = match argv.get(1).map(|s| s.as_str()) {
        Some(dir) if ABSOLUTE_PATH_REGEX.is_match(dir) => dir.to_string(),
        Some(dir) if HOME_PATH_REGEX.is_match(dir) => shellexpand::tilde(dir).to_string(),
        Some(dir) => {
            let res = Path::new(&current_dir).join(dir).canonicalize();

            match res {
                Ok(res) => res.to_string_lossy().into_owned(),
                Err(err) => {
                    ctx.write_stderr(&format!("cd: {}: {}", err, dir)).ok();
                    return ExitStatus::ExitedWith(1);
                }
            }
        }
        None => {
            if let Some(home_dir) = dirs::home_dir() {
                home_dir.to_string_lossy().into_owned()
            } else {
                String::from("/")
            }
        }
    };

    match proxy.changepwd(&dir) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(err) => {
            ctx.write_stderr(&format!("cd: {}: {}", err, dir)).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

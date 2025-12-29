use crate::ShellProxy;
use anyhow::{Context as _, Result};
use dsh_types::{Context, ExitStatus};
use std::io::Write;
use std::process::Command;

/// Description for the comp-gen command
pub fn description() -> &'static str {
    "Generate command completion using AI"
}

/// comp-gen command implementation
///
/// Usage: comp-gen <command_name>
///
/// This command fetches the help text for the specified command (using `man` or `--help`),
/// sends it to the AI service to generate a JSON completion definition,
/// and saves the result to `~/.config/dsh/completions/<command_name>.json`.
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.len() < 2 {
        ctx.write_stderr("Usage: comp-gen <command>").ok();
        return ExitStatus::ExitedWith(1);
    }

    let command_name = &argv[1];

    match generate_and_save(ctx, proxy, command_name) {
        Ok(path) => {
            ctx.write_stdout(&format!(
                "Completion generated and saved to {}",
                path.display()
            ))
            .ok();
            ExitStatus::ExitedWith(0)
        }
        Err(e) => {
            ctx.write_stderr(&format!("Error: {:#}", e)).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

fn generate_and_save(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    command_name: &str,
) -> Result<std::path::PathBuf> {
    ctx.write_stdout(&format!("Fetching help text for '{}'...", command_name))
        .ok();

    // Try man first, then --help
    let help_text = get_help_text(command_name)?;
    if help_text.trim().is_empty() {
        return Err(anyhow::anyhow!(
            "Could not retrieve help text for '{}'. \nPlease ensure the command is installed and has a manual page or supports --help.",
            command_name
        ));
    }

    ctx.write_stdout("Generating completion JSON via AI (this may take a moment)...")
        .ok();
    let json = proxy.generate_command_completion(command_name, &help_text)?;

    // Validate JSON parsing to verify it's valid before saving
    let _parsed: serde_json::Value =
        serde_json::from_str(&json).context("AI returned invalid JSON")?;

    // Save to file
    // We use "dsh" suffix for config, but the app name constant is "dsh".
    // The environment uses "doge-shell" or "dsh"?
    // dsh/src/shell/mod.rs: pub const APP_NAME: &str = "dsh";
    // So "dsh" is likely correct for XDG prefix.
    let xdg_dirs = xdg::BaseDirectories::with_prefix("dsh")?;

    // Ensure completions directory exists
    // place_config_file creates the directory structure if missing
    let filename = format!("completions/{}.json", command_name);
    let path = xdg_dirs.place_config_file(filename)?;

    let mut file = std::fs::File::create(&path)?;
    file.write_all(json.as_bytes())?;

    Ok(path)
}

fn get_help_text(command_name: &str) -> Result<String> {
    // 1. Try `man -P cat <command>` to avoid pagination
    let man_output = Command::new("man")
        .arg("-P")
        .arg("cat")
        .arg(command_name)
        .output();

    if let Ok(output) = man_output
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if !stdout.trim().is_empty() {
            return Ok(stdout);
        }
    }

    // 2. Fallback to `<command> --help`
    // Note: We execute the command here. This implies trust in the command.
    let help_output = Command::new(command_name).arg("--help").output();

    if let Ok(output) = help_output
        && output.status.success()
    {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }

    Ok(String::new())
}

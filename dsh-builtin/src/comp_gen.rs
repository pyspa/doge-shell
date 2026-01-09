use crate::ShellProxy;
use anyhow::{Context as _, Result, bail};
use dsh_types::{Context, ExitStatus};
use serde_json::{Map, Value};
use std::io::Write;
use std::process::Command;

/// Description for the comp-gen command
pub fn description() -> &'static str {
    "Generate command completion using AI"
}

/// comp-gen command implementation
///
/// Usage: comp-gen [--stdout] [--check] <command_name>
///
/// This command fetches the help text for the specified command (using `man` or `--help`),
/// sends it to the AI service to generate a JSON completion definition,
/// and saves the result to `~/.config/dsh/completions/<command_name>.json`.
pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.iter().any(|arg| arg == "--help" || arg == "-h") {
        ctx.write_stdout(usage()).ok();
        return ExitStatus::ExitedWith(0);
    }

    let args = &argv[1..];
    let (options, command_name) = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            ctx.write_stderr(&format!("Error: {:#}\n", e)).ok();
            ctx.write_stderr(usage()).ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    let log_to_stderr = options.stdout;
    match generate_completion(ctx, proxy, &command_name, log_to_stderr) {
        Ok(json) => {
            if options.check_only {
                ctx.write_stdout("OK\n").ok();
                return ExitStatus::ExitedWith(0);
            }
            if options.stdout {
                ctx.write_stdout(&format!("{json}\n")).ok();
                return ExitStatus::ExitedWith(0);
            }
            match save_completion_json(&command_name, &json) {
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
        Err(e) => {
            ctx.write_stderr(&format!("Error: {:#}", e)).ok();
            ExitStatus::ExitedWith(1)
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CompGenOptions {
    stdout: bool,
    check_only: bool,
}

fn usage() -> &'static str {
    r#"Usage: comp-gen [--stdout] [--check] <command>

Options:
  --stdout   Print generated JSON to stdout instead of saving
  --check    Validate generated JSON and exit (no save)
  -h, --help Show this help message

Notes:
  --stdout and --check are mutually exclusive.
"#
}

fn parse_args(args: &[String]) -> Result<(CompGenOptions, String)> {
    let mut options = CompGenOptions {
        stdout: false,
        check_only: false,
    };
    let mut command_name: Option<String> = None;

    for arg in args {
        match arg.as_str() {
            "--stdout" => options.stdout = true,
            "--check" => options.check_only = true,
            "-h" | "--help" => {}
            _ if arg.starts_with('-') => bail!("Unknown option: {}", arg),
            _ => {
                if command_name.is_some() {
                    bail!("Only one command may be specified");
                }
                command_name = Some(arg.clone());
            }
        }
    }

    let command_name = command_name.context("Missing required <command> argument")?;
    if options.stdout && options.check_only {
        bail!("--stdout and --check cannot be used together");
    }
    Ok((options, command_name))
}

fn generate_completion(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    command_name: &str,
    log_to_stderr: bool,
) -> Result<String> {
    log(
        ctx,
        log_to_stderr,
        &format!("Fetching help text for '{}'...", command_name),
    );

    // Try man first, then --help
    let help_text = get_help_text(command_name)?;
    if help_text.trim().is_empty() {
        return Err(anyhow::anyhow!(
            "Could not retrieve help text for '{}'. \nPlease ensure the command is installed and has a manual page or supports --help.",
            command_name
        ));
    }

    log(
        ctx,
        log_to_stderr,
        "Generating completion JSON via AI (this may take a moment)...",
    );
    let json = proxy.generate_command_completion(command_name, &help_text)?;

    // Validate JSON before saving
    validate_completion_json(&json, command_name)?;

    Ok(json)
}

fn save_completion_json(command_name: &str, json: &str) -> Result<std::path::PathBuf> {
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

fn log(ctx: &Context, to_stderr: bool, message: &str) {
    if to_stderr {
        let _ = ctx.write_stderr(message);
    } else {
        let _ = ctx.write_stdout(message);
    }
}

fn validate_completion_json(json: &str, expected_command: &str) -> Result<()> {
    let value: Value = serde_json::from_str(json).context("AI returned invalid JSON")?;
    let obj = value
        .as_object()
        .context("Completion JSON must be an object")?;

    let command_value = obj
        .get("command")
        .context("Missing required field: command")?;
    let command = require_non_empty_string(command_value, "command")?;
    if command != expected_command {
        bail!(
            "Command mismatch: expected '{}', got '{}'",
            expected_command,
            command
        );
    }

    if let Some(options) = obj.get("global_options") {
        validate_options_array(options, "global_options")?;
    }
    if let Some(arguments) = obj.get("arguments") {
        validate_arguments_array(arguments, "arguments")?;
    }
    if let Some(subcommands) = obj.get("subcommands") {
        validate_subcommands_array(subcommands, "subcommands")?;
    }

    Ok(())
}

fn require_non_empty_string<'a>(value: &'a Value, path: &str) -> Result<&'a str> {
    let s = value
        .as_str()
        .with_context(|| format!("{path} must be a string"))?;
    if s.trim().is_empty() {
        bail!("{path} must be a non-empty string");
    }
    Ok(s)
}

fn optional_string<'a>(
    obj: &'a Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<&'a str>> {
    let Some(value) = obj.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let s = value
        .as_str()
        .with_context(|| format!("{path}.{key} must be a string"))?;
    if s.trim().is_empty() {
        bail!("{path}.{key} must be a non-empty string");
    }
    Ok(Some(s))
}

fn validate_options_array(value: &Value, path: &str) -> Result<()> {
    let options = value
        .as_array()
        .with_context(|| format!("{path} must be an array"))?;
    for (idx, option) in options.iter().enumerate() {
        let option_path = format!("{path}[{idx}]");
        let obj = option
            .as_object()
            .with_context(|| format!("{option_path} must be an object"))?;
        let short = optional_string(obj, "short", &option_path)?;
        let long = optional_string(obj, "long", &option_path)?;
        if short.is_none() && long.is_none() {
            bail!("{option_path} must have at least one of 'short' or 'long'");
        }
        if let Some(takes_value) = obj.get("takes_value")
            && !takes_value.is_boolean()
        {
            bail!("{option_path}.takes_value must be boolean");
        }
        if let Some(value_type) = obj.get("value_type") {
            validate_argument_type(value_type, &format!("{option_path}.value_type"))?;
        }
    }
    Ok(())
}

fn validate_arguments_array(value: &Value, path: &str) -> Result<()> {
    let args = value
        .as_array()
        .with_context(|| format!("{path} must be an array"))?;
    for (idx, arg) in args.iter().enumerate() {
        let arg_path = format!("{path}[{idx}]");
        let obj = arg
            .as_object()
            .with_context(|| format!("{arg_path} must be an object"))?;
        let name_value = obj
            .get("name")
            .with_context(|| format!("{arg_path}.name is required"))?;
        require_non_empty_string(name_value, &format!("{arg_path}.name"))?;
        if let Some(arg_type) = obj.get("type") {
            validate_argument_type(arg_type, &format!("{arg_path}.type"))?;
        }
        if let Some(required) = obj.get("required")
            && !required.is_boolean()
        {
            bail!("{arg_path}.required must be boolean");
        }
        if let Some(multiple) = obj.get("multiple")
            && !multiple.is_boolean()
        {
            bail!("{arg_path}.multiple must be boolean");
        }
    }
    Ok(())
}

fn validate_subcommands_array(value: &Value, path: &str) -> Result<()> {
    let subs = value
        .as_array()
        .with_context(|| format!("{path} must be an array"))?;
    for (idx, sub) in subs.iter().enumerate() {
        let sub_path = format!("{path}[{idx}]");
        let obj = sub
            .as_object()
            .with_context(|| format!("{sub_path} must be an object"))?;
        let name_value = obj
            .get("name")
            .with_context(|| format!("{sub_path}.name is required"))?;
        require_non_empty_string(name_value, &format!("{sub_path}.name"))?;
        if let Some(options) = obj.get("options") {
            validate_options_array(options, &format!("{sub_path}.options"))?;
        }
        if let Some(arguments) = obj.get("arguments") {
            validate_arguments_array(arguments, &format!("{sub_path}.arguments"))?;
        }
        if let Some(children) = obj.get("subcommands") {
            validate_subcommands_array(children, &format!("{sub_path}.subcommands"))?;
        }
    }
    Ok(())
}

fn validate_argument_type(value: &Value, path: &str) -> Result<()> {
    let obj = value
        .as_object()
        .with_context(|| format!("{path} must be an object"))?;
    let type_value = obj
        .get("type")
        .with_context(|| format!("{path}.type is required"))?;
    let type_name = require_non_empty_string(type_value, &format!("{path}.type"))?;
    if type_name == "Script" {
        bail!("{path}.type 'Script' is not allowed");
    }

    if type_name == "Choice" {
        let data = obj
            .get("data")
            .with_context(|| format!("{path}.data is required for Choice"))?;
        let items = data
            .as_array()
            .with_context(|| format!("{path}.data must be an array of strings"))?;
        for (idx, item) in items.iter().enumerate() {
            if item.as_str().is_none() {
                bail!("{path}.data[{idx}] must be a string");
            }
        }
    }

    if type_name == "File"
        && let Some(data) = obj.get("data")
    {
        let data_obj = data
            .as_object()
            .with_context(|| format!("{path}.data must be an object"))?;
        if let Some(exts) = data_obj.get("extensions") {
            let list = exts
                .as_array()
                .with_context(|| format!("{path}.data.extensions must be an array"))?;
            for (idx, ext) in list.iter().enumerate() {
                if ext.as_str().is_none() {
                    bail!("{path}.data.extensions[{idx}] must be a string");
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_args, validate_completion_json};

    #[test]
    fn validate_completion_allows_minimal() {
        let json = r#"{ "command": "foo" }"#;
        assert!(validate_completion_json(json, "foo").is_ok());
    }

    #[test]
    fn validate_completion_rejects_missing_command() {
        let json = r#"{ "description": "x" }"#;
        assert!(validate_completion_json(json, "foo").is_err());
    }

    #[test]
    fn validate_completion_rejects_command_mismatch() {
        let json = r#"{ "command": "bar" }"#;
        assert!(validate_completion_json(json, "foo").is_err());
    }

    #[test]
    fn validate_completion_rejects_option_without_flag() {
        let json = r#"
        {
          "command": "foo",
          "global_options": [
            { "description": "no flag" }
          ]
        }
        "#;
        assert!(validate_completion_json(json, "foo").is_err());
    }

    #[test]
    fn validate_completion_rejects_script_type() {
        let json = r#"
        {
          "command": "foo",
          "arguments": [
            { "name": "x", "type": { "type": "Script", "data": "echo hi" } }
          ]
        }
        "#;
        assert!(validate_completion_json(json, "foo").is_err());
    }

    #[test]
    fn validate_completion_requires_type_object() {
        let json = r#"
        {
          "command": "foo",
          "arguments": [
            { "name": "x", "type": "String" }
          ]
        }
        "#;
        assert!(validate_completion_json(json, "foo").is_err());
    }

    #[test]
    fn parse_args_accepts_stdout() {
        let args = vec!["--stdout".to_string(), "git".to_string()];
        let (options, command) = parse_args(&args).unwrap();
        assert!(options.stdout);
        assert!(!options.check_only);
        assert_eq!(command, "git");
    }

    #[test]
    fn parse_args_accepts_check() {
        let args = vec!["--check".to_string(), "cargo".to_string()];
        let (options, command) = parse_args(&args).unwrap();
        assert!(!options.stdout);
        assert!(options.check_only);
        assert_eq!(command, "cargo");
    }

    #[test]
    fn parse_args_rejects_unknown_option() {
        let args = vec!["--nope".to_string(), "git".to_string()];
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn parse_args_rejects_conflicting_modes() {
        let args = vec![
            "--stdout".to_string(),
            "--check".to_string(),
            "git".to_string(),
        ];
        assert!(parse_args(&args).is_err());
    }
}

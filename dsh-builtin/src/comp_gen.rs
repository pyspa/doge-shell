use crate::ShellProxy;
use anyhow::{Context as _, Result, bail};
use dsh_types::completion::{DYNAMIC_COMPLETION_PROVIDERS, is_known_dynamic_completion_provider};
use dsh_types::{Context, ExitStatus};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Description for the comp-gen command
pub fn description() -> &'static str {
    "Generate command completion using AI"
}

/// comp-gen command implementation
///
/// Usage: comp-gen [--stdout] [--check] <command_name>
///        comp-gen --list-dynamic-providers
///        comp-gen --audit [completion-dir]
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
    let action = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            ctx.write_stderr(&format!("Error: {:#}\n", e)).ok();
            ctx.write_stderr(usage()).ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    let CompGenAction::Generate {
        options,
        command_name,
    } = action
    else {
        return match action {
            CompGenAction::ListDynamicProviders => {
                ctx.write_stdout(&dynamic_provider_list()).ok();
                ExitStatus::ExitedWith(0)
            }
            CompGenAction::Audit { dir } => match audit_completion_dir(&dir) {
                Ok(output) => {
                    ctx.write_stdout(&output).ok();
                    ExitStatus::ExitedWith(0)
                }
                Err(e) => {
                    ctx.write_stderr(&format!("Error: {:#}", e)).ok();
                    ExitStatus::ExitedWith(1)
                }
            },
            CompGenAction::Generate { .. } => unreachable!(),
        };
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompGenOptions {
    stdout: bool,
    check_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompGenAction {
    Generate {
        options: CompGenOptions,
        command_name: String,
    },
    ListDynamicProviders,
    Audit {
        dir: PathBuf,
    },
}

fn usage() -> &'static str {
    r#"Usage: comp-gen [--stdout] [--check] <command>
       comp-gen --list-dynamic-providers
       comp-gen --audit [completion-dir]

Options:
  --stdout                  Print generated JSON to stdout instead of saving
  --check                   Validate generated JSON and exit (no save)
  --list-dynamic-providers  Print known Dynamic provider ids
  --audit [completion-dir]  Summarize JSON command/type/provider coverage
  -h, --help                Show this help message

Notes:
  --stdout and --check are mutually exclusive. Script argument types are rejected
  for generated JSON; handwritten runtime definitions may still use Script.
"#
}

fn parse_args(args: &[String]) -> Result<CompGenAction> {
    let mut options = CompGenOptions {
        stdout: false,
        check_only: false,
    };
    let mut command_name: Option<String> = None;
    let mut list_dynamic_providers = false;
    let mut audit_dir: Option<PathBuf> = None;
    let mut audit_dir_explicit = false;

    for arg in args {
        match arg.as_str() {
            "--stdout" => options.stdout = true,
            "--check" => options.check_only = true,
            "--list-dynamic-providers" => list_dynamic_providers = true,
            "--audit" => {
                if audit_dir.is_some() {
                    bail!("--audit may only be specified once");
                }
                audit_dir = Some(PathBuf::from("completions"));
                audit_dir_explicit = false;
            }
            "-h" | "--help" => {}
            _ if arg.starts_with('-') => bail!("Unknown option: {}", arg),
            _ => {
                if audit_dir.is_some() && !audit_dir_explicit {
                    audit_dir = Some(PathBuf::from(arg));
                    audit_dir_explicit = true;
                } else {
                    if command_name.is_some() {
                        bail!("Only one command may be specified");
                    }
                    command_name = Some(arg.clone());
                }
            }
        }
    }

    let mode_count = usize::from(list_dynamic_providers) + usize::from(audit_dir.is_some());
    if mode_count > 1 {
        bail!("Only one listing/audit mode may be specified");
    }
    if mode_count > 0 && (options.stdout || options.check_only || command_name.is_some()) {
        bail!("Listing/audit modes cannot be combined with generation options or <command>");
    }
    if options.stdout && options.check_only {
        bail!("--stdout and --check cannot be used together");
    }
    if list_dynamic_providers {
        return Ok(CompGenAction::ListDynamicProviders);
    }
    if let Some(dir) = audit_dir {
        return Ok(CompGenAction::Audit { dir });
    }

    let command_name = command_name.context("Missing required <command> argument")?;
    Ok(CompGenAction::Generate {
        options,
        command_name,
    })
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

fn dynamic_provider_list() -> String {
    DYNAMIC_COMPLETION_PROVIDERS.join("\n")
}

#[derive(Debug, Default)]
struct CompletionAudit {
    command_count: usize,
    string_count: usize,
    dynamic_count: usize,
    unknown_providers: BTreeMap<String, usize>,
}

fn audit_completion_dir(dir: &Path) -> Result<String> {
    let mut audit = CompletionAudit::default();
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("Failed to read completion dir '{}'", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let json = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read '{}'", path.display()))?;
        let value: Value = serde_json::from_str(&json)
            .with_context(|| format!("Invalid JSON in '{}'", path.display()))?;
        audit.command_count += 1;
        audit_command_value(&value, &mut audit);
    }

    let mut lines = vec![
        format!("commands={}", audit.command_count),
        format!("string_types={}", audit.string_count),
        format!("dynamic_types={}", audit.dynamic_count),
        format!("unknown_providers={}", audit.unknown_providers.len()),
    ];
    for (provider, count) in audit.unknown_providers {
        lines.push(format!("unknown_provider {provider} count={count}"));
    }
    Ok(lines.join("\n"))
}

fn audit_command_value(value: &Value, audit: &mut CompletionAudit) {
    let Some(obj) = value.as_object() else {
        return;
    };
    if let Some(options) = obj.get("global_options").and_then(Value::as_array) {
        for option in options {
            audit_option_value(option, audit);
        }
    }
    if let Some(arguments) = obj.get("arguments").and_then(Value::as_array) {
        for argument in arguments {
            audit_argument_value(argument, audit);
        }
    }
    if let Some(subcommands) = obj.get("subcommands").and_then(Value::as_array) {
        for subcommand in subcommands {
            audit_subcommand_value(subcommand, audit);
        }
    }
}

fn audit_subcommand_value(value: &Value, audit: &mut CompletionAudit) {
    let Some(obj) = value.as_object() else {
        return;
    };
    if let Some(options) = obj.get("options").and_then(Value::as_array) {
        for option in options {
            audit_option_value(option, audit);
        }
    }
    if let Some(arguments) = obj.get("arguments").and_then(Value::as_array) {
        for argument in arguments {
            audit_argument_value(argument, audit);
        }
    }
    if let Some(subcommands) = obj.get("subcommands").and_then(Value::as_array) {
        for subcommand in subcommands {
            audit_subcommand_value(subcommand, audit);
        }
    }
}

fn audit_option_value(value: &Value, audit: &mut CompletionAudit) {
    let Some(obj) = value.as_object() else {
        return;
    };
    if let Some(value_type) = obj.get("value_type") {
        audit_argument_type_value(value_type, audit);
    }
    if let Some(argument) = obj.get("argument") {
        audit_argument_value(argument, audit);
    }
}

fn audit_argument_value(value: &Value, audit: &mut CompletionAudit) {
    let Some(obj) = value.as_object() else {
        return;
    };
    if let Some(arg_type) = obj.get("type") {
        audit_argument_type_value(arg_type, audit);
    }
}

fn audit_argument_type_value(value: &Value, audit: &mut CompletionAudit) {
    let Some(type_name) = value.get("type").and_then(Value::as_str) else {
        return;
    };
    match type_name {
        "String" => audit.string_count += 1,
        "Dynamic" => {
            audit.dynamic_count += 1;
            let provider = value
                .get("data")
                .and_then(|data| data.get("provider"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if !is_known_dynamic_completion_provider(provider) {
                *audit
                    .unknown_providers
                    .entry(provider.to_string())
                    .or_insert(0) += 1;
            }
        }
        _ => {}
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
        if let Some(short) = short
            && !valid_short_option(short)
        {
            bail!("{option_path}.short has invalid option format '{short}'");
        }
        if let Some(long) = long
            && !valid_long_option(long)
        {
            bail!("{option_path}.long has invalid option format '{long}'");
        }
        if let Some(takes_value) = obj.get("takes_value")
            && !takes_value.is_boolean()
        {
            bail!("{option_path}.takes_value must be boolean");
        }
        if let Some(value_type) = obj.get("value_type") {
            validate_argument_type(value_type, &format!("{option_path}.value_type"))?;
        }
        if let Some(argument) = obj.get("argument") {
            validate_argument_object(argument, &format!("{option_path}.argument"))?;
        }
    }
    Ok(())
}

fn option_base(option: &str) -> &str {
    option.split_whitespace().next().unwrap_or("")
}

fn valid_short_option(option: &str) -> bool {
    let base = option_base(option);
    base.starts_with('-') && !base.starts_with("--") && base.len() > 1
}

fn valid_long_option(option: &str) -> bool {
    let base = option_base(option);
    base.starts_with('-') && base.len() > 1 && base != "--"
}

fn validate_arguments_array(value: &Value, path: &str) -> Result<()> {
    let args = value
        .as_array()
        .with_context(|| format!("{path} must be an array"))?;
    for (idx, arg) in args.iter().enumerate() {
        let arg_path = format!("{path}[{idx}]");
        validate_argument_object(arg, &arg_path)?;
    }
    Ok(())
}

fn validate_argument_object(value: &Value, path: &str) -> Result<()> {
    let obj = value
        .as_object()
        .with_context(|| format!("{path} must be an object"))?;
    let name_value = obj
        .get("name")
        .with_context(|| format!("{path}.name is required"))?;
    require_non_empty_string(name_value, &format!("{path}.name"))?;
    if let Some(arg_type) = obj.get("type") {
        validate_argument_type(arg_type, &format!("{path}.type"))?;
    }
    if let Some(required) = obj.get("required")
        && !required.is_boolean()
    {
        bail!("{path}.required must be boolean");
    }
    if let Some(multiple) = obj.get("multiple")
        && !multiple.is_boolean()
    {
        bail!("{path}.multiple must be boolean");
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

    if type_name == "Dynamic" {
        let data = obj
            .get("data")
            .with_context(|| format!("{path}.data is required for Dynamic"))?;
        let data_obj = data
            .as_object()
            .with_context(|| format!("{path}.data must be an object"))?;
        let provider = data_obj
            .get("provider")
            .with_context(|| format!("{path}.data.provider is required"))?;
        let provider = require_non_empty_string(provider, &format!("{path}.data.provider"))?;
        if !is_known_dynamic_completion_provider(provider) {
            bail!("{path}.data.provider has unknown Dynamic provider '{provider}'");
        }
        if let Some(scope) = data_obj.get("scope")
            && !scope.is_null()
            && scope.as_str().is_none()
        {
            bail!("{path}.data.scope must be a string or null");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CompGenAction, audit_completion_dir, dynamic_provider_list, parse_args,
        validate_completion_json,
    };
    use std::fs;

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
    fn validate_completion_aligns_runtime_option_formats() {
        let json = r#"
        {
          "command": "foo",
          "global_options": [
            { "short": "-f <FILE>" },
            { "short": "-123" },
            { "short": "-ofile" },
            { "long": "--123invalid" },
            { "long": "--type <TYPE>" },
            { "long": "-Xmx" }
          ]
        }
        "#;
        assert!(validate_completion_json(json, "foo").is_ok());
    }

    #[test]
    fn validate_completion_rejects_bare_option_markers() {
        let bare_short = r#"
        {
          "command": "foo",
          "global_options": [
            { "short": "-" }
          ]
        }
        "#;
        assert!(validate_completion_json(bare_short, "foo").is_err());

        let long_prefix_as_short = r#"
        {
          "command": "foo",
          "global_options": [
            { "short": "--verbose" }
          ]
        }
        "#;
        assert!(validate_completion_json(long_prefix_as_short, "foo").is_err());

        let bare_long = r#"
        {
          "command": "foo",
          "global_options": [
            { "long": "--" }
          ]
        }
        "#;
        assert!(validate_completion_json(bare_long, "foo").is_err());

        let bare_long_with_placeholder = r#"
        {
          "command": "foo",
          "global_options": [
            { "long": "-- <ARG>" }
          ]
        }
        "#;
        assert!(validate_completion_json(bare_long_with_placeholder, "foo").is_err());
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
    fn validate_completion_rejects_option_argument_script_type() {
        let json = r#"
        {
          "command": "foo",
          "global_options": [
            {
              "long": "--branch",
              "argument": {
                "name": "branch",
                "type": { "type": "Script", "data": "git branch" }
              }
            }
          ]
        }
        "#;
        assert!(validate_completion_json(json, "foo").is_err());
    }

    #[test]
    fn validate_completion_allows_dynamic_type() {
        let json = r#"
        {
          "command": "foo",
          "arguments": [
            {
              "name": "branch",
              "type": {
                "type": "Dynamic",
                "data": { "provider": "git.branch", "scope": "project" }
              }
            }
          ]
        }
        "#;
        assert!(validate_completion_json(json, "foo").is_ok());
    }

    #[test]
    fn validate_completion_rejects_unknown_dynamic_provider() {
        let json = r#"
        {
          "command": "foo",
          "arguments": [
            {
              "name": "branch",
              "type": {
                "type": "Dynamic",
                "data": { "provider": "git.unknown" }
              }
            }
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
        let action = parse_args(&args).unwrap();
        let CompGenAction::Generate {
            options,
            command_name,
        } = action
        else {
            panic!("expected generate action");
        };
        assert!(options.stdout);
        assert!(!options.check_only);
        assert_eq!(command_name, "git");
    }

    #[test]
    fn parse_args_accepts_check() {
        let args = vec!["--check".to_string(), "cargo".to_string()];
        let action = parse_args(&args).unwrap();
        let CompGenAction::Generate {
            options,
            command_name,
        } = action
        else {
            panic!("expected generate action");
        };
        assert!(!options.stdout);
        assert!(options.check_only);
        assert_eq!(command_name, "cargo");
    }

    #[test]
    fn parse_args_accepts_list_dynamic_providers() {
        let args = vec!["--list-dynamic-providers".to_string()];
        assert_eq!(
            parse_args(&args).unwrap(),
            CompGenAction::ListDynamicProviders
        );
    }

    #[test]
    fn parse_args_accepts_audit_dir() {
        let args = vec!["--audit".to_string(), "dsh/completions".to_string()];
        assert!(matches!(
            parse_args(&args).unwrap(),
            CompGenAction::Audit { dir } if dir == std::path::Path::new("dsh/completions")
        ));
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

    #[test]
    fn dynamic_provider_list_includes_reusable_providers() {
        let output = dynamic_provider_list();
        assert!(output.lines().any(|line| line == "git.branch"));
        assert!(output.lines().any(|line| line == "systemctl.unit"));
        assert!(output.lines().any(|line| line == "kernel.module"));
    }

    #[test]
    fn audit_completion_dir_counts_string_dynamic_and_unknown_provider() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("foo.json"),
            r#"
            {
              "command": "foo",
              "arguments": [
                { "name": "plain", "type": { "type": "String" } },
                { "name": "branch", "type": { "type": "Dynamic", "data": { "provider": "git.branch" } } },
                { "name": "bad", "type": { "type": "Dynamic", "data": { "provider": "bad.provider" } } }
              ]
            }
            "#,
        )
        .unwrap();

        let output = audit_completion_dir(dir.path()).unwrap();
        assert!(output.contains("commands=1"));
        assert!(output.contains("string_types=1"));
        assert!(output.contains("dynamic_types=2"));
        assert!(output.contains("unknown_providers=1"));
        assert!(output.contains("unknown_provider bad.provider count=1"));
    }
}

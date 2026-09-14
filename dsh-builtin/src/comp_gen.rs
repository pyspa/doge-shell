use crate::capability::AiCapability;
use crate::completion_generation::CompletionGenerationService;
use crate::{BuiltinFuture, ShellProxy};
use anyhow::{Context as _, Result, bail};
use dsh_types::completion::{DYNAMIC_COMPLETION_PROVIDERS, is_known_dynamic_completion_provider};
use dsh_types::{Context, ExitStatus};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

mod audit;
mod validate;
use audit::audit_completion_dir;
pub(crate) use validate::validate_completion_json;

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
pub fn command(ctx: &Context, argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.iter().any(|arg| arg == "--help" || arg == "-h") {
        ctx.write_stdout(usage()).ok();
        return ExitStatus::ExitedWith(0);
    }

    let args = &argv[1..];
    let action = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            ctx.write_stderr(&format!("Error: {:#}", e)).ok();
            ctx.write_stderr(usage()).ok();
            return ExitStatus::ExitedWith(1);
        }
    };

    let CompGenAction::Generate { .. } = action else {
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

    ctx.write_stderr("comp-gen: AI generation requires foreground async execution")
        .ok();
    ExitStatus::ExitedWith(1)
}

pub fn command_async<'a>(
    ctx: &'a Context,
    argv: Vec<String>,
    proxy: &'a mut dyn ShellProxy,
) -> BuiltinFuture<'a> {
    Box::pin(async move {
        if argv.iter().any(|arg| arg == "--help" || arg == "-h") {
            ctx.write_stdout(usage()).ok();
            return ExitStatus::ExitedWith(0);
        }

        let action = match parse_args(&argv[1..]) {
            Ok(parsed) => parsed,
            Err(e) => {
                ctx.write_stderr(&format!("Error: {:#}", e)).ok();
                ctx.write_stderr(usage()).ok();
                return ExitStatus::ExitedWith(1);
            }
        };

        let CompGenAction::Generate {
            options,
            command_name,
        } = action
        else {
            return run_non_generate_action(ctx, action);
        };

        let log_to_stderr = options.stdout;
        let json = match generate_completion_async(ctx, proxy, &command_name, log_to_stderr).await {
            Ok(json) => json,
            Err(e) => {
                ctx.write_stderr(&format!("Error: {:#}", e)).ok();
                return ExitStatus::ExitedWith(1);
            }
        };

        if options.check_only {
            ctx.write_stdout("OK").ok();
            return ExitStatus::ExitedWith(0);
        }
        if options.stdout {
            ctx.write_stdout(&json).ok();
            return ExitStatus::ExitedWith(0);
        }

        let path = match CompletionGenerationService::default_output_path(&command_name) {
            Ok(path) => path,
            Err(e) => {
                ctx.write_stderr(&format!("Error: {:#}", e)).ok();
                return ExitStatus::ExitedWith(1);
            }
        };
        match CompletionGenerationService::write_json_atomic(
            &path,
            &json,
            &command_name,
            options.force,
        ) {
            Ok(()) => {
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
    })
}

fn run_non_generate_action(ctx: &Context, action: CompGenAction) -> ExitStatus {
    match action {
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
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompGenOptions {
    stdout: bool,
    check_only: bool,
    force: bool,
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
    r#"Usage: comp-gen [--stdout] [--check] [--force] <command>
       comp-gen --list-dynamic-providers
       comp-gen --audit [completion-dir]

Options:
  --stdout                  Print generated JSON to stdout instead of saving
  --check                   Validate generated JSON and exit (no save)
  --force                   Atomically replace an existing completion file
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
        force: false,
    };
    let mut command_name: Option<String> = None;
    let mut list_dynamic_providers = false;
    let mut audit_dir: Option<PathBuf> = None;
    let mut audit_dir_explicit = false;

    for arg in args {
        match arg.as_str() {
            "--stdout" => options.stdout = true,
            "--check" => options.check_only = true,
            "--force" => options.force = true,
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
    if mode_count > 0
        && (options.stdout || options.check_only || options.force || command_name.is_some())
    {
        bail!("Listing/audit modes cannot be combined with generation options or <command>");
    }
    if options.stdout && options.check_only {
        bail!("--stdout and --check cannot be used together");
    }
    if options.force && (options.stdout || options.check_only) {
        bail!("--force can only be used when saving a generated completion");
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

async fn generate_completion_async(
    ctx: &Context,
    proxy: &mut (impl AiCapability + ?Sized),
    command_name: &str,
    log_to_stderr: bool,
) -> Result<String> {
    log(
        ctx,
        log_to_stderr,
        &format!("Fetching help text for '{}'...", command_name),
    );
    let help_text = CompletionGenerationService::collect_help_text(command_name)?;
    log(
        ctx,
        log_to_stderr,
        "Generating completion JSON via AI (this may take a moment)...",
    );
    let json = proxy.generate_completion(command_name, &help_text).await?;
    CompletionGenerationService::validate_json(&json, command_name)?;
    Ok(json)
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
    fn validate_completion_checks_top_level_options_alias() {
        let json = r#"
        {
          "command": "foo",
          "options": [
            { "short": "-v", "long": "--verbose" },
            { "long": "+O2" }
          ]
        }
        "#;
        assert!(validate_completion_json(json, "foo").is_ok());
    }

    #[test]
    fn validate_completion_rejects_invalid_top_level_options_alias() {
        let json = r#"
        {
          "command": "foo",
          "options": [
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
        let args = vec!["--audit".to_string(), "custom/completions".to_string()];
        assert!(matches!(
            parse_args(&args).unwrap(),
            CompGenAction::Audit { dir } if dir == std::path::Path::new("custom/completions")
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
        assert!(output.contains("empty_definitions=0"));
        assert!(output.contains("unknown_provider bad.provider count=1"));
    }

    #[test]
    fn audit_completion_dir_reports_empty_definitions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("empty.json"),
            r#"{ "command": "empty", "global_options": [], "subcommands": [] }"#,
        )
        .unwrap();

        let output = audit_completion_dir(dir.path()).unwrap();
        assert!(output.contains("empty_definitions=1"));
        assert!(output.contains("empty_definition empty.json"));
    }
}

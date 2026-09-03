//! `output-gen`: the `comp-gen` sibling for `output-schemas/*.json`.
//!
//! `comp-gen` teaches the shell how to *complete* a command; `output-gen`
//! teaches `|:` how to *parse* a command's stdout into a typed table. The
//! flow mirrors `comp-gen`'s (`crate::comp_gen`) closely on purpose: fetch a
//! sample, ask the AI for a declarative schema, validate it before trusting
//! it. Where it differs is what "validate" means -- a completion definition
//! is either well-formed JSON or not, but an output schema can be
//! well-formed JSON that still doesn't parse the very output it was
//! generated from (wrong separator, a header label the model invented,
//! a column that doesn't line up). So generation here always re-parses the
//! captured sample with the schema it just produced -- via
//! `dsh_types::output_text`, the same pure splitter `|:` uses at runtime --
//! and refuses to save a schema that can't parse its own sample.
//!
//! Usage: output-gen [--stdout] [--force] <command...>
//!        output-gen --check <command...>
//!        output-gen --audit [output-schema-dir]

use crate::capability::AiCapability;
use crate::completion_generation::CompletionGenerationService;
use crate::config_paths;
use crate::{BuiltinFuture, ShellProxy};
use anyhow::{Context as _, Result, anyhow, bail};
use dsh_openai::strip_code_fence;
use dsh_openai::turn::truncate_middle;
use dsh_types::output_schema::{ColumnType, OutputSchema, ParseMode, PreferSpec};
use dsh_types::output_text::{looks_like_type, split_rows};
use dsh_types::{Context, ExitStatus};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Description for the output-gen command
pub fn description() -> &'static str {
    "Generate a |: output-schema using AI, or validate existing ones"
}

pub fn command(ctx: &Context, argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.iter().any(|arg| arg == "--help" || arg == "-h") {
        ctx.write_stdout(usage()).ok();
        return ExitStatus::ExitedWith(0);
    }

    match parse_args(&argv[1..]) {
        Ok(OutputGenAction::Generate { .. }) => {
            ctx.write_stderr("output-gen: AI generation requires foreground async execution\n")
                .ok();
            ExitStatus::ExitedWith(1)
        }
        Ok(action) => run_non_generate_action(ctx, action),
        Err(e) => {
            ctx.write_stderr(&format!("Error: {:#}\n", e)).ok();
            ctx.write_stderr(usage()).ok();
            ExitStatus::ExitedWith(1)
        }
    }
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
                ctx.write_stderr(&format!("Error: {:#}\n", e)).ok();
                ctx.write_stderr(usage()).ok();
                return ExitStatus::ExitedWith(1);
            }
        };

        let OutputGenAction::Generate {
            options,
            command_line,
        } = action
        else {
            return run_non_generate_action(ctx, action);
        };

        let outcome = match generate_async(ctx, proxy, &command_line, options.stdout).await {
            Ok(outcome) => outcome,
            Err(e) => {
                ctx.write_stderr(&format!("Error: {:#}\n", e)).ok();
                return ExitStatus::ExitedWith(1);
            }
        };

        for warning in &outcome.warnings {
            ctx.write_stderr(&format!("output-gen: warning: {warning}\n"))
                .ok();
        }

        if options.stdout {
            ctx.write_stdout(&format!("{}\n", outcome.json)).ok();
            return ExitStatus::ExitedWith(0);
        }

        let path = output_path(&outcome.command);
        match write_schema_atomic(&path, &outcome.json, options.force) {
            Ok(()) => {
                ctx.write_stdout(&format!(
                    "output-schema for '{}' generated and saved to {} ({} sample row(s) parsed)\n",
                    outcome.command,
                    path.display(),
                    outcome.row_count
                ))
                .ok();
                ExitStatus::ExitedWith(0)
            }
            Err(e) => {
                ctx.write_stderr(&format!("Error: {:#}\n", e)).ok();
                ExitStatus::ExitedWith(1)
            }
        }
    })
}

fn run_non_generate_action(ctx: &Context, action: OutputGenAction) -> ExitStatus {
    match action {
        OutputGenAction::Check { command_line } => match run_check(&command_line) {
            Ok(report) => {
                ctx.write_stdout(&format!("{report}\n")).ok();
                ExitStatus::ExitedWith(0)
            }
            Err(e) => {
                ctx.write_stderr(&format!("Error: {:#}\n", e)).ok();
                ExitStatus::ExitedWith(1)
            }
        },
        OutputGenAction::Audit { dir } => match audit_output_schema_dir(&dir) {
            Ok(output) => {
                ctx.write_stdout(&format!("{output}\n")).ok();
                ExitStatus::ExitedWith(0)
            }
            Err(e) => {
                ctx.write_stderr(&format!("Error: {:#}\n", e)).ok();
                ExitStatus::ExitedWith(1)
            }
        },
        OutputGenAction::Generate { .. } => unreachable!(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputGenOptions {
    stdout: bool,
    force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OutputGenAction {
    Generate {
        options: OutputGenOptions,
        command_line: String,
    },
    Check {
        command_line: String,
    },
    Audit {
        dir: PathBuf,
    },
}

fn usage() -> &'static str {
    r#"Usage: output-gen [--stdout] [--force] <command...>
       output-gen --check <command...>
       output-gen --audit [output-schema-dir]

Generates a declarative output-schemas/*.json entry (see
command-output-schema.json) for <command...> by running it, sampling its
real output, and asking the AI to describe how to parse it. The generated
schema is always re-parsed against that same sample before being trusted;
generation fails rather than saving a schema that can't parse its own
output.

Options:
  --stdout           Print the generated JSON to stdout instead of saving
  --force            Atomically replace an existing schema file
  --check            Re-run <command...> and re-verify its saved schema,
                      without calling the AI
  --audit [dir]      Validate every *.json in dir (default: output-schemas)
  -h, --help         Show this help message
"#
}

fn parse_args(args: &[String]) -> Result<OutputGenAction> {
    let mut options = OutputGenOptions {
        stdout: false,
        force: false,
    };
    let mut check_only = false;
    let mut audit_dir: Option<PathBuf> = None;
    let mut audit_dir_explicit = false;
    let mut command_words: Vec<String> = Vec::new();

    for arg in args {
        match arg.as_str() {
            "--stdout" => options.stdout = true,
            "--force" => options.force = true,
            "--check" => check_only = true,
            "--audit" => {
                if audit_dir.is_some() {
                    bail!("--audit may only be specified once");
                }
                audit_dir = Some(PathBuf::from("output-schemas"));
                audit_dir_explicit = false;
            }
            "-h" | "--help" => {}
            _ if arg.starts_with('-') => bail!("Unknown option: {}", arg),
            _ => {
                if audit_dir.is_some() && !audit_dir_explicit {
                    audit_dir = Some(PathBuf::from(arg));
                    audit_dir_explicit = true;
                } else {
                    command_words.push(arg.clone());
                }
            }
        }
    }

    if let Some(dir) = audit_dir {
        if check_only || !command_words.is_empty() || options.stdout || options.force {
            bail!("--audit cannot be combined with other options or a command");
        }
        return Ok(OutputGenAction::Audit { dir });
    }

    let command_line = command_words.join(" ");
    if command_line.trim().is_empty() {
        bail!("Missing required <command...> argument");
    }

    if check_only {
        if options.stdout || options.force {
            bail!("--check cannot be combined with --stdout or --force");
        }
        return Ok(OutputGenAction::Check { command_line });
    }

    Ok(OutputGenAction::Generate {
        options,
        command_line,
    })
}

fn output_path(command_name: &str) -> PathBuf {
    config_paths::config_home()
        .join("output-schemas")
        .join(format!("{command_name}.json"))
}

fn write_schema_atomic(output_path: &Path, json: &str, force: bool) -> Result<()> {
    crate::atomic_write::write_atomic(output_path, json, force, "output-schema")
}

struct GenerateOutcome {
    command: String,
    json: String,
    row_count: usize,
    warnings: Vec<String>,
}

async fn generate_async(
    ctx: &Context,
    proxy: &mut (impl AiCapability + ?Sized),
    command_line: &str,
    log_to_stderr: bool,
) -> Result<GenerateOutcome> {
    let argv = shell_words::split(command_line).context("Could not tokenize the command line")?;
    let Some(command_name) = argv.first().cloned() else {
        bail!("Missing required <command...> argument");
    };
    CompletionGenerationService::validate_command_name(&command_name)?;

    log(
        ctx,
        log_to_stderr,
        &format!("Running '{command_line}' to capture a sample..."),
    );
    let captured = run_command(&argv)
        .with_context(|| format!("Failed to run '{command_line}' for a sample"))?;
    let sample = captured.text;
    if sample.trim().is_empty() {
        bail!("'{command_line}' produced no output to learn from");
    }
    let mut warnings = Vec::new();
    if !captured.exit_success {
        warnings.push(format!(
            "'{command_line}' exited with a failure status; the sample (and the schema \
             generated from it) may just describe an error message, not real output"
        ));
    }

    log(
        ctx,
        log_to_stderr,
        "Generating output-schema JSON via AI (this may take a moment)...",
    );
    let raw = ask_for_schema(proxy, &command_name, command_line, &sample).await?;
    let json = strip_code_fence(&raw);

    let schema: OutputSchema =
        serde_json::from_str(&json).context("AI returned JSON that isn't a valid output-schema")?;
    validate_schema_shape(&schema, &command_name)?;

    log(
        ctx,
        log_to_stderr,
        "Re-parsing the sample with the generated schema to verify it...",
    );
    let report = verify_schema(&schema, &argv, &sample)
        .context("generated schema failed verification against its own sample")?;
    warnings.extend(report.warnings);

    let formatted =
        serde_json::to_string_pretty(&schema).context("Failed to format the output-schema JSON")?;
    Ok(GenerateOutcome {
        command: command_name,
        json: formatted,
        row_count: report.row_count,
        warnings,
    })
}

fn log(ctx: &Context, to_stderr: bool, message: &str) {
    if to_stderr {
        let _ = ctx.write_stderr(&format!("{message}\n"));
    } else {
        let _ = ctx.write_stdout(&format!("{message}\n"));
    }
}

const MAX_SAMPLE_CHARS_IN_PROMPT: usize = 4000;

async fn ask_for_schema(
    proxy: &mut (impl AiCapability + ?Sized),
    command_name: &str,
    command_line: &str,
    sample: &str,
) -> Result<String> {
    let sample_for_prompt = truncate_middle(sample, MAX_SAMPLE_CHARS_IN_PROMPT);
    let system = r#"You write declarative output schemas for a shell's `|:` structured pipe.
Reply with ONLY a JSON object matching this shape (no prose, no code fence):

{
  "command": "<the command name, exactly>",
  "outputs": [
    {
      "subcommand": "<optional: required first non-option argument>",
      "when": { "args_include": ["..."], "args_exclude": ["..."] },
      "prefer": { "inject_args": ["..."], "parse": "json"|"json-lines"|"text", "json_root": "<optional>" },
      "text": {
        "separator": "whitespace" | "auto" | { "delimiter": "<string>" },
        "header_lines": <number, default 1>,
        "skip_prefixes": ["..."],
        "columns": [
          { "name": "snake_case_name", "header": "<optional, e.g. \"%CPU\">", "type": "string"|"int"|"float"|"percent"|"size"|"duration"|"date", "rest": false }
        ]
      }
    }
  ]
}

Rules:
- Every entry in "outputs" needs "text", "prefer", or both.
- Use "auto" separator only when columns are visually aligned under a header (like `docker ps`); "whitespace" for ordinary space-separated columns (like `ps aux`); a delimiter object for machine-readable text (TSV, etc).
- The LAST column may set "rest": true to greedily take the remainder of the line (e.g. a command or path that may contain spaces); no other column may.
- Column names are lowercase snake_case; set "header" only when it differs from the name in uppercase (e.g. name "cpu", header "%CPU").
- Only set "type" to something other than "string" when you are confident the column always holds that shape; a wrong type is worse than "string".
- Prefer a machine-readable "prefer" mode (JSON output flag) when the command actually supports one and you can see how from the sample or its flags; otherwise omit "prefer" and rely on "text" alone.
"#;
    let user = format!(
        "Command: {command_line}\n\nSample output of running it just now:\n```\n{sample_for_prompt}\n```"
    );
    let messages = vec![
        json!({"role": "system", "content": system}),
        json!({"role": "user", "content": user}),
    ];
    proxy.ask(messages).await.with_context(|| {
        format!("AI request failed while generating a schema for '{command_name}'")
    })
}

/// The same invariants `dsh`'s embedded-schema test enforces
/// (`output_schema::loader::tests::embedded_output_schemas_are_valid`),
/// checked here before a generated schema is ever saved.
fn validate_schema_shape(schema: &OutputSchema, expected_command: &str) -> Result<()> {
    if schema.command != expected_command {
        bail!(
            "Command mismatch: expected '{}', got '{}'",
            expected_command,
            schema.command
        );
    }
    if schema.outputs.is_empty() {
        bail!("schema has no \"outputs\" entries");
    }
    for spec in &schema.outputs {
        if spec.prefer.is_none() && spec.text.is_none() {
            bail!("every output spec needs \"prefer\", \"text\", or both");
        }
        if let Some(text) = &spec.text {
            if text.columns.is_empty() {
                bail!("a text spec has no columns");
            }
            for column in &text.columns[..text.columns.len() - 1] {
                if column.rest {
                    bail!("only the last column may set \"rest\": true");
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct VerifyReport {
    row_count: usize,
    warnings: Vec<String>,
}

/// Re-parses `sample` with the schema that just generated from it, using the
/// exact splitter `|:` uses at runtime (`dsh_types::output_text`). Returns
/// `Err` when the schema is unusable outright (no matching spec, unparsable
/// text spec); a schema that parses but looks shaky (a declared numeric
/// column mostly not looking numeric) is still saved, with the reason
/// surfaced as a warning instead.
fn verify_schema(schema: &OutputSchema, argv: &[String], sample: &str) -> Result<VerifyReport> {
    let args = &argv[1..];
    let spec = schema
        .outputs
        .iter()
        .find(|spec| spec.matches(args))
        .ok_or_else(|| anyhow!("no output spec matches the arguments {:?}", args))?;

    let mut warnings = Vec::new();

    if let Some(prefer) = &spec.prefer {
        let mut prefer_argv = argv.to_vec();
        prefer_argv.extend(prefer.inject_args.iter().cloned());
        match run_command(&prefer_argv) {
            Ok(captured) => match verify_prefer(&captured.text, prefer) {
                Ok(row_count) => {
                    if !captured.exit_success {
                        warnings.push(format!(
                            "'{}' (the \"prefer\" args) exited with a failure status",
                            prefer_argv.join(" ")
                        ));
                    }
                    return Ok(VerifyReport { row_count, warnings });
                }
                Err(e) => warnings.push(format!(
                    "\"prefer\" mode's own output failed to parse ({e}); `|:` would silently \
                     fall back to \"text\" here, so make sure \"text\" alone is reliable"
                )),
            },
            Err(e) => warnings.push(format!(
                "could not re-run the command with the injected \"prefer\" args to verify them ({e})"
            )),
        }
    }

    let Some(text) = &spec.text else {
        bail!(warnings.join("; "));
    };

    let rows = split_rows(sample, text).map_err(|e| anyhow!("\"text\" spec: {e}"))?;

    if rows.is_empty() {
        // `split_rows` silently drops any data line that split into
        // all-empty fields, so an empty `rows` on its own can't tell "the
        // command genuinely printed nothing" (fine) apart from "every data
        // line failed to split" (the schema is broken). `count_data_lines`
        // runs the same header/skip_prefix filtering without that drop, so
        // a positive count here means real data lines existed and none of
        // them produced anything -- exactly the case the module doc
        // promises never to save silently.
        let data_lines = dsh_types::output_text::count_data_lines(sample, text)
            .map_err(|e| anyhow!("\"text\" spec: {e}"))?;
        if data_lines > 0 {
            bail!(
                "\"text\" spec parsed 0 of {data_lines} data line(s) in the sample -- \
                 check separator/header_lines/columns"
            );
        }
    }

    let mut low_match_columns = Vec::new();
    for (index, column) in text.columns.iter().enumerate() {
        if matches!(
            column.column_type,
            ColumnType::String | ColumnType::Duration | ColumnType::Date
        ) {
            continue;
        }
        let cells: Vec<&String> = rows.iter().filter_map(|row| row.get(index)).collect();
        if cells.is_empty() {
            continue;
        }
        let matching = cells
            .iter()
            .filter(|cell| looks_like_type(cell, column.column_type))
            .count();
        if matching * 2 < cells.len() {
            low_match_columns.push(column.name.clone());
        }
    }
    if !low_match_columns.is_empty() {
        warnings.push(format!(
            "columns declared as a non-string type but mostly not shaped like one: {}",
            low_match_columns.join(", ")
        ));
    }

    Ok(VerifyReport {
        row_count: rows.len(),
        warnings,
    })
}

/// Whether `prefer`'s own output parses, and how many rows it would yield --
/// a proxy for `Table::from_json_value`'s row count (array length, or 1 for
/// a single object/primitive) without depending on `lisp::model::Table`.
fn verify_prefer(output: &str, prefer: &PreferSpec) -> std::result::Result<usize, String> {
    match prefer.parse {
        ParseMode::Json => {
            let value: serde_json::Value =
                serde_json::from_str(output).map_err(|e| format!("json parse: {e}"))?;
            let value = match &prefer.json_root {
                Some(root) => value.get(root).cloned().unwrap_or(value),
                None => value,
            };
            Ok(match value {
                serde_json::Value::Array(items) => items.len(),
                _ => 1,
            })
        }
        ParseMode::JsonLines => {
            let count = output
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(serde_json::from_str::<serde_json::Value>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| format!("json-lines parse: {e}"))?
                .len();
            Ok(count)
        }
        ParseMode::Text => Err(
            "\"prefer\" is declared with parse: \"text\", which just means \"use the text spec\""
                .to_string(),
        ),
    }
}

struct CommandSample {
    text: String,
    exit_success: bool,
}

fn run_command(argv: &[String]) -> Result<CommandSample> {
    let Some((program, args)) = argv.split_first() else {
        bail!("empty command");
    };
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("Failed to execute '{}'", argv.join(" ")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let text = if stdout.trim().is_empty() {
        stderr
    } else {
        stdout
    };
    Ok(CommandSample {
        text,
        exit_success: output.status.success(),
    })
}

fn run_check(command_line: &str) -> Result<String> {
    let argv = shell_words::split(command_line).context("Could not tokenize the command line")?;
    let Some(command_name) = argv.first().cloned() else {
        bail!("Missing required <command...> argument");
    };
    CompletionGenerationService::validate_command_name(&command_name)?;

    let (json, source) = find_schema_json(&command_name).ok_or_else(|| {
        anyhow!(
            "no output-schema found for '{command_name}'; run `output-gen {command_line}` first"
        )
    })?;
    let schema: OutputSchema =
        serde_json::from_str(&json).with_context(|| format!("Invalid JSON in {source}"))?;
    validate_schema_shape(&schema, &command_name)?;

    let captured = run_command(&argv)
        .with_context(|| format!("Failed to run '{command_line}' for a sample"))?;
    let sample = captured.text;
    if sample.trim().is_empty() {
        bail!("'{command_line}' produced no output to check against");
    }

    let report = verify_schema(&schema, &argv, &sample)?;
    let mut lines = vec![format!(
        "OK: {source} ({} row(s) parsed from a fresh sample)",
        report.row_count
    )];
    if !captured.exit_success {
        lines.push(format!(
            "warning: '{command_line}' exited with a failure status during this check"
        ));
    }
    for warning in report.warnings {
        lines.push(format!("warning: {warning}"));
    }
    Ok(lines.join("\n"))
}

/// The user's override first (same directory `output-gen` saves to), then
/// the repository's own `output-schemas/` relative to the current
/// directory (so a contributor can `--check` a schema they are editing in
/// a checkout without installing it), then the schemas embedded into this
/// binary itself -- the same three-tier precedence `|:` uses at runtime
/// (`dsh::output_schema::loader`), reimplemented here because `dsh-builtin`
/// can't depend on `dsh` for its loader. Without this last tier, `--check`
/// couldn't verify any of the schemas the shell ships with out of the box
/// on a normal install (only user overrides and an in-repo checkout).
/// Returns the JSON text and a human-readable description of where it came
/// from.
fn find_schema_json(command_name: &str) -> Option<(String, String)> {
    let user_path = output_path(command_name);
    if let Ok(json) = fs::read_to_string(&user_path) {
        return Some((json, user_path.display().to_string()));
    }
    let repo_path = PathBuf::from("output-schemas").join(format!("{command_name}.json"));
    if let Ok(json) = fs::read_to_string(&repo_path) {
        return Some((json, repo_path.display().to_string()));
    }
    let embedded_name = format!("{command_name}.json");
    EmbeddedOutputSchemas::get(&embedded_name).map(|file| {
        (
            String::from_utf8_lossy(&file.data).into_owned(),
            format!("<embedded {embedded_name}>"),
        )
    })
}

/// The same `output-schemas/` directory `dsh`'s own loader embeds
/// (`dsh/src/output_schema/loader.rs`'s `OutputSchemaAssets`); duplicated
/// here only because `dsh-builtin` cannot depend on `dsh` for it. Adding a
/// schema JSON needs a `touch` of this file to make it into a release build
/// (rust-embed tracks files, not the directory) -- same caveat as the
/// original.
#[derive(rust_embed::RustEmbed)]
#[folder = "../output-schemas/"]
struct EmbeddedOutputSchemas;

fn audit_output_schema_dir(dir: &Path) -> Result<String> {
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("Failed to read output-schema dir '{}'", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.path());

    let mut schema_count = 0;
    let mut spec_count = 0;
    let mut problems: Vec<String> = Vec::new();

    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<unknown>")
            .to_string();
        let json = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read '{}'", path.display()))?;
        let schema: OutputSchema = match serde_json::from_str(&json) {
            Ok(schema) => schema,
            Err(e) => {
                problems.push(format!("{name}: invalid JSON ({e})"));
                continue;
            }
        };
        if let Err(e) = validate_schema_shape(&schema, &schema.command) {
            problems.push(format!("{name}: {e:#}"));
            continue;
        }
        schema_count += 1;
        spec_count += schema.outputs.len();
    }

    let mut lines = vec![
        format!("schemas={schema_count}"),
        format!("output_specs={spec_count}"),
        format!("problems={}", problems.len()),
    ];
    lines.extend(problems);
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_joins_unquoted_words_into_one_command_line() {
        let args = vec!["ps".to_string(), "aux".to_string()];
        let action = parse_args(&args).unwrap();
        assert_eq!(
            action,
            OutputGenAction::Generate {
                options: OutputGenOptions {
                    stdout: false,
                    force: false
                },
                command_line: "ps aux".to_string(),
            }
        );
    }

    #[test]
    fn parse_args_accepts_stdout_and_force() {
        let args = vec![
            "--stdout".to_string(),
            "--force".to_string(),
            "ps".to_string(),
            "aux".to_string(),
        ];
        let action = parse_args(&args).unwrap();
        let OutputGenAction::Generate {
            options,
            command_line,
        } = action
        else {
            panic!("expected generate action");
        };
        assert!(options.stdout);
        assert!(options.force);
        assert_eq!(command_line, "ps aux");
    }

    #[test]
    fn parse_args_accepts_check() {
        let args = vec!["--check".to_string(), "ps".to_string(), "aux".to_string()];
        assert_eq!(
            parse_args(&args).unwrap(),
            OutputGenAction::Check {
                command_line: "ps aux".to_string()
            }
        );
    }

    #[test]
    fn parse_args_rejects_check_with_stdout() {
        let args = vec![
            "--check".to_string(),
            "--stdout".to_string(),
            "ps".to_string(),
        ];
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn parse_args_accepts_audit_with_default_and_explicit_dir() {
        assert_eq!(
            parse_args(&["--audit".to_string()]).unwrap(),
            OutputGenAction::Audit {
                dir: PathBuf::from("output-schemas")
            }
        );
        assert_eq!(
            parse_args(&["--audit".to_string(), "custom".to_string()]).unwrap(),
            OutputGenAction::Audit {
                dir: PathBuf::from("custom")
            }
        );
    }

    /// Before the fix, the unknown-option guard stopped firing once
    /// `--audit` had been seen (its own directory argument doesn't start
    /// with `-`, so the guard never needed that extra allowance), silently
    /// treating a mistyped flag as the audit directory name instead.
    #[test]
    fn parse_args_rejects_unknown_option_after_audit() {
        assert!(parse_args(&["--audit".to_string(), "--bogus".to_string()]).is_err());
    }

    #[test]
    fn parse_args_rejects_audit_combined_with_a_command() {
        let args = vec!["--audit".to_string(), "dir".to_string(), "ps".to_string()];
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn parse_args_rejects_missing_command() {
        assert!(parse_args(&[]).is_err());
    }

    #[test]
    fn validate_schema_shape_accepts_a_minimal_valid_schema() {
        let schema: OutputSchema = serde_json::from_str(
            r#"{"command":"ps","outputs":[{"text":{"separator":"whitespace","header_lines":1,"columns":[{"name":"a"}]}}]}"#,
        )
        .unwrap();
        assert!(validate_schema_shape(&schema, "ps").is_ok());
    }

    #[test]
    fn validate_schema_shape_rejects_command_mismatch() {
        let schema: OutputSchema = serde_json::from_str(
            r#"{"command":"other","outputs":[{"text":{"separator":"whitespace","header_lines":1,"columns":[{"name":"a"}]}}]}"#,
        )
        .unwrap();
        assert!(validate_schema_shape(&schema, "ps").is_err());
    }

    #[test]
    fn validate_schema_shape_rejects_rest_on_a_non_last_column() {
        let schema: OutputSchema = serde_json::from_str(
            r#"{"command":"ps","outputs":[{"text":{"separator":"whitespace","header_lines":1,
               "columns":[{"name":"a","rest":true},{"name":"b"}]}}]}"#,
        )
        .unwrap();
        assert!(validate_schema_shape(&schema, "ps").is_err());
    }

    #[test]
    fn validate_schema_shape_rejects_a_spec_with_neither_prefer_nor_text() {
        let schema: OutputSchema =
            serde_json::from_str(r#"{"command":"ps","outputs":[{}]}"#).unwrap();
        assert!(validate_schema_shape(&schema, "ps").is_err());
    }

    #[test]
    fn verify_schema_parses_a_matching_text_spec() {
        let schema: OutputSchema = serde_json::from_str(
            r#"{"command":"ps","outputs":[{"when":{"args_include":["aux"]},
               "text":{"separator":"whitespace","header_lines":1,
               "columns":[{"name":"user"},{"name":"pid","type":"int"},{"name":"command","rest":true}]}}]}"#,
        )
        .unwrap();
        let argv = vec!["ps".to_string(), "aux".to_string()];
        let sample = "USER PID COMMAND\nroot 1 /sbin/init\n";
        let report = verify_schema(&schema, &argv, sample).unwrap();
        assert_eq!(report.row_count, 1);
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn verify_schema_errors_when_no_spec_matches() {
        let schema: OutputSchema = serde_json::from_str(
            r#"{"command":"ps","outputs":[{"when":{"args_include":["-ef"]},
               "text":{"separator":"whitespace","header_lines":1,"columns":[{"name":"a"}]}}]}"#,
        )
        .unwrap();
        let argv = vec!["ps".to_string(), "aux".to_string()];
        assert!(verify_schema(&schema, &argv, "A\nb\n").is_err());
    }

    #[test]
    fn verify_schema_errors_when_text_spec_cannot_parse_the_sample() {
        let schema: OutputSchema = serde_json::from_str(
            r#"{"command":"ps","outputs":[{"text":{"separator":"auto","header_lines":1,
               "columns":[{"name":"a","header":"NOPE"}]}}]}"#,
        )
        .unwrap();
        let argv = vec!["ps".to_string()];
        assert!(verify_schema(&schema, &argv, "USER PID\nroot 1\n").is_err());
    }

    #[test]
    fn verify_schema_errors_when_every_data_line_splits_to_nothing() {
        // A non-blank data line ("," has real characters, so the blank-line
        // filter doesn't drop it) that still produces all-empty fields after
        // splitting: `split_rows` silently drops it, yielding an *empty*
        // `Vec` rather than an error. Before the fix this was reported as a
        // successful generation with "0 sample row(s) parsed".
        let schema: OutputSchema = serde_json::from_str(
            r#"{"command":"cmd","outputs":[{"text":{"separator":{"delimiter":","},
               "header_lines":0,"columns":[{"name":"a"}]}}]}"#,
        )
        .unwrap();
        let argv = vec!["cmd".to_string()];
        let err = verify_schema(&schema, &argv, ",,,\n").unwrap_err();
        assert!(err.to_string().contains("parsed 0 of"), "{err}");
    }

    #[test]
    fn verify_schema_allows_genuinely_empty_output() {
        // Zero data lines at all (just a header) is not a schema bug -- the
        // command may legitimately have printed nothing this time.
        let schema: OutputSchema = serde_json::from_str(
            r#"{"command":"docker","outputs":[{"when":{"args_include":["ps"]},
               "text":{"separator":"whitespace","header_lines":1,"columns":[{"name":"a"}]}}]}"#,
        )
        .unwrap();
        let argv = vec!["docker".to_string(), "ps".to_string()];
        let report = verify_schema(&schema, &argv, "CONTAINER ID\n").unwrap();
        assert_eq!(report.row_count, 0);
    }

    #[test]
    fn verify_schema_warns_when_a_typed_column_mostly_does_not_look_typed() {
        let schema: OutputSchema = serde_json::from_str(
            r#"{"command":"ps","outputs":[{"text":{"separator":"whitespace","header_lines":1,
               "columns":[{"name":"pid","type":"int"}]}}]}"#,
        )
        .unwrap();
        let argv = vec!["ps".to_string()];
        let sample = "PID\nnotanumber\nalsonotanumber\n";
        let report = verify_schema(&schema, &argv, sample).unwrap();
        assert!(!report.warnings.is_empty());
    }

    #[test]
    fn verify_prefer_counts_json_array_rows() {
        let prefer = PreferSpec {
            inject_args: vec![],
            parse: ParseMode::Json,
            json_root: None,
        };
        assert_eq!(verify_prefer(r#"[{"a":1},{"a":2}]"#, &prefer), Ok(2));
    }

    #[test]
    fn verify_prefer_unwraps_json_root() {
        let prefer = PreferSpec {
            inject_args: vec![],
            parse: ParseMode::Json,
            json_root: Some("items".to_string()),
        };
        assert_eq!(
            verify_prefer(r#"{"items":[{"a":1},{"a":2},{"a":3}]}"#, &prefer),
            Ok(3)
        );
    }

    #[test]
    fn verify_prefer_counts_json_lines() {
        let prefer = PreferSpec {
            inject_args: vec![],
            parse: ParseMode::JsonLines,
            json_root: None,
        };
        assert_eq!(verify_prefer("{\"a\":1}\n{\"a\":2}\n", &prefer), Ok(2));
    }

    #[test]
    fn verify_prefer_reports_invalid_json() {
        let prefer = PreferSpec {
            inject_args: vec![],
            parse: ParseMode::Json,
            json_root: None,
        };
        assert!(verify_prefer("not json", &prefer).is_err());
    }

    #[test]
    fn audit_output_schema_dir_counts_schemas_and_specs() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("ps.json"),
            r#"{"command":"ps","outputs":[{"text":{"separator":"whitespace","header_lines":1,"columns":[{"name":"a"}]}}]}"#,
        )
        .unwrap();
        fs::write(dir.path().join("broken.json"), "{ not json").unwrap();

        let output = audit_output_schema_dir(dir.path()).unwrap();
        assert!(output.contains("schemas=1"));
        assert!(output.contains("output_specs=1"));
        assert!(output.contains("problems=1"));
        assert!(output.contains("broken.json"));
    }
}

//! Checking a generated output-schema against reality: re-parsing the captured sample it was generated from (`verify_schema`), the `--check`/`--audit` entry points that re-verify a schema already on
//! disk without calling the AI, and the plain-JSON shape check every schema must pass before either.
use super::*;
use std::fs;

/// The same invariants `dsh`'s embedded-schema test enforces
/// (`output_schema::loader::tests::embedded_output_schemas_are_valid`),
/// checked here before a generated schema is ever saved.
pub(super) fn validate_schema_shape(schema: &OutputSchema, expected_command: &str) -> Result<()> {
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
pub(super) struct VerifyReport {
    pub(super) row_count: usize,
    pub(super) warnings: Vec<String>,
}
/// Re-parses `sample` with the schema that just generated from it, using the
/// exact splitter `|:` uses at runtime (`dsh_types::output_text`). Returns
/// `Err` when the schema is unusable outright (no matching spec, unparsable
/// text spec); a schema that parses but looks shaky (a declared numeric
/// column mostly not looking numeric) is still saved, with the reason
/// surfaced as a warning instead.
pub(super) fn verify_schema(
    snapshot: &dsh_types::process_runtime::CommandRuntimeSnapshot,
    schema: &OutputSchema,
    argv: &[String],
    sample: &str,
) -> Result<VerifyReport> {
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
        match run_command(snapshot, &prefer_argv) {
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
pub(super) fn verify_prefer(
    output: &str,
    prefer: &PreferSpec,
) -> std::result::Result<usize, String> {
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
pub(super) struct CommandSample {
    pub(super) text: String,
    pub(super) exit_success: bool,
}
pub(super) fn run_command(
    snapshot: &dsh_types::process_runtime::CommandRuntimeSnapshot,
    argv: &[String],
) -> Result<CommandSample> {
    let Some((program, args)) = argv.split_first() else {
        bail!("empty command");
    };
    // The sampled program resolves through the logical runtime and runs
    // with exactly the exported child environment — the same binary the
    // shell would run, never the process-global PATH.
    let output = snapshot
        .std_command(program)
        .ok_or_else(|| anyhow::anyhow!("Failed to execute '{}'", argv.join(" ")))?
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
pub(super) fn run_check(
    snapshot: &dsh_types::process_runtime::CommandRuntimeSnapshot,
    command_line: &str,
) -> Result<String> {
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

    let captured = run_command(snapshot, &argv)
        .with_context(|| format!("Failed to run '{command_line}' for a sample"))?;
    let sample = captured.text;
    if sample.trim().is_empty() {
        bail!("'{command_line}' produced no output to check against");
    }

    let report = verify_schema(snapshot, &schema, &argv, &sample)?;
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
pub(super) fn audit_output_schema_dir(dir: &Path) -> Result<String> {
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

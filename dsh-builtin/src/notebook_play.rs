use super::ShellProxy;
use crate::interactive_input;
use crate::runbook;
use anyhow::Result;
use dsh_types::notebook::{BlockKind, Notebook};
use dsh_types::{Context, ExitStatus};
use std::collections::HashMap;
use std::io::{self, Write};

pub fn description() -> &'static str {
    "Play a notebook file (execute code blocks interactively)"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let args = match parse_args(&argv[1..]) {
        Ok(args) => args,
        Err(err) => {
            let _ = ctx.write_stderr(&format!("notebook-play: {err}\n"));
            let _ = ctx.write_stderr("Usage: notebook-play <file> [--var name=value ...]\n");
            return ExitStatus::ExitedWith(1);
        }
    };

    match run_play(ctx, &args, proxy) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            let _ = ctx.write_stderr(&format!("notebook-play: {}\n", e));
            ExitStatus::ExitedWith(1)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PlayArgs {
    file: String,
    /// Pre-supplied `{{placeholder}}` values; names given here are never
    /// prompted for.
    vars: HashMap<String, String>,
}

fn parse_args(args: &[String]) -> Result<PlayArgs, String> {
    let mut file = None;
    let mut vars = HashMap::new();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--var" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err("--var requires name=value".to_string());
                };
                let Some((name, value)) = value.split_once('=') else {
                    return Err(format!("--var requires name=value, got: {value}"));
                };
                vars.insert(name.to_string(), value.to_string());
            }
            value if value.starts_with("--") => {
                return Err(format!("unknown option: {value}"));
            }
            value => {
                if file.replace(value.to_string()).is_some() {
                    return Err("expected exactly one file".to_string());
                }
            }
        }
        index += 1;
    }

    let Some(file) = file else {
        return Err("expected a notebook file".to_string());
    };
    Ok(PlayArgs { file, vars })
}

/// Placeholders across the notebook that still need a prompt, in order of
/// first appearance and asked at most once each.
///
/// Names already supplied via `--var` are skipped, and a name that appears in
/// several blocks is only listed once — accepting a default answers it for
/// the whole notebook, which is why "already asked" cannot be inferred from
/// the collected values.
fn placeholders_to_prompt(
    code_blocks: &[String],
    supplied: &HashMap<String, String>,
) -> Vec<runbook::Placeholder> {
    let mut pending: Vec<runbook::Placeholder> = Vec::new();
    for code in code_blocks {
        for placeholder in runbook::parse_placeholders(code) {
            if supplied.contains_key(&placeholder.name)
                || pending.iter().any(|seen| seen.name == placeholder.name)
            {
                continue;
            }
            pending.push(placeholder);
        }
    }
    pending
}

fn run_play(ctx: &Context, args: &PlayArgs, proxy: &mut dyn ShellProxy) -> Result<()> {
    let path_buf = std::path::PathBuf::from(&args.file);
    if !path_buf.exists() {
        anyhow::bail!("File not found: {}", args.file);
    }

    let notebook = Notebook::load_from_file(&path_buf)?;

    // Filter for executable code blocks (bash, sh, or no language specified)
    let blocks: Vec<_> = notebook
        .blocks
        .iter()
        .filter(|b| match &b.kind {
            BlockKind::Code(lang) => lang == "bash" || lang == "sh" || lang.is_empty(),
            _ => false,
        })
        .collect();

    if blocks.is_empty() {
        let _ = ctx.write_stdout("No executable blocks found in notebook.\n");
        return Ok(());
    }

    // Resolve `{{placeholder}}` values once for the whole notebook: values
    // from --var win, everything else is prompted for (empty input keeps the
    // marker's default).
    let mut values = args.vars.clone();
    let code_blocks: Vec<String> = blocks.iter().map(|block| block.raw_content()).collect();
    for placeholder in placeholders_to_prompt(&code_blocks, &values) {
        let prompt = match &placeholder.default {
            Some(default) => format!("{} [{}]: ", placeholder.name, default),
            None => format!("{}: ", placeholder.name),
        };
        let _ = ctx.write_stdout(&prompt);
        let _ = io::stdout().flush();
        let mut input = String::new();
        interactive_input::read_line(ctx, &mut input)?;
        let input = input.trim();
        if !input.is_empty() {
            values.insert(placeholder.name, input.to_string());
        }
        // Empty input: leave the name out so substitution falls back to
        // the default written in the notebook.
    }

    let _ = io::stdin();
    let _ = io::stdout();

    for (i, block) in blocks.iter().enumerate() {
        let code = runbook::substitute_placeholders(&block.raw_content(), &values);
        let _ = ctx.write_stdout(&format!("\n--- Block {} ---\n", i + 1));
        let _ = ctx.write_stdout(&format!("{}\n", code.trim()));
        let _ = ctx.write_stdout("Execute? [Y/n/q] ");
        let _ = io::stdout().flush();

        let mut input = String::new();
        interactive_input::read_line(ctx, &mut input)?;
        let choice = input.trim().to_lowercase();

        if choice == "q" {
            break;
        } else if choice == "n" {
            continue;
        } else {
            // Execute the code
            for line in code.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }

                let _ = ctx.write_stdout(&format!("> {}\n", line));

                // Currently dsh-builtin's dispatch is limited (assumes command + args),
                // but real execution needs full evaluation which is in dsh::shell::eval.
                // Since we are in dsh-builtin, we can only call what's exposed in ShellProxy.
                // Assuming proxy.dispatch simply hands off execution to the main shell logic
                // (or executes builtins).
                // Note: The previous implementation also just called dispatch with empty argv.
                // We preserve that behavior here.
                let _ = proxy.dispatch(ctx, line, vec![]);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_file_and_vars() {
        let args = parse_args(&[
            "runbook.md".to_string(),
            "--var".to_string(),
            "host=example.com".to_string(),
        ])
        .unwrap();
        assert_eq!(args.file, "runbook.md");
        assert_eq!(
            args.vars.get("host").map(String::as_str),
            Some("example.com")
        );
    }

    #[test]
    fn each_placeholder_is_prompted_at_most_once() {
        let blocks = vec![
            "ssh {{host:localhost}} uptime".to_string(),
            "echo {{message}}".to_string(),
            // Same name again: answering it once (even by accepting the
            // default) must not produce a second prompt.
            "scp file {{host:localhost}}:/tmp".to_string(),
        ];
        let pending = placeholders_to_prompt(&blocks, &HashMap::new());
        assert_eq!(
            pending.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["host", "message"]
        );
        assert_eq!(pending[0].default.as_deref(), Some("localhost"));

        // --var answers a placeholder without prompting.
        let supplied = HashMap::from([("host".to_string(), "example.com".to_string())]);
        let pending = placeholders_to_prompt(&blocks, &supplied);
        assert_eq!(
            pending.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["message"]
        );
    }

    #[test]
    fn go_template_blocks_prompt_for_nothing() {
        let blocks = vec!["docker ps --format '{{json .}}'".to_string()];
        assert!(placeholders_to_prompt(&blocks, &HashMap::new()).is_empty());
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&["a.md".to_string(), "b.md".to_string()]).is_err());
        assert!(parse_args(&["a.md".to_string(), "--var".to_string()]).is_err());
        assert!(
            parse_args(&[
                "a.md".to_string(),
                "--var".to_string(),
                "novalue".to_string()
            ])
            .is_err()
        );
        assert!(parse_args(&["a.md".to_string(), "--unknown".to_string()]).is_err());
    }
}

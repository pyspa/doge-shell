use super::ShellProxy;
use crate::interactive_input;
use crate::runbook;
use anyhow::{Context as _, Result};
use dsh_types::notebook::{BlockKind, Notebook};
use dsh_types::{Context, ExitStatus};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::os::fd::{BorrowedFd, OwnedFd, RawFd};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;

pub fn description() -> &'static str {
    "Play a notebook file (execute code blocks interactively)"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    let args = match parse_args(&argv[1..]) {
        Ok(args) => args,
        Err(err) => {
            let _ = ctx.write_stderr(&format!("notebook-play: {err}"));
            let _ = ctx.write_stderr("Usage: notebook-play <file> [--var name=value ...]");
            return ExitStatus::ExitedWith(1);
        }
    };

    match run_play(ctx, &args, proxy) {
        Ok(code) => ExitStatus::ExitedWith(code),
        Err(e) => {
            let _ = ctx.write_stderr(&format!("notebook-play: {e:#}"));
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
fn placeholders_to_prompt<'a>(
    code_blocks: impl IntoIterator<Item = &'a str>,
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

// The Context retains ownership of each descriptor, including redirected streams.
fn duplicate_fd(fd: RawFd) -> io::Result<OwnedFd> {
    unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()
}

fn run_play(ctx: &Context, args: &PlayArgs, proxy: &mut dyn ShellProxy) -> Result<i32> {
    let notebook = Notebook::load_from_file(Path::new(&args.file))
        .with_context(|| format!("cannot read {}", args.file))?;
    let blocks: Vec<_> = notebook
        .blocks
        .iter()
        .filter_map(|block| {
            let shell = match &block.kind {
                BlockKind::Code(lang) if lang == "bash" => "bash",
                BlockKind::Code(lang) if lang == "sh" || lang.is_empty() => "/bin/sh",
                _ => return None,
            };
            Some((shell, block.raw_content()))
        })
        .collect();
    if blocks.is_empty() {
        ctx.write_stdout("No executable blocks found in notebook.")?;
        return Ok(0);
    }

    // A child can read the same input after its confirmation. Reading past
    // the newline here would consume bytes that belong to its script.
    let prompt_file = if ctx.infile == nix::libc::STDIN_FILENO
        && let Some(tty) = interactive_input::terminal_input(ctx)
    {
        tty
    } else {
        File::from(duplicate_fd(ctx.infile)?)
    };
    let mut prompts = BufReader::with_capacity(1, prompt_file);
    let mut input = String::new();
    let mut values = args.vars.clone();
    for placeholder in placeholders_to_prompt(blocks.iter().map(|(_, code)| code.as_str()), &values)
    {
        let prompt = match &placeholder.default {
            Some(default) => format!("{} [{}]: ", placeholder.name, default),
            None => format!("{}: ", placeholder.name),
        };
        ctx.write_stdout(&prompt)?;
        input.clear();
        prompts.read_line(&mut input)?;
        if !input.ends_with('\n') {
            return Ok(0);
        }
        if !input.trim().is_empty() {
            values.insert(placeholder.name, input.trim().to_string());
        }
    }

    let runtime = proxy.command_runtime_snapshot()?;
    'blocks: for (i, (shell, template)) in blocks.iter().enumerate() {
        let number = i + 1;
        let code = runbook::substitute_placeholders(template, &values);
        ctx.write_stdout(&format!(
            "\n--- Block {number} (starts in {}) ---",
            runtime.current_dir().display()
        ))?;
        ctx.write_stdout(&code)?;
        loop {
            ctx.write_stdout("Execute? [Y/n/q] ")?;
            input.clear();
            prompts.read_line(&mut input)?;
            if !input.ends_with('\n') {
                return Ok(0);
            }
            match input.trim().to_ascii_lowercase().as_str() {
                "" | "y" | "yes" => break,
                "n" | "no" => continue 'blocks,
                "q" | "quit" => return Ok(0),
                _ => continue,
            }
        }
        let status = run_script(ctx, proxy, shell, &code)
            .with_context(|| format!("block {number}: could not run {shell}"))?;
        if !status.success() {
            let code = status
                .code()
                .unwrap_or_else(|| status.signal().map_or(1, |sig| 128 + sig));
            ctx.write_stderr(&format!(
                "notebook-play: block {number} exited with status {code}"
            ))?;
            return Ok(code);
        }
    }
    Ok(0)
}

fn run_script(
    ctx: &Context,
    proxy: &mut dyn ShellProxy,
    shell: &str,
    code: &str,
) -> Result<std::process::ExitStatus> {
    let mut command = crate::runtime_spawn::runtime_command(proxy, shell)?;
    Ok(command
        .args(["-c", "--", code])
        .stdin(duplicate_fd(ctx.infile)?)
        .stdout(duplicate_fd(ctx.outfile)?)
        .stderr(duplicate_fd(ctx.errfile)?)
        .status()?)
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
        let blocks = [
            "ssh {{host:localhost}} uptime",
            "echo {{message}}",
            // Same name again: answering it once (even by accepting the
            // default) must not produce a second prompt.
            "scp file {{host:localhost}}:/tmp",
        ];
        let pending = placeholders_to_prompt(blocks, &HashMap::new());
        assert_eq!(
            pending.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["host", "message"]
        );
        assert_eq!(pending[0].default.as_deref(), Some("localhost"));

        // --var answers a placeholder without prompting.
        let supplied = HashMap::from([("host".to_string(), "example.com".to_string())]);
        let pending = placeholders_to_prompt(blocks, &supplied);
        assert_eq!(
            pending.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["message"]
        );
    }

    #[test]
    fn go_template_blocks_prompt_for_nothing() {
        let blocks = ["docker ps --format '{{json .}}'"];
        assert!(placeholders_to_prompt(blocks, &HashMap::new()).is_empty());
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

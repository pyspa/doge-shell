use super::ShellProxy;
use anyhow::Result;
use dsh_types::notebook::{BlockKind, Notebook};
use dsh_types::{Context, ExitStatus};
use std::io::{self, Write};

pub fn description() -> &'static str {
    "Play a notebook file (execute code blocks interactively)"
}

pub fn command(ctx: &Context, argv: Vec<String>, proxy: &mut dyn ShellProxy) -> ExitStatus {
    if argv.is_empty() {
        let _ = ctx.write_stderr("Usage: notebook-play <file>\n");
        return ExitStatus::ExitedWith(1);
    }

    let file_path = &argv[0];
    match run_play(ctx, file_path, proxy) {
        Ok(_) => ExitStatus::ExitedWith(0),
        Err(e) => {
            let _ = ctx.write_stderr(&format!("notebook-play: {}\n", e));
            ExitStatus::ExitedWith(1)
        }
    }
}

fn run_play(ctx: &Context, path: &str, proxy: &mut dyn ShellProxy) -> Result<()> {
    let path_buf = std::path::PathBuf::from(path);
    if !path_buf.exists() {
        anyhow::bail!("File not found: {}", path);
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

    let _ = io::stdin();
    let _ = io::stdout();

    for (i, block) in blocks.iter().enumerate() {
        let code = block.raw_content();
        let _ = ctx.write_stdout(&format!("\n--- Block {} ---\n", i + 1));
        let _ = ctx.write_stdout(&format!("{}\n", code.trim()));
        let _ = ctx.write_stdout("Execute? [Y/n/q] ");
        let _ = io::stdout().flush();

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
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

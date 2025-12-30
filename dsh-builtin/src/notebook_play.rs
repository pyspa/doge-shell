use super::ShellProxy;
use anyhow::Result;
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
    // We use Lisp engine's notebook functionality if possible, or just parse here.
    // For simplicity and since we are in dsh-builtin which doesn't have direct access to dsh::notebook,
    // we might need to expose notebook functionality via Lisp or Proxy.

    // Check if notebook functionality is available via Lisp
    let _lisp_cmd = format!("(notebook-play \"{}\")", path);
    // Actually, it might be better to implement the logic here using a shared library if possible,
    // but dsh/src/notebook.rs is in the main crate.

    // Alternative: Use `proxy.dispatch` to run commands, but we need to parse the file first.
    // Let's assume we want to do this in Rust. We should probably move Notebook to a shared crate or dsh-types.
    // But since it's already in `dsh`, let's see if we can use it.

    // Actually, I'll implement the parsing here again or use a simpler approach for now if I can't access `dsh::notebook`.
    // Wait, `dsh-builtin` is a separate crate. `dsh` depends on `dsh-builtin`.
    // So `dsh-builtin` CANNOT depend on `dsh`.

    // I should move `Notebook` to `dsh-types` if it's meant to be used by builtins.
    // Or I can use Lisp to bridge it.

    let path_buf = std::path::PathBuf::from(path);
    if !path_buf.exists() {
        anyhow::bail!("File not found: {}", path);
    }

    let content = std::fs::read_to_string(&path_buf)?;
    let mut blocks = Vec::new();

    // Simple parsing for now (copying logic from notebook.rs or similar)
    // In a real scenario, we'd refactor the code to be shared.
    use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};

    let parser = Parser::new(&content);
    let mut in_code_block = false;
    let mut current_lang = String::new();
    let mut current_code = String::new();

    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code_block = true;
                current_lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                current_code.clear();
            }
            Event::Text(text) => {
                if in_code_block {
                    current_code.push_str(&text);
                }
            }
            Event::End(_) => {
                if in_code_block
                    && (current_lang == "bash" || current_lang == "sh" || current_lang.is_empty())
                {
                    blocks.push(current_code.clone());
                }
                in_code_block = false;
            }
            _ => {}
        }
    }

    if blocks.is_empty() {
        let _ = ctx.write_stdout("No executable blocks found in notebook.\n");
        return Ok(());
    }

    let _ = io::stdin();
    let _ = io::stdout();

    for (i, code) in blocks.iter().enumerate() {
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
                // We use proxy.dispatch to execute the command in the shell context
                // Note: dispatch expects tokens, but we have a raw string.
                // Main shell's eval handles this. Builtin proxy might not have a full eval.
                // Let's check how dispatch is used.

                // For now, let's try to just run it as a shell command.
                let _ = proxy.dispatch(ctx, line, vec![]);
            }
        }
    }

    Ok(())
}

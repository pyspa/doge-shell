use crate::command_palette::CommandPalette;
use crate::completion::display::Candidate;
use crate::repl::Repl;
use crate::repl::state::ReplControlFlow;
use crate::terminal::renderer::TerminalRenderer;
use anyhow::Result;
use crossterm::cursor;
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use skim::prelude::*;
use std::io::Write;
use std::sync::Arc;

/// Handle opening the command palette.
pub(crate) async fn handle_open_command_palette(repl: &mut Repl<'_>) -> Result<ReplControlFlow> {
    // Disable raw mode so Skim can handle terminal state correctly
    disable_raw_mode().ok();

    CommandPalette::run(repl.shell, repl.input.as_str()).await?;

    // Re-enable raw mode for the shell
    enable_raw_mode().ok();

    let mut renderer = TerminalRenderer::new();
    repl.print_prompt(&mut renderer);
    renderer.flush().ok();
    Ok(ReplControlFlow::Continue)
}

/// Handle clearing the screen.
pub(crate) fn handle_clear_screen(repl: &mut Repl<'_>) -> Result<ReplControlFlow> {
    let mut renderer = TerminalRenderer::new();
    queue!(renderer, Clear(ClearType::All), cursor::MoveTo(0, 0)).ok();
    repl.print_prompt(&mut renderer);
    renderer.flush().ok();
    repl.input.clear();
    repl.suggestion_manager.clear();
    Ok(ReplControlFlow::Continue)
}

pub(crate) async fn check_background_jobs(repl: &mut Repl<'_>, output: bool) -> Result<()> {
    let jobs = repl.shell.check_job_state().await?;
    let exists = !jobs.is_empty();

    if output && exists {
        // Process background output for completed jobs
        for mut job in jobs {
            if !job.foreground {
                job.check_background_all_output().await?;
            }
        }

        // Batch all output operations with a single terminal renderer
        let mut renderer = TerminalRenderer::new();
        let mut output_buffer = String::new();

        // Check remaining jobs in wait_jobs for status messages
        for job in &repl.shell.wait_jobs {
            if !job.foreground && output {
                output_buffer.push_str(&format!(
                    "\rdsh: job {} '{}' {}\n",
                    job.job_id, job.cmd, job.state
                ));
            }
        }

        if !output_buffer.is_empty() {
            renderer.write_all(output_buffer.as_bytes())?;
            repl.print_prompt(&mut renderer);
            renderer.flush()?;
        }
    }
    Ok(())
}

pub(crate) async fn handle_macro_record(repl: &mut Repl<'_>) -> Result<()> {
    let history_items = if let Some(history_arc) = &repl.shell.cmd_history {
        let history = history_arc.lock();
        history.get_recent_context(100)
    } else {
        return Ok(());
    };

    if history_items.is_empty() {
        return Ok(());
    }

    // Disable raw mode for Skim
    let _ = disable_raw_mode();

    // Run Skim in a blocking task to avoid runtime conflict
    let commands = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<String>> {
        let options = SkimOptionsBuilder::default()
            .multi(true)
            .bind(vec!["Enter:accept".to_string()])
            .prompt("Select commands for macro > ".to_string())
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build skim options: {}", e))?;

        let (tx_item, rx_item): (SkimItemSender, SkimItemReceiver) = unbounded();
        for item in history_items {
            let _ = tx_item.send(vec![Arc::new(Candidate::Basic(item))]);
        }
        drop(tx_item);

        let selected_items = Skim::run_with(options, Some(rx_item))
            .map(|out| out.selected_items)
            .unwrap_or_default();

        // Convert selected items back to strings inside the blocking task
        Ok(selected_items
            .iter()
            .map(|item| item.output().to_string())
            .collect())
    })
    .await??;

    // Re-enable raw mode
    let _ = enable_raw_mode();

    if commands.is_empty() {
        return Ok(());
    }

    // Prompt for macro name
    let mut renderer = TerminalRenderer::new();
    queue!(renderer, Print("\r\nMacro name: ")).ok();
    renderer.flush().ok();

    let _ = disable_raw_mode();
    let mut name = String::new();
    std::io::stdin().read_line(&mut name)?;
    let _ = enable_raw_mode();

    let name = name.trim();
    if name.is_empty() {
        queue!(renderer, Print("\r\nMacro creation cancelled.\r\n")).ok();
        repl.print_prompt(&mut renderer);
        renderer.flush().ok();
        return Ok(());
    }

    // Generate Lisp code
    let lisp_code = crate::repl::macro_utils::generate_macro_lisp(name, &commands);

    // Save to config.lisp
    let config_path = crate::environment::get_config_file(crate::lisp::CONFIG_FILE)?;

    // Append
    use std::fs::OpenOptions;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config_path)?;

    writeln!(file, "{}", lisp_code)?;

    // Evaluate
    match repl.shell.lisp_engine.borrow().run(&lisp_code) {
        Ok(_) => {
            queue!(
                renderer,
                Print(format!("\r\nMacro '{}' saved and loaded.\r\n", name))
            )
            .ok();
        }
        Err(e) => {
            queue!(
                renderer,
                Print(format!("\r\nMacro saved but failed to load: {}\r\n", e))
            )
            .ok();
        }
    }

    repl.print_prompt(&mut renderer);
    renderer.flush().ok();

    Ok(())
}

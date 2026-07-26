use crate::command_palette::CommandPalette;
use crate::completion::display::Candidate;
use crate::process::job::Job;
use crate::repl::Repl;
use crate::repl::job_notify::{JobMarker, JobNotice, format_job_notice, notice_state_from};
use crate::repl::notify::notify_command_finished;
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

    if let Some(replacement) = CommandPalette::run(repl.shell, repl.input.as_str()).await? {
        repl.input.reset(replacement);
        repl.input.completion = None;
        repl.input.color_ranges = None;
    }

    // Re-enable raw mode for the shell
    enable_raw_mode().ok();

    let mut renderer = TerminalRenderer::new();
    repl.print_prompt(&mut renderer);
    // Actions that hand a command back (Search History, Block Browser) put it in
    // the buffer; without this it stays invisible until the next keystroke and
    // the palette looks like it did nothing.
    repl.print_input(&mut renderer, true, false);
    renderer.flush().ok();
    Ok(ReplControlFlow::Continue)
}

/// Open the command block browser (Ctrl-O).
///
/// `CommandBlock` is plain owned data, so a snapshot crosses the `Send`
/// boundary into the `RunInteractive` closure without dragging `Environment`
/// (which is `!Send`) along.
pub(crate) fn handle_open_block_browser(repl: &mut Repl<'_>) -> Result<ReplControlFlow> {
    let blocks = repl
        .shell
        .environment
        .read()
        .command_blocks
        .get_all_blocks();
    if blocks.is_empty() {
        let mut renderer = TerminalRenderer::new();
        renderer.write_all(b"\r\ndsh: no command blocks recorded yet\r\n")?;
        repl.print_prompt(&mut renderer);
        repl.print_input(&mut renderer, false, false);
        renderer.flush()?;
        return Ok(ReplControlFlow::Continue);
    }

    Ok(ReplControlFlow::RunInteractive(Box::new(move || {
        use crate::blocks_ui::{BlockBrowser, BrowserOutcome, run};
        use crate::repl::state::InteractiveAction;

        Ok(match run(BlockBrowser::new(blocks))? {
            BrowserOutcome::Insert(text) => Some(InteractiveAction::ReplaceAll { text }),
            BrowserOutcome::Run(text) => Some(InteractiveAction::ReplaceAllAndExecute { text }),
            BrowserOutcome::Quit => None,
        })
    })))
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

/// Reap finished jobs and report the ones that completed in the background.
///
/// `check_job_state` returns the jobs that just *completed* and removes them
/// from `wait_jobs`; reporting must therefore iterate the returned set, not the
/// surviving one.
pub(crate) async fn check_background_jobs(repl: &mut Repl<'_>, output: bool) -> Result<()> {
    let completed = repl.shell.check_job_state().await?;
    if completed.is_empty() {
        return Ok(());
    }

    // Keep completion's job list in sync now that jobs were removed.
    repl.sync_completion_jobs();

    if !output {
        return Ok(());
    }

    let notices: Vec<String> = completed
        .iter()
        .filter(|job| !job.foreground)
        .enumerate()
        .map(|(index, job)| {
            format_job_notice(&JobNotice {
                job_id: job.job_id,
                cmd: job.cmd.clone(),
                state: notice_state_from(&job.state),
                marker: JobMarker::for_index(index),
            })
        })
        .collect();

    if notices.is_empty() {
        return Ok(());
    }

    notify_desktop_for_jobs(repl, &completed);

    let mut renderer = TerminalRenderer::new();
    crate::repl::render::print_above_prompt(repl, &mut renderer, &notices);
    renderer.flush()?;
    Ok(())
}

fn notify_desktop_for_jobs(repl: &Repl<'_>, completed: &[Job]) {
    let prefs = repl.shell.environment.read().input_preferences;
    if !prefs.auto_notify_enabled {
        return;
    }
    for job in completed.iter().filter(|job| !job.foreground) {
        let exit_code = notice_state_from(&job.state).exit_code();
        notify_command_finished(&prefs, &job.cmd, job.started_at.elapsed(), exit_code);
    }
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

        let selected_items = crate::utils::skim::run_skim_with(options, Some(rx_item))
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

//! Interactive adapter; prompt text never goes through shell parsing or eval.
use crate::{ai_task_input::TaskInput, shell::Shell};
use anyhow::{Result, bail};
use dsh_types::Context;

pub(crate) fn execute(
    shell: &mut Shell,
    ctx: &Context,
    input: Result<TaskInput<'_>, &'static str>,
) -> Result<()> {
    // Check the real context descriptors, including redirected/captured output.
    if !ctx.interactive || ctx.infile != libc::STDIN_FILENO || ctx.captured_out.is_some()
        // SAFETY: isatty only inspects descriptors and handles invalid ones.
        || unsafe { libc::isatty(ctx.infile) } != 1
        || unsafe { libc::isatty(ctx.outfile) } != 1
    {
        bail!("!& requires an interactive input/output TTY");
    }
    match input.map_err(anyhow::Error::msg)? {
        TaskInput::Start { edit, prompt } => super::command(
            shell,
            ctx,
            vec![
                "chat_task".into(),
                "start".into(),
                "--profile".into(),
                if edit { "workspace-edit" } else { "read-only" }.into(),
                "--".into(),
                prompt.into(),
            ],
        ),
        TaskInput::Menu => menu(shell, ctx),
    }
}

// The REPL has paused its EventStream before entering here (Enter and
// Alt+Enter alike). This is the sole reader until the menu returns.
fn read_key() -> Result<char> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    loop {
        if let Event::Key(key) = read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
                return Ok('q');
            }
            if !key.modifiers.is_empty() {
                continue;
            }
            match key.code {
                KeyCode::Enter | KeyCode::Esc => return Ok('q'),
                KeyCode::Char(ch) => return Ok(ch),
                _ => {}
            }
        }
    }
}

fn line(ctx: &Context, text: &str) -> Result<()> {
    ctx.write_stdout(&(text.replace('\n', "\r\n") + "\r"))
}

fn menu(shell: &mut Shell, ctx: &Context) -> Result<()> {
    let Some(task) = shell.background_task.as_mut() else {
        return line(ctx, "no background task");
    };
    let _restore = crate::repl::terminal_state::RawModeRestore::new();
    crossterm::terminal::enable_raw_mode()?;
    loop {
        line(ctx, &serde_json::to_string_pretty(&task.status())?)?;
        line(
            ctx,
            "[o] output  [a] approval  [s] stop  [q] close (task keeps running)",
        )?;
        match read_key()? {
            'q' => return Ok(()),
            'o' => line(ctx, &task.output())?,
            'a' => {
                let Some(preview) = task.approval_preview() else {
                    line(ctx, "no pending approval")?;
                    continue;
                };
                // The displayed operation and opaque answer token were captured
                // atomically. A subsequent change expires this response.
                line(ctx, &serde_json::to_string_pretty(&preview.display)?)?;
                line(
                    ctx,
                    if preview.complete {
                        "[y] approve once  [n] deny  [q] close approval"
                    } else {
                        "incomplete preview: approval unavailable; [n] deny  [q] close approval"
                    },
                )?;
                let allow = match read_key()? {
                    'y' if preview.complete => true,
                    'n' => false,
                    _ => continue,
                };
                match task.respond_preview(&preview, allow) {
                    Ok(()) => line(ctx, "approval response recorded")?,
                    Err(error) => line(ctx, &error.to_string())?,
                }
            }
            's' => {
                line(ctx, "Stop this task? [y/N]")?;
                if read_key()? == 'y' {
                    if task.is_active() {
                        task.cancel();
                        line(ctx, "task cancelled")?;
                    } else {
                        line(ctx, "task already stopped")?;
                    }
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

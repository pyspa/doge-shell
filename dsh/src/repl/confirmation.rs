use crate::ai_features::ConfirmationHandler;
use anyhow::Result;
use async_trait::async_trait;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::queue;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use std::io::{Write, stdout};
use std::sync::Arc;

pub struct ReplConfirmationHandler;

impl ReplConfirmationHandler {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

#[async_trait]
impl ConfirmationHandler for ReplConfirmationHandler {
    async fn confirm(&self, message: &str) -> Result<bool> {
        // We need to use spawn_blocking because crossterm::event::read is blocking
        // and we want to avoid blocking the tokio runtime executor.
        // Also, we assume we have control over the terminal (Repl is awaiting us).

        let message = message.to_string();

        tokio::task::spawn_blocking(move || {
            let mut stdout = stdout();

            // Print message
            queue!(
                stdout,
                Print("\r\n"),
                SetForegroundColor(Color::Yellow),
                Print(&message),
                Print("\r\n"),
                Print("Proceed? [y/N]: "),
                ResetColor
            )?;
            stdout.flush()?;

            loop {
                // Read event
                if let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                queue!(stdout, Print("Yes\r\n"))?;
                                stdout.flush()?;
                                return Ok(true);
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                queue!(stdout, Print("No\r\n"))?;
                                stdout.flush()?;
                                return Ok(false);
                            }
                            KeyCode::Enter => {
                                // Default is No
                                queue!(stdout, Print("No\r\n"))?;
                                stdout.flush()?;
                                return Ok(false);
                            }
                            _ => {}
                        }
                    }
            }
        })
        .await?
    }
}

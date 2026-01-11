use crate::ai_features::ConfirmationHandler;
use anyhow::Result;
use async_trait::async_trait;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::queue;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};
use std::io::{Write, stdout};
use std::sync::Arc;

/// Action selected by the user in confirmation dialog
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationAction {
    Yes,
    No,
    AlwaysAllow,
}

/// Synchronous confirmation helper that handles raw mode switching
pub fn confirm_action(message: &str) -> Result<ConfirmationAction> {
    let mut stdout = stdout();

    // Check if raw mode is already enabled to restore state later
    // Note: crossterm's is_raw_mode_enabled isn't always reliable across processes but works for this process.
    let was_raw = is_raw_mode_enabled().unwrap_or(false);
    if !was_raw {
        enable_raw_mode()?;
    }

    // Print message
    queue!(
        stdout,
        Print("\r\n"),
        SetForegroundColor(Color::Yellow),
        Print("🛡️  SAFETY GUARD: "),
        Print(message),
        Print("\r\n"),
        Print("Proceed? [y/N/a(Always)]: "),
        ResetColor
    )?;
    stdout.flush()?;

    let mut action = ConfirmationAction::No;

    loop {
        // Read event
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    action = ConfirmationAction::Yes;
                    // Print 'Yes' to confirm selection visually
                    queue!(stdout, Print("Yes\r\n"))?;
                    break;
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    action = ConfirmationAction::AlwaysAllow;
                    // Print 'Always' to confirm selection visually
                    queue!(stdout, Print("Always\r\n"))?;
                    break;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    queue!(stdout, Print("No\r\n"))?;
                    break;
                }
                KeyCode::Enter => {
                    // Default is No
                    queue!(stdout, Print("No\r\n"))?;
                    break;
                }
                _ => {}
            }
        }
    }
    stdout.flush()?;

    // Restore raw mode state
    if !was_raw {
        disable_raw_mode()?;
    }

    Ok(action)
}

pub struct ReplConfirmationHandler;

impl ReplConfirmationHandler {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

#[async_trait]
impl ConfirmationHandler for ReplConfirmationHandler {
    async fn confirm(&self, message: &str) -> Result<ConfirmationAction> {
        // We need to use spawn_blocking because crossterm::event::read is blocking
        let message = message.to_string();

        let result = tokio::task::spawn_blocking(move || confirm_action(&message)).await??;

        Ok(result)
    }
}

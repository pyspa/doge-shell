use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use std::io::{self, stdout};

pub struct DiagnosticContext {
    pub command: String,
    pub output: String,
    pub exit_code: i32,
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        let _ = execute!(io::stdout(), crossterm::cursor::Show);
    }
}

pub struct AiChatUi {
    context: DiagnosticContext,
    diagnosis_text: String,
}

impl AiChatUi {
    pub fn new(context: DiagnosticContext, diagnosis_text: String) -> Self {
        Self {
            context,
            diagnosis_text,
        }
    }

    pub fn set_diagnosis_text(&mut self, text: String) {
        self.diagnosis_text = text;
    }

    pub fn run(&mut self) -> Result<()> {
        let mut stdout = stdout();
        enable_raw_mode()?;
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

        let _guard = TerminalGuard; // Ensures cleanup on ? or panic
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let mut should_quit = false;

        while !should_quit {
            terminal.draw(|f| {
                let chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                    .split(f.area());

                let context_text = format!(
                    "Command: {}\nExit Code: {}\n\n{}",
                    self.context.command, self.context.exit_code, self.context.output
                );
                let context_widget = Paragraph::new(context_text)
                    .block(Block::default().title(" Context ").borders(Borders::ALL))
                    .style(Style::default().fg(Color::DarkGray))
                    .wrap(Wrap { trim: false });

                f.render_widget(context_widget, chunks[0]);

                let mut display_text = self.diagnosis_text.clone();
                if display_text.is_empty() {
                    display_text = "Analyzing...".to_string();
                }

                let chat_widget = Paragraph::new(display_text)
                    .block(
                        Block::default()
                            .title(" AI Diagnosis ")
                            .borders(Borders::ALL),
                    )
                    .wrap(Wrap { trim: false });

                f.render_widget(chat_widget, chunks[1]);
            })?;

            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => should_quit = true,
                    _ => {}
                }
            }
        }

        // Drop guard restores the terminal
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ui_initialization() {
        let ctx = DiagnosticContext {
            command: "ls -al".to_string(),
            output: "No such file or directory".to_string(),
            exit_code: 1,
        };
        let ui = AiChatUi::new(ctx, "Error detected.".to_string());
        assert_eq!(ui.context.command, "ls -al");
        assert_eq!(ui.context.exit_code, 1);
        assert_eq!(ui.diagnosis_text, "Error detected.");
    }

    #[test]
    fn test_ui_set_text() {
        let ctx = DiagnosticContext {
            command: "git".to_string(),
            output: "".to_string(),
            exit_code: 0,
        };
        let mut ui = AiChatUi::new(ctx, "".to_string());
        ui.set_diagnosis_text("Checking branch...".to_string());
        assert_eq!(ui.diagnosis_text, "Checking branch...");
    }
}

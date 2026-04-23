use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
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

#[derive(Debug, Clone)]
pub struct DiagnosticContext {
    pub command: String,
    pub output: String,
    pub exit_code: i32,
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = execute!(io::stdout(), crossterm::cursor::Show);
    }
}

pub enum UiOutcome {
    ApplyCommand(String),
    Ask(String),
    Quit,
}

pub struct AiChatUi {
    context: DiagnosticContext,
    diagnosis_text: String,
    input_buffer: String,
}

impl AiChatUi {
    pub fn new(context: DiagnosticContext, diagnosis_text: String) -> Self {
        Self {
            context,
            diagnosis_text,
            input_buffer: String::new(),
        }
    }

    pub fn set_diagnosis_text(&mut self, text: String) {
        self.diagnosis_text = text;
    }

    pub fn run(&mut self) -> Result<UiOutcome> {
        let mut stdout = stdout();
        enable_raw_mode()?;
        execute!(stdout, EnterAlternateScreen)?;

        let _guard = TerminalGuard; // Ensures cleanup on ? or panic
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let mut outcome = UiOutcome::Quit;
        let mut should_quit = false;

        while !should_quit {
            terminal.draw(|f| {
                let main_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Percentage(80), Constraint::Percentage(20)].as_ref())
                    .split(f.area());

                let top_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(40), Constraint::Percentage(60)].as_ref())
                    .split(main_chunks[0]);

                let context_text = format!(
                    "Command: {}\nExit Code: {}\n\n{}",
                    self.context.command, self.context.exit_code, self.context.output
                );
                let context_widget = Paragraph::new(context_text)
                    .block(Block::default().title(" Context ").borders(Borders::ALL))
                    .style(Style::default().fg(Color::DarkGray))
                    .wrap(Wrap { trim: false });

                f.render_widget(context_widget, top_chunks[0]);

                let mut display_text = self.diagnosis_text.clone();
                if display_text.is_empty() {
                    display_text = "Analyzing...".to_string();
                }

                let chat_widget = Paragraph::new(display_text)
                    .block(
                        Block::default()
                            .title(" AI Diagnosis (Ctrl+Y/Alt+A: Apply command) ")
                            .borders(Borders::ALL),
                    )
                    .wrap(Wrap { trim: false });

                f.render_widget(chat_widget, top_chunks[1]);

                // Prompt area
                let input_widget = Paragraph::new(self.input_buffer.clone())
                    .style(Style::default().fg(Color::Yellow))
                    .block(
                        Block::default()
                            .title(" Ask AI (Enter to send, Esc/Ctrl+C to quit) ")
                            .borders(Borders::ALL),
                    );

                f.render_widget(input_widget, main_chunks[1]);
            })?;

            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                match key.code {
                    KeyCode::Enter if !self.input_buffer.trim().is_empty() => {
                        outcome = UiOutcome::Ask(self.input_buffer.clone());
                        should_quit = true;
                    }
                    KeyCode::Backspace => {
                        self.input_buffer.pop();
                    }
                    KeyCode::Char('y')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        if let Some(cmd) = Self::extract_command(&self.diagnosis_text) {
                            outcome = UiOutcome::ApplyCommand(cmd);
                            should_quit = true;
                        }
                    }
                    KeyCode::Char('a')
                        if key.modifiers.contains(crossterm::event::KeyModifiers::ALT) =>
                    {
                        if let Some(cmd) = Self::extract_command(&self.diagnosis_text) {
                            outcome = UiOutcome::ApplyCommand(cmd);
                            should_quit = true;
                        }
                    }
                    KeyCode::Char('c')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        should_quit = true;
                    }
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL)
                            && !key.modifiers.contains(crossterm::event::KeyModifiers::ALT) =>
                    {
                        self.input_buffer.push(c);
                    }
                    KeyCode::Esc => should_quit = true,
                    _ => {}
                }
            }
        }

        // Drop guard restores the terminal
        Ok(outcome)
    }

    /// Extract the first markdown bash code block from the given text
    pub fn extract_command(text: &str) -> Option<String> {
        let mut in_block = false;
        let mut command = String::new();

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("```bash") || trimmed.starts_with("```sh") || trimmed == "```" {
                if in_block {
                    // end of block
                    break;
                } else {
                    in_block = true;
                    continue;
                }
            }
            if in_block {
                if !command.is_empty() {
                    command.push('\n');
                }
                command.push_str(line);
            }
        }

        if command.is_empty() {
            None
        } else {
            Some(command.trim().to_string())
        }
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

    #[test]
    fn test_extract_command() {
        let text1 = "Try running this:\n```bash\nls -al\n```\nGood luck!";
        assert_eq!(AiChatUi::extract_command(text1).unwrap(), "ls -al");

        let text2 = "No code blocks here, just text";
        assert!(AiChatUi::extract_command(text2).is_none());

        let text3 = "Empty block:\n```bash\n```\nEnd.";
        assert!(AiChatUi::extract_command(text3).is_none());

        let text4 = "```sh\necho 'hello'\n```";
        assert_eq!(AiChatUi::extract_command(text4).unwrap(), "echo 'hello'");
    }
}

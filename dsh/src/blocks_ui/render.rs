//! Terminal driver and drawing for the block browser.
//!
//! Everything stateful lives in [`super::model`]; this file only turns that
//! state into cells and feeds it key events.

use super::model::{BlockBrowser, BrowserAction, BrowserOutcome, Focus};
use anyhow::Result;
use dsh_types::command_block::CommandBlock;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

/// Below this the bordered two-pane layout collapses into nothing.
const MIN_COLS: u16 = 40;
const MIN_ROWS: u16 = 10;

/// Terminal width at which the list moves above the output instead of beside it.
const SIDE_BY_SIDE_COLS: u16 = 100;

/// Run the browser and return what the REPL should do with the input buffer.
pub fn run(browser: BlockBrowser) -> Result<BrowserOutcome> {
    use crossterm::event::{self, Event};
    use crossterm::execute;
    use crossterm::terminal::{
        ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use std::io::{self, IsTerminal};

    // Bail out before touching raw mode when there is nothing to draw on.
    if !io::stdout().is_terminal() {
        return Ok(BrowserOutcome::Quit);
    }
    let Ok((cols, rows)) = crossterm::terminal::size() else {
        return Ok(BrowserOutcome::Quit);
    };
    if cols < MIN_COLS || rows < MIN_ROWS {
        // Rendering borders into a few columns produces garbage; the caller
        // falls back to `blocks list`.
        anyhow::bail!(
            "terminal too small for the block browser ({}x{}, need {}x{})",
            cols,
            rows,
            MIN_COLS,
            MIN_ROWS
        );
    }

    struct TerminalGuard;
    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            let _ = execute!(io::stdout(), crossterm::cursor::Show);
        }
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // ratatui diffs against an all-blank buffer, so its first draw paints only
    // non-blank cells; clear so nothing shows through on terminals that hand
    // out a dirty alternate screen. Not `Terminal::clear`, which snapshots the
    // cursor with a DSR round-trip that can hang.
    execute!(
        stdout,
        EnterAlternateScreen,
        crossterm::terminal::Clear(ClearType::All)
    )?;
    // Restores the terminal even if drawing panics.
    let _guard = TerminalGuard;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut browser = browser;

    loop {
        terminal.draw(|frame| draw(frame, &mut browser))?;

        match event::read()? {
            Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                match browser.on_key(key) {
                    BrowserAction::Finish(outcome) => return Ok(outcome),
                    BrowserAction::Copy(text) => {
                        let message = match copy_to_clipboard(&text) {
                            Ok(()) => format!("copied {} bytes", text.len()),
                            Err(err) => format!("copy failed: {}", err),
                        };
                        browser.set_status(message);
                    }
                    BrowserAction::Redraw | BrowserAction::Noop => {}
                }
            }
            Event::Resize(_, _) => browser.clamp_scroll(),
            _ => {}
        }
    }
}

/// Put text on the system clipboard, falling back to OSC 52.
///
/// `arboard` needs a display server, so it fails over SSH and on headless
/// machines; OSC 52 asks the terminal emulator to do it instead, which works
/// through an SSH session as long as the emulator allows it.
fn copy_to_clipboard(text: &str) -> Result<()> {
    if let Ok(mut clipboard) = arboard::Clipboard::new()
        && clipboard.set_text(text.to_string()).is_ok()
    {
        return Ok(());
    }

    use std::io::Write;
    let mut stdout = std::io::stdout();
    write!(stdout, "\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))?;
    stdout.flush()?;
    Ok(())
}

/// Standard base64, for the OSC 52 payload.
///
/// Hand-rolled rather than pulling in a crate for the one place the shell needs
/// it.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

fn draw(frame: &mut Frame, browser: &mut BlockBrowser) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    draw_query_line(frame, chunks[0], browser);

    if browser.is_empty() {
        frame.render_widget(
            Paragraph::new("No command blocks recorded in this session yet.")
                .style(Style::default().fg(Color::DarkGray)),
            chunks[1],
        );
    } else {
        let side_by_side = area.width >= SIDE_BY_SIDE_COLS;
        let panes = Layout::default()
            .direction(if side_by_side {
                Direction::Horizontal
            } else {
                Direction::Vertical
            })
            .constraints(if side_by_side {
                [Constraint::Percentage(35), Constraint::Percentage(65)]
            } else {
                [Constraint::Percentage(40), Constraint::Percentage(60)]
            })
            .split(chunks[1]);

        draw_list(frame, panes[0], browser);
        // Paging needs to know how many rows the pane can show.
        browser.set_output_height(panes[1].height.saturating_sub(2) as usize);
        draw_output(frame, panes[1], browser);
    }

    draw_status(frame, chunks[2], browser);

    if browser.show_help() {
        draw_help(frame, area);
    }
}

fn draw_query_line(frame: &mut Frame, area: Rect, browser: &BlockBrowser) {
    let line = if browser.filter_input() {
        Line::from(vec![
            Span::styled("filter> ", Style::default().fg(Color::Cyan)),
            Span::raw(browser.filter().to_string()),
        ])
    } else if !browser.filter().is_empty() {
        Line::from(vec![
            Span::styled("filter: ", Style::default().fg(Color::DarkGray)),
            Span::raw(browser.filter().to_string()),
        ])
    } else {
        Line::from(Span::styled(
            "command blocks — / filter   ? help",
            Style::default().fg(Color::DarkGray),
        ))
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_list(frame: &mut Frame, area: Rect, browser: &BlockBrowser) {
    let focused = browser.focus() == Focus::List;
    let items: Vec<ListItem> = browser
        .blocks()
        .into_iter()
        .enumerate()
        .map(|(pos, block)| ListItem::new(list_row(block, browser.is_marked(pos), area.width)))
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style(focused))
                .title(format!(
                    " blocks {}/{} ",
                    browser.matched(),
                    browser.total()
                )),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut state = ListState::default();
    if browser.matched() > 0 {
        state.select(Some(browser.selected()));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

fn list_row(block: &CommandBlock, marked: bool, width: u16) -> Line<'static> {
    let (glyph, color) = if block.exit_code == 0 {
        ("✔", Color::Green)
    } else {
        ("✘", Color::Red)
    };

    let mut spans = vec![
        Span::styled(
            if marked { "* " } else { "  " },
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(format!("{} ", glyph), Style::default().fg(color)),
        Span::styled(
            format!("{:>7} ", format_duration(block.duration_ms)),
            Style::default().fg(Color::Yellow),
        ),
    ];
    if block.watched {
        spans.push(Span::styled("👁 ", Style::default().fg(Color::Magenta)));
    }
    spans.push(Span::raw(flatten(&block.command)));

    // Whatever room is left goes to a preview of the output.
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let room = (width as usize).saturating_sub(used + 6);
    if room > 12 {
        let preview = block.output_preview(room);
        if !preview.is_empty() {
            spans.push(Span::styled(
                format!("  — {}", preview),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    Line::from(spans)
}

fn draw_output(frame: &mut Frame, area: Rect, browser: &mut BlockBrowser) {
    let focused = browser.focus() == Focus::Output;
    let wrap = browser.wrap();
    let scroll = browser.output_scroll();
    let stream = browser.stream();
    let folded = browser.is_folded();
    let truncated = browser.is_truncated();
    let empty_note = browser.empty_output_note();
    let (lines, hidden) = browser.output_lines();

    let mut title = format!(" output ({}) ", stream.label());
    if truncated {
        // `append_bounded` keeps the tail, so a truncated block shows the end.
        title.push_str("[tail only — 1 MiB cap] ");
    }

    let mut text: Vec<Line> = Vec::new();
    if let Some(note) = empty_note {
        text.push(Line::from(Span::styled(
            note,
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Render only the visible window: a 1 MiB block is ~20k lines and
        // building a Paragraph from all of it every frame is not viable.
        let height = area.height.saturating_sub(2) as usize;
        let visible = lines.iter().skip(scroll).take(height.max(1));
        text.extend(visible.map(|line| Line::from(line.clone())));

        if folded && hidden > 0 {
            text.push(Line::from(Span::styled(
                format!("  … {} more lines (Space to unfold)", hidden),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let mut paragraph = Paragraph::new(text).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style(focused))
            .title(title),
    );
    if wrap {
        paragraph = paragraph.wrap(Wrap { trim: false });
    }
    frame.render_widget(paragraph, area);
}

fn draw_status(frame: &mut Frame, area: Rect, browser: &BlockBrowser) {
    let text = match browser.status() {
        Some(status) => status.to_string(),
        None => {
            let mut text = "Enter insert  r rerun  d cd  e explain  m mark  x export  c/y copy  Space fold  q quit".to_string();
            if browser.marked_count() > 0 {
                text = format!("[{} marked] {}", browser.marked_count(), text);
            }
            text
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let help = [
        "j / k / ↑ / ↓   move (list) or scroll (output)",
        "g / G           top / bottom",
        "Tab             switch pane",
        "Ctrl-d / Ctrl-u page output",
        "Space           fold / unfold output",
        "s               cycle stdout / stderr / both",
        "W               toggle wrapping",
        "/               filter by command or output",
        "f               failed blocks only",
        "w               AI-watched blocks only",
        "c / y           copy command / output",
        "Enter           insert the command",
        "r               re-run the command",
        "d               cd to where it ran",
        "e               explain it with AI",
        "m               mark for export",
        "x               export marked (or selected) as a runbook",
        "q / Esc         close",
    ];

    let width = 60.min(area.width.saturating_sub(4));
    let height = (help.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(help.iter().map(|l| Line::from(*l)).collect::<Vec<_>>()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" keys (any key closes) "),
        ),
        popup,
    );
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn format_duration(ms: u64) -> String {
    match ms {
        ms if ms < 1000 => format!("{}ms", ms),
        ms if ms < 60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        ms => {
            let secs = ms / 1000;
            format!("{}m{}s", secs / 60, secs % 60)
        }
    }
}

fn flatten(command: &str) -> String {
    command.replace("\r\n", "⏎").replace(['\n', '\r'], "⏎")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_picks_a_readable_unit() {
        assert_eq!(format_duration(340), "340ms");
        assert_eq!(format_duration(1200), "1.2s");
        assert_eq!(format_duration(90_000), "1m30s");
    }

    #[test]
    fn flatten_keeps_a_multiline_command_on_one_row() {
        assert_eq!(flatten("echo a\necho b"), "echo a⏎echo b");
        assert_eq!(flatten("echo a\r\necho b"), "echo a⏎echo b");
    }

    #[test]
    fn base64_encode_matches_the_standard_alphabet_and_padding() {
        // RFC 4648 test vectors: padding is the easy thing to get wrong.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encode_handles_high_bytes_and_utf8() {
        assert_eq!(base64_encode(&[0xff, 0xfe, 0xfd]), "//79");
        assert_eq!(base64_encode("日本語".as_bytes()), "5pel5pys6Kqe");
    }
}

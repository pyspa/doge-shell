use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use dsh_types::{Context, ExitStatus};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
};
use std::{
    io,
    time::{Duration, Instant},
};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

use crate::ShellProxy;

pub fn description() -> &'static str {
    "Interactive process viewer and manager"
}

struct App {
    system: System,
    processes: Vec<(sysinfo::Pid, String, f32, u64, String)>, // pid, name, cpu, mem, status
    state: TableState,
    filter: String,
    input_mode: bool,
    should_quit: bool,
    last_refresh: Instant,
}

impl App {
    fn new() -> Self {
        let mut app = Self {
            system: System::new_with_specifics(
                RefreshKind::nothing().with_processes(ProcessRefreshKind::everything()),
            ),
            processes: Vec::new(),
            state: TableState::default(),
            filter: String::new(),
            input_mode: false,
            should_quit: false,
            last_refresh: Instant::now(),
        };
        app.refresh_processes();
        if !app.processes.is_empty() {
            app.state.select(Some(0));
        }
        app
    }

    fn refresh_processes(&mut self) {
        self.system.refresh_processes(ProcessesToUpdate::All, true);
        let mut procs: Vec<_> = self
            .system
            .processes()
            .iter()
            .map(|(pid, proc)| {
                (
                    *pid,
                    proc.name().to_string_lossy().into_owned(),
                    proc.cpu_usage(),
                    proc.memory(),
                    format!("{:?}", proc.status()),
                )
            })
            .collect();

        // Sort by CPU usage descending by default
        procs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        // Filter
        if !self.filter.is_empty() {
            let filter = self.filter.to_lowercase();
            procs.retain(|(_, name, _, _, _)| name.to_lowercase().contains(&filter));
        }

        self.processes = procs;

        // Adjust selection if out of bounds
        if let Some(selected) = self.state.selected() {
            if selected >= self.processes.len() {
                if !self.processes.is_empty() {
                    self.state.select(Some(self.processes.len() - 1));
                } else {
                    self.state.select(None);
                }
            }
        } else if !self.processes.is_empty() {
            self.state.select(Some(0));
        }
    }

    fn on_tick(&mut self) {
        if self.last_refresh.elapsed() >= Duration::from_secs(1) {
            self.refresh_processes();
            self.last_refresh = Instant::now();
        }
    }

    fn next(&mut self) {
        if self.processes.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.processes.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn previous(&mut self) {
        if self.processes.is_empty() {
            return;
        }
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.processes.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    fn kill_selected(&mut self, signal: sysinfo::Signal) {
        if let Some(selected) = self.state.selected()
            && let Some((pid, _, _, _, _)) = self.processes.get(selected)
            && let Some(process) = self.system.process(*pid)
        {
            process.kill_with(signal);
        }
    }
}

pub fn command(_ctx: &Context, _argv: Vec<String>, _proxy: &mut dyn ShellProxy) -> ExitStatus {
    // Setup terminal
    enable_raw_mode().unwrap();
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).unwrap();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = App::new();
    let res = run_app(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode().unwrap();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).unwrap();
    terminal.show_cursor().unwrap();

    if let Err(err) = res {
        println!("{:?}", err);
        return ExitStatus::ExitedWith(1);
    }

    ExitStatus::ExitedWith(0)
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if app.input_mode {
                match key.code {
                    KeyCode::Enter => app.input_mode = false,
                    KeyCode::Esc => {
                        app.input_mode = false;
                        app.filter.clear();
                        app.refresh_processes();
                    }
                    KeyCode::Char(c) => {
                        app.filter.push(c);
                        app.refresh_processes();
                    }
                    KeyCode::Backspace => {
                        app.filter.pop();
                        app.refresh_processes();
                    }
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('j') | KeyCode::Down => app.next(),
                    KeyCode::Char('k') | KeyCode::Up => app.previous(),
                    KeyCode::Char('/') => {
                        app.input_mode = true;
                    }
                    KeyCode::Char('x') => {
                        app.kill_selected(sysinfo::Signal::Term);
                    }
                    KeyCode::Char('X') => {
                        app.kill_selected(sysinfo::Signal::Kill);
                    }
                    _ => {}
                }
            }
        }

        app.on_tick();

        if app.should_quit {
            return Ok(());
        }
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Min(0),
                Constraint::Length(3),
                Constraint::Length(1),
            ]
            .as_ref(),
        )
        .split(f.area());

    let rows: Vec<Row> = app
        .processes
        .iter()
        .map(|(pid, name, cpu, mem, status)| {
            Row::new(vec![
                Cell::from(format!("{}", pid)),
                Cell::from(name.as_str()),
                Cell::from(format!("{:.1}%", cpu)),
                Cell::from(format!("{} KB", mem / 1024)),
                Cell::from(
                    status
                        .replace("Run", "R")
                        .replace("Sleep", "S")
                        .replace("Idle", "I")
                        .to_string(),
                ),
            ])
            .style(Style::default().fg(Color::Gray))
        })
        .collect();

    let widths = [
        Constraint::Length(8),
        Constraint::Min(20),
        Constraint::Length(8),
        Constraint::Length(12),
        Constraint::Length(10),
    ];

    let table = Table::new(rows, widths)
        .header(
            Row::new(vec!["PID", "Name", "CPU", "Mem", "Status"])
                .style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .bottom_margin(1),
        )
        .block(Block::default().borders(Borders::ALL).title("Processes"))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");

    f.render_stateful_widget(table, chunks[0], &mut app.state);

    let filter_text = if app.input_mode {
        format!("Filter: {}_", app.filter)
    } else {
        format!("Filter: {}", app.filter)
    };

    let filter = Paragraph::new(filter_text)
        .style(if app.input_mode {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        })
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(filter, chunks[1]);

    let help_text = "q:Quit | /:Filter | j/k: Nav | x: Kill(TERM) | X: Kill(KILL)";
    let help = Paragraph::new(help_text).style(Style::default().fg(Color::Gray));
    f.render_widget(help, chunks[2]);
}

use std::{
    collections::VecDeque,
    io::{self, Stdout},
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, Paragraph, Wrap},
};

use crate::{
    error::{Result, XferError},
    protocol::DEFAULT_PORT,
    reporter::{Progress, Reporter, TrustPrompt},
    transfer::{ReceiveOptions, SendOptions, TransferSummary, receive, send},
};

type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

pub fn run(config_dir: Option<PathBuf>) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(error.into());
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(error) => {
            let _ = disable_raw_mode();
            let mut stdout = io::stdout();
            let _ = execute!(stdout, LeaveAlternateScreen);
            return Err(error.into());
        }
    };

    let result = run_app(&mut terminal, config_dir);
    let raw_mode_result = disable_raw_mode();
    let screen_result = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let cursor_result = terminal.show_cursor();
    let cleanup_result = raw_mode_result.and(screen_result).and(cursor_result);

    match (result, cleanup_result) {
        (Err(error), _) => Err(error.into()),
        (Ok(()), Err(error)) => Err(error.into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn run_app(terminal: &mut TuiTerminal, config_dir: Option<PathBuf>) -> Result<()> {
    let (event_tx, event_rx) = mpsc::channel();
    let mut app = App::new(config_dir, event_tx, event_rx);
    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();

    loop {
        app.drain_worker_events();
        terminal.draw(|frame| app.draw(frame))?;
        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && app.handle_key(key)?
        {
            return Ok(());
        }
        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Send,
    Receive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Home,
    Form,
    Running,
}

struct Form {
    mode: Mode,
    values: Vec<String>,
    focus: usize,
    secure: bool,
    option: bool,
}

impl Form {
    fn send() -> Self {
        Self {
            mode: Mode::Send,
            values: vec![
                String::new(),
                String::new(),
                DEFAULT_PORT.to_string(),
                String::new(),
            ],
            focus: 0,
            secure: true,
            option: false,
        }
    }

    fn receive() -> Self {
        Self {
            mode: Mode::Receive,
            values: vec![
                ".".into(),
                "::".into(),
                DEFAULT_PORT.to_string(),
                String::new(),
            ],
            focus: 0,
            secure: true,
            option: false,
        }
    }

    fn labels(&self) -> [&'static str; 4] {
        match self.mode {
            Mode::Send => ["Receiver", "Path", "Port", "Token (optional)"],
            Mode::Receive => [
                "Output directory",
                "Bind address",
                "Port",
                "Token (optional)",
            ],
        }
    }
}

enum WorkerEvent {
    Status(String),
    Progress(Progress),
    Sas(String, String),
    Trust(TrustPrompt, SyncSender<bool>),
    Finished(std::result::Result<TransferSummary, String>),
}

struct UiReporter {
    sender: Sender<WorkerEvent>,
    accept_new: bool,
}

impl Reporter for UiReporter {
    fn status(&self, message: &str) {
        let _ = self.sender.send(WorkerEvent::Status(message.to_string()));
    }

    fn progress(&self, progress: &Progress) {
        let _ = self.sender.send(WorkerEvent::Progress(progress.clone()));
    }

    fn show_sas(&self, sas: &str, fingerprint: &str) {
        let _ = self
            .sender
            .send(WorkerEvent::Sas(sas.to_string(), fingerprint.to_string()));
    }

    fn confirm_peer(&self, prompt: &TrustPrompt) -> Result<bool> {
        if self.accept_new && !prompt.changed {
            self.status(&format!("trusting new peer {}", prompt.endpoint));
            return Ok(true);
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.sender
            .send(WorkerEvent::Trust(prompt.clone(), reply_tx))
            .map_err(|_| XferError::Cancelled)?;
        reply_rx.recv().map_err(|_| XferError::Cancelled)
    }
}

struct App {
    screen: Screen,
    home_selection: usize,
    form: Form,
    config_dir: Option<PathBuf>,
    worker_tx: Sender<WorkerEvent>,
    worker_rx: Receiver<WorkerEvent>,
    logs: VecDeque<String>,
    progress: Option<Progress>,
    pending_trust: Option<(TrustPrompt, SyncSender<bool>)>,
    finished: bool,
}

impl App {
    fn new(
        config_dir: Option<PathBuf>,
        worker_tx: Sender<WorkerEvent>,
        worker_rx: Receiver<WorkerEvent>,
    ) -> Self {
        Self {
            screen: Screen::Home,
            home_selection: 0,
            form: Form::send(),
            config_dir,
            worker_tx,
            worker_rx,
            logs: VecDeque::new(),
            progress: None,
            pending_trust: None,
            finished: false,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if let Some((prompt, reply)) = self.pending_trust.take() {
            match key.code {
                KeyCode::Char('y' | 'Y') => {
                    let _ = reply.send(true);
                    self.push_log("Peer trusted.".into());
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                    let _ = reply.send(false);
                    self.push_log("Peer rejected.".into());
                }
                _ => {
                    self.pending_trust = Some((prompt, reply));
                }
            }
            return Ok(false);
        }

        match self.screen {
            Screen::Home => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
                KeyCode::Up | KeyCode::Char('k') => {
                    self.home_selection = self.home_selection.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.home_selection = (self.home_selection + 1).min(1);
                }
                KeyCode::Enter => {
                    self.form = if self.home_selection == 0 {
                        Form::send()
                    } else {
                        Form::receive()
                    };
                    self.screen = Screen::Form;
                }
                _ => {}
            },
            Screen::Form => match key.code {
                KeyCode::Esc => self.screen = Screen::Home,
                KeyCode::Tab | KeyCode::Down => {
                    self.form.focus = (self.form.focus + 1) % self.form.values.len();
                }
                KeyCode::BackTab | KeyCode::Up => {
                    self.form.focus = self
                        .form
                        .focus
                        .checked_sub(1)
                        .unwrap_or(self.form.values.len() - 1);
                }
                KeyCode::Backspace => {
                    self.form.values[self.form.focus].pop();
                }
                KeyCode::F(2) => self.start_transfer()?,
                KeyCode::F(3) => self.form.secure = !self.form.secure,
                KeyCode::F(4) => self.form.option = !self.form.option,
                KeyCode::Char(character) => {
                    self.form.values[self.form.focus].push(character);
                }
                _ => {}
            },
            Screen::Running => {
                if self.finished
                    && matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q'))
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn start_transfer(&mut self) -> Result<()> {
        let port = self.form.values[2]
            .parse::<u16>()
            .map_err(|_| XferError::invalid_input("port must be between 1 and 65535"))?;
        let reporter = Arc::new(UiReporter {
            sender: self.worker_tx.clone(),
            accept_new: self.form.mode == Mode::Send && self.form.option,
        });
        let config_dir = self.config_dir.clone();
        let values = self.form.values.clone();
        let secure = self.form.secure;
        let option = self.form.option;
        let mode = self.form.mode;
        self.logs.clear();
        self.progress = None;
        self.finished = false;
        self.screen = Screen::Running;
        let sender = self.worker_tx.clone();
        thread::spawn(move || {
            let result = match mode {
                Mode::Send => send(
                    &SendOptions {
                        host: values[0].clone(),
                        port,
                        input: PathBuf::from(&values[1]),
                        excludes: Vec::new(),
                        follow_links: false,
                        secure,
                        token: nonempty(values[3].clone()),
                        connect_timeout: Duration::from_secs(30),
                        config_dir,
                    },
                    reporter.as_ref(),
                ),
                Mode::Receive => receive(
                    &ReceiveOptions {
                        output: PathBuf::from(&values[0]),
                        bind: values[1].clone(),
                        port,
                        overwrite: option,
                        secure,
                        token: nonempty(values[3].clone()),
                        config_dir,
                    },
                    reporter.as_ref(),
                ),
            };
            let _ = sender.send(WorkerEvent::Finished(
                result.map_err(|error| error.to_string()),
            ));
        });
        Ok(())
    }

    fn drain_worker_events(&mut self) {
        while let Ok(event) = self.worker_rx.try_recv() {
            match event {
                WorkerEvent::Status(message) => self.push_log(message),
                WorkerEvent::Progress(progress) => self.progress = Some(progress),
                WorkerEvent::Sas(sas, fingerprint) => {
                    self.push_log(format!("Security code: {sas}"));
                    self.push_log(format!("Identity: {fingerprint}"));
                }
                WorkerEvent::Trust(prompt, reply) => {
                    self.pending_trust = Some((prompt, reply));
                }
                WorkerEvent::Finished(result) => {
                    match result {
                        Ok(summary) => self.push_log(format!(
                            "Complete: {} bytes, {} file(s), peer {}",
                            summary.total_bytes, summary.file_count, summary.peer
                        )),
                        Err(error) => self.push_log(format!("Error: {error}")),
                    }
                    self.finished = true;
                }
            }
        }
    }

    fn push_log(&mut self, message: String) {
        self.logs.push_back(message);
        while self.logs.len() > 100 {
            self.logs.pop_front();
        }
    }

    fn draw(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(2),
            ])
            .split(area);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " XFER ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  secure direct transfer"),
            ]))
            .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        match self.screen {
            Screen::Home => self.draw_home(frame, chunks[1]),
            Screen::Form => self.draw_form(frame, chunks[1]),
            Screen::Running => self.draw_running(frame, chunks[1]),
        }
        let help = match self.screen {
            Screen::Home => "↑/↓ select  Enter open  q quit",
            Screen::Form => "Tab field  F2 start  F3 security  F4 option  Esc back",
            Screen::Running if self.finished => "Enter, Esc, or q to close",
            Screen::Running => "Transfer in progress",
        };
        frame.render_widget(
            Paragraph::new(help)
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::DarkGray)),
            chunks[2],
        );
    }

    fn draw_home(&self, frame: &mut Frame<'_>, area: Rect) {
        let items = ["Send a file or directory", "Receive one transfer"];
        let rows = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let prefix = if index == self.home_selection {
                    "› "
                } else {
                    "  "
                };
                ListItem::new(format!("{prefix}{item}")).style(if index == self.home_selection {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                })
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            List::new(rows).block(
                Block::default()
                    .title(" Choose a workflow ")
                    .borders(Borders::ALL),
            ),
            centered(area, 60, 10),
        );
    }

    fn draw_form(&self, frame: &mut Frame<'_>, area: Rect) {
        let labels = self.form.labels();
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
            ])
            .split(centered(area, 78, 17));
        for (index, value) in self.form.values.iter().enumerate() {
            let style = if index == self.form.focus {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let displayed = if index == 3 {
                "•".repeat(value.chars().count())
            } else {
                value.clone()
            };
            frame.render_widget(
                Paragraph::new(displayed).block(
                    Block::default()
                        .title(format!(" {} ", labels[index]))
                        .borders(Borders::ALL)
                        .border_style(style),
                ),
                rows[index],
            );
        }
        let option = match self.form.mode {
            Mode::Send => format!(
                "Security: {}    Accept new peer: {}",
                on_off(self.form.secure),
                on_off(self.form.option)
            ),
            Mode::Receive => format!(
                "Security: {}    Overwrite: {}",
                on_off(self.form.secure),
                on_off(self.form.option)
            ),
        };
        frame.render_widget(
            Paragraph::new(option)
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL)),
            rows[4],
        );
    }

    fn draw_running(&self, frame: &mut Frame<'_>, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(4), Constraint::Min(6)])
            .split(area);
        let (percent, label) = self.progress.as_ref().map_or((0, "Waiting…".into()), |p| {
            let percent = if p.total == 0 {
                100
            } else {
                let value = u128::from(p.transferred) * 100 / u128::from(p.total);
                u16::try_from(value.min(100)).unwrap_or(100)
            };
            (
                percent,
                format!(
                    "{} {} — {}/{} files",
                    p.phase, p.current_path, p.files_done, p.files_total
                ),
            )
        });
        frame.render_widget(
            Gauge::default()
                .block(Block::default().title(" Transfer ").borders(Borders::ALL))
                .gauge_style(Style::default().fg(Color::Cyan))
                .percent(percent)
                .label(label),
            chunks[0],
        );
        let lines = self
            .logs
            .iter()
            .rev()
            .take(chunks[1].height.saturating_sub(2) as usize)
            .rev()
            .map(|line| Line::from(line.as_str()))
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(Block::default().title(" Activity ").borders(Borders::ALL)),
            chunks[1],
        );

        if let Some((prompt, _)) = &self.pending_trust {
            let modal = centered(area, 72, 11);
            frame.render_widget(Clear, modal);
            let warning = if prompt.changed {
                "SAVED IDENTITY CHANGED"
            } else {
                "NEW PEER"
            };
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        warning,
                        Style::default()
                            .fg(if prompt.changed {
                                Color::Red
                            } else {
                                Color::Yellow
                            })
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::from(format!("Endpoint: {}", prompt.endpoint)),
                    Line::from(format!("Security code: {}", prompt.sas)),
                    Line::from(format!("Fingerprint: {}", prompt.fingerprint)),
                    Line::from("Compare the code on the receiver. Trust this peer? [y/N]"),
                ])
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .title(" Confirm peer identity ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Yellow)),
                ),
                modal,
            );
        }
    }
}

fn centered(area: Rect, percent_x: u16, height: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height.min(area.height)),
            Constraint::Fill(1),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn nonempty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Stdout},
    net::{IpAddr, SocketAddr},
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
    discovery::{self, Browser, DiscoveredPeer},
    error::{Result, XferError},
    net,
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
            && app.handle_key(key)
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
    discoverable: bool,
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
            discoverable: true,
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
            discoverable: true,
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

struct SeenPeer {
    peer: DiscoveredPeer,
    last_seen: Instant,
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
    local_addresses: Vec<IpAddr>,
    address_error: Option<String>,
    browser: Option<Browser>,
    discovery_error: Option<String>,
    peers: BTreeMap<SocketAddr, SeenPeer>,
    peer_selection: usize,
}

impl App {
    fn new(
        config_dir: Option<PathBuf>,
        worker_tx: Sender<WorkerEvent>,
        worker_rx: Receiver<WorkerEvent>,
    ) -> Self {
        let (local_addresses, address_error) = match net::local_addresses() {
            Ok(addresses) => (addresses, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        let (browser, discovery_error) = match Browser::start() {
            Ok(browser) => (Some(browser), None),
            Err(error) => (None, Some(error.to_string())),
        };
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
            local_addresses,
            address_error,
            browser,
            discovery_error,
            peers: BTreeMap::new(),
            peer_selection: 0,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
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
            return false;
        }

        match self.screen {
            Screen::Home => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return true,
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
                KeyCode::Tab => {
                    self.form.focus = (self.form.focus + 1) % self.form.values.len();
                }
                KeyCode::Down => {
                    if self.peer_navigation_active() {
                        self.peer_selection =
                            (self.peer_selection + 1).min(self.peers.len().saturating_sub(1));
                    } else {
                        self.form.focus = (self.form.focus + 1) % self.form.values.len();
                    }
                }
                KeyCode::BackTab => {
                    self.form.focus = self
                        .form
                        .focus
                        .checked_sub(1)
                        .unwrap_or(self.form.values.len() - 1);
                }
                KeyCode::Up => {
                    if self.peer_navigation_active() {
                        self.peer_selection = self.peer_selection.saturating_sub(1);
                    } else {
                        self.form.focus = self
                            .form
                            .focus
                            .checked_sub(1)
                            .unwrap_or(self.form.values.len() - 1);
                    }
                }
                KeyCode::Enter if self.form.mode == Mode::Send => {
                    self.select_discovered_peer();
                }
                KeyCode::Backspace => {
                    self.form.values[self.form.focus].pop();
                }
                KeyCode::F(2) => {
                    if let Err(error) = self.start_transfer() {
                        self.push_log(format!("Input error: {error}"));
                    }
                }
                KeyCode::F(3) => self.form.secure = !self.form.secure,
                KeyCode::F(4) => self.form.option = !self.form.option,
                KeyCode::F(5) => match self.form.mode {
                    Mode::Send => self.select_discovered_peer(),
                    Mode::Receive => self.form.discoverable = !self.form.discoverable,
                },
                KeyCode::Char(character) => {
                    self.form.values[self.form.focus].push(character);
                }
                _ => {}
            },
            Screen::Running => {
                if self.finished
                    && matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q'))
                {
                    return true;
                }
            }
        }
        false
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
        let discoverable = self.form.discoverable;
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
                        discoverable,
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
        self.drain_discovery();
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

    fn drain_discovery(&mut self) {
        while let Some(peer) = self.browser.as_ref().and_then(Browser::try_recv) {
            self.peers.insert(
                peer.address,
                SeenPeer {
                    peer,
                    last_seen: Instant::now(),
                },
            );
        }
        let now = Instant::now();
        self.peers
            .retain(|_, peer| now.saturating_duration_since(peer.last_seen) <= discovery::PEER_TTL);
        self.peer_selection = self.peer_selection.min(self.peers.len().saturating_sub(1));
    }

    fn peer_navigation_active(&self) -> bool {
        self.form.mode == Mode::Send && self.form.focus == 0 && !self.peers.is_empty()
    }

    fn select_discovered_peer(&mut self) {
        let address = self
            .peers
            .values()
            .nth(self.peer_selection)
            .map(|seen| seen.peer.address);
        if let Some(address) = address {
            self.form.values[0] = address.ip().to_string();
            self.form.values[2] = address.port().to_string();
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
            Screen::Form if self.form.mode == Mode::Send => {
                "Tab field  ↑/↓ receiver  Enter/F5 choose  F2 start  F3 security  F4 trust"
            }
            Screen::Form => {
                "Tab field  F2 start  F3 security  F4 overwrite  F5 discovery  Esc back"
            }
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
        let (max_height, constraints) = match self.form.mode {
            Mode::Send => (
                26,
                vec![
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Min(4),
                ],
            ),
            Mode::Receive => (
                24,
                vec![
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Min(4),
                ],
            ),
        };
        let height = area.height.min(max_height);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(centered(area, 82, height));
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
                "Security: {}    Overwrite: {}    LAN discovery: {}",
                on_off(self.form.secure),
                on_off(self.form.option),
                on_off(self.form.discoverable)
            ),
        };
        frame.render_widget(
            Paragraph::new(option)
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL)),
            rows[4],
        );
        match self.form.mode {
            Mode::Send => self.draw_discovered_peers(frame, rows[5]),
            Mode::Receive => self.draw_receiver_addresses(frame, rows[5]),
        }
    }

    fn draw_discovered_peers(&self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::default()
            .title(format!(" Nearby receivers ({}) ", self.peers.len()))
            .borders(Borders::ALL);
        if self.peers.is_empty() {
            let message = self.discovery_error.as_ref().map_or_else(
                || {
                    "Listening for XFER receivers on this LAN… Start Receive on another machine. XFER does not scan addresses or ports."
                        .to_string()
                },
                |error| {
                    format!(
                        "Automatic discovery is unavailable ({error}). Enter the receiver IP manually."
                    )
                },
            );
            frame.render_widget(
                Paragraph::new(message)
                    .wrap(Wrap { trim: true })
                    .block(block),
                area,
            );
            return;
        }

        let visible_rows = usize::from(area.height.saturating_sub(2)).max(1);
        let start = self
            .peer_selection
            .saturating_add(1)
            .saturating_sub(visible_rows);
        let items = self
            .peers
            .values()
            .enumerate()
            .skip(start)
            .take(visible_rows)
            .map(|(index, seen)| {
                let selected = index == self.peer_selection;
                let prefix = if selected { "›" } else { " " };
                let security = if seen.peer.secure {
                    "secure"
                } else {
                    "insecure"
                };
                ListItem::new(format!(
                    "{prefix} {}  {}  [{security}]",
                    seen.peer.name, seen.peer.address
                ))
                .style(if selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                })
            })
            .collect::<Vec<_>>();
        frame.render_widget(List::new(items).block(block), area);
    }

    fn draw_receiver_addresses(&self, frame: &mut Frame<'_>, area: Rect) {
        let endpoints = self.receiver_endpoints();
        let lines = if endpoints.is_empty() {
            vec![Line::from(self.address_error.as_ref().map_or_else(
                || "No non-loopback address detected; loopback still works.".into(),
                |error| format!("Could not enumerate local addresses: {error}"),
            ))]
        } else {
            summarize_endpoints(&endpoints)
        };
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .title(" Receiver addresses ")
                    .borders(Borders::ALL),
            ),
            area,
        );
    }

    fn receiver_endpoints(&self) -> Vec<SocketAddr> {
        let Ok(port) = self.form.values[2].parse::<u16>() else {
            return Vec::new();
        };
        let Ok(bind) = self.form.values[1].parse::<IpAddr>() else {
            return Vec::new();
        };
        net::endpoints_for_bind(bind, port, &self.local_addresses)
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

fn summarize_endpoints(endpoints: &[SocketAddr]) -> Vec<Line<'static>> {
    let ipv4 = endpoints
        .iter()
        .filter(|endpoint| endpoint.is_ipv4())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let ipv6 = endpoints
        .iter()
        .filter(|endpoint| endpoint.is_ipv6())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    if !ipv4.is_empty() {
        lines.push(Line::from(format!(
            "IPv4: {}{}",
            ipv4.iter().take(2).cloned().collect::<Vec<_>>().join("  "),
            more_count(ipv4.len(), 2)
        )));
    }
    if !ipv6.is_empty() {
        lines.push(Line::from(format!(
            "IPv6: {}{}",
            ipv6[0],
            more_count(ipv6.len(), 1)
        )));
    }
    lines
}

fn more_count(total: usize, shown: usize) -> String {
    if total > shown {
        format!("  (+{} more; xfer ip)", total - shown)
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use crossterm::event::KeyModifiers;

    use super::*;

    #[test]
    fn forms_start_with_secure_production_defaults() {
        let send = Form::send();
        assert!(send.secure);
        assert!(send.discoverable);
        assert!(!send.option);
        assert_eq!(send.values[2], DEFAULT_PORT.to_string());

        let receive = Form::receive();
        assert!(receive.secure);
        assert!(receive.discoverable);
        assert!(!receive.option);
        assert_eq!(receive.values[1], "::");
    }

    #[test]
    fn receiver_endpoint_summary_groups_families_and_limits_output() {
        let endpoints = vec![
            "192.168.1.20:9000".parse().unwrap(),
            "10.0.0.2:9000".parse().unwrap(),
            "172.16.0.2:9000".parse().unwrap(),
            "[2001:db8::1]:9000".parse().unwrap(),
            "[2001:db8::2]:9000".parse().unwrap(),
        ];
        let rendered = summarize_endpoints(&endpoints)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert_eq!(rendered.len(), 2);
        assert!(rendered[0].contains("IPv4:"));
        assert!(rendered[0].contains("+1 more"));
        assert!(rendered[1].contains("IPv6:"));
        assert!(rendered[1].contains("+1 more"));
    }

    #[test]
    fn centered_rectangle_never_exceeds_available_area() {
        let area = Rect::new(0, 0, 40, 10);
        let result = centered(area, 80, 24);
        assert!(result.width <= area.width);
        assert!(result.height <= area.height);
    }

    #[test]
    fn helper_values_handle_empty_and_nonempty_inputs() {
        assert_eq!(nonempty(String::new()), None);
        assert_eq!(nonempty("token".into()), Some("token".into()));
        assert_eq!(on_off(true), "on");
        assert_eq!(on_off(false), "off");
        assert_eq!(more_count(2, 2), "");
        assert_eq!(more_count(4, 2), "  (+2 more; xfer ip)");
    }

    #[test]
    fn invalid_port_keeps_the_form_open() {
        let (worker_tx, worker_rx) = mpsc::channel();
        let mut app = App::new(None, worker_tx, worker_rx);
        app.screen = Screen::Form;
        app.form.values[2] = "not-a-port".into();

        let should_quit = app.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));

        assert!(!should_quit);
        assert_eq!(app.screen, Screen::Form);
        assert!(
            app.logs
                .back()
                .is_some_and(|message| message.contains("port must be between"))
        );
    }
}

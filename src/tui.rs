//! A keyboard-first workflow: choose content, choose a computer, review, run.
use crate::{
    control::TransferControl,
    discovery::{Browser, DiscoveredPeer, PEER_TTL},
    error::{Result, XferError},
    net,
    protocol::{DEFAULT_PORT, sanitize_peer_text},
    reporter::{Progress, Reporter, TrustPrompt},
    transfer::{
        ReceiveOptions, SendOptions, TransferSummary, human_bytes, receive_controlled,
        send_controlled,
    },
};
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap},
};
use rust_cli_toolkit::{LockedJsonStore, SecureDir};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fs,
    io::{self, Stdout},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
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
    execute!(terminal.backend_mut(), EnableBracketedPaste)?;
    let mut app = App::new(config_dir);
    let result = (|| loop {
        app.poll();
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(80))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if app.key(key) {
                        break Ok(());
                    }
                }
                Event::Paste(text) => app.paste(&text),
                _ => {}
            }
        }
    })();
    let _ = execute!(terminal.backend_mut(), DisableBracketedPaste);
    result
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Action {
    #[default]
    Copy,
    Sync,
    TwoWay,
    Receive,
}
impl Action {
    fn title(self) -> &'static str {
        match self {
            Self::Copy => "Send a copy",
            Self::Sync => "Sync to another computer",
            Self::TwoWay => "Sync both computers",
            Self::Receive => "Receive on this computer",
        }
    }
    fn syncing(self) -> bool {
        matches!(self, Self::Sync | Self::TwoWay)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Recent {
    action: Action,
    path: PathBuf,
    host: String,
    port: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Home,
    Folder,
    Computer,
    Review,
    Running,
    Preview,
    Result,
}
#[derive(Clone, Copy)]
enum Edit {
    Path,
    Host,
    Port,
    Token,
    Excludes,
    Bind,
}
struct Input {
    text: String,
    cursor: usize,
}
impl Input {
    fn new(text: String) -> Self {
        let cursor = text.len();
        Self { text, cursor }
    }
    fn insert(&mut self, text: &str) {
        let clean = text.chars().filter(|c| !c.is_control()).collect::<String>();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }
    fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&c.to_string());
            }
            KeyCode::Left => {
                self.cursor = self.text[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(i, _)| i);
            }
            KeyCode::Right => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            KeyCode::Backspace => {
                if let Some((i, _)) = self.text[..self.cursor].char_indices().next_back() {
                    self.text.drain(i..self.cursor);
                    self.cursor = i;
                }
            }
            KeyCode::Delete => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    self.text.drain(self.cursor..self.cursor + c.len_utf8());
                }
            }
            _ => {}
        }
    }
}
struct Folder {
    directory: PathBuf,
    entries: Vec<PathBuf>,
    selected: usize,
    hidden: bool,
}
impl Folder {
    fn new(directory: PathBuf) -> Self {
        let mut value = Self {
            directory,
            entries: Vec::new(),
            selected: 0,
            hidden: false,
        };
        let _ = value.reload();
        value
    }
    fn reload(&mut self) -> Result<()> {
        let mut entries = fs::read_dir(&self.directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.retain(|path| {
            self.hidden
                || !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        });
        entries.sort_by_key(|path| {
            (
                !path.is_dir(),
                path.file_name().map(std::ffi::OsStr::to_os_string),
            )
        });
        self.entries = entries;
        self.selected = 0;
        Ok(())
    }
    fn count(&self) -> usize {
        self.entries.len() + 2
    }
}
struct UiReporter {
    tx: SyncSender<WorkerEvent>,
}
impl Reporter for UiReporter {
    fn status(&self, message: &str) {
        let _ = self.tx.send(WorkerEvent::Status(message.into()));
    }
    fn progress(&self, progress: &Progress) {
        let _ = self.tx.send(WorkerEvent::Progress(progress.clone()));
    }
    fn show_sas(&self, sas: &str, fingerprint: &str) {
        let _ = self
            .tx
            .send(WorkerEvent::Sas(sas.into(), fingerprint.into()));
    }
    fn confirm_peer(&self, prompt: &TrustPrompt) -> Result<bool> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .send(WorkerEvent::Trust(prompt.clone(), tx))
            .map_err(|_| XferError::Cancelled)?;
        rx.recv().map_err(|_| XferError::Cancelled)
    }
}
enum WorkerEvent {
    Status(String),
    Progress(Progress),
    Sas(String, String),
    Trust(TrustPrompt, SyncSender<bool>),
    Received(TransferSummary),
    Finished(std::result::Result<TransferSummary, String>),
}
struct App {
    screen: Screen,
    action: Action,
    selection: usize,
    folder: Folder,
    path: PathBuf,
    host: String,
    port: u16,
    token: String,
    bind: String,
    excludes: Vec<String>,
    allow_sync: bool,
    conflict_policy: crate::transfer::ConflictPolicy,
    config_dir: Option<PathBuf>,
    edit: Option<(Edit, Input)>,
    error: Option<String>,
    browser: Option<Browser>,
    peers: Vec<(DiscoveredPeer, Instant)>,
    discovery_error: Option<String>,
    addresses: Vec<String>,
    tx: SyncSender<WorkerEvent>,
    rx: Receiver<WorkerEvent>,
    control: Arc<TransferControl>,
    progress: Option<Progress>,
    logs: VecDeque<String>,
    trust: Option<(TrustPrompt, SyncSender<bool>)>,
    sas: Option<String>,
    summary: Option<TransferSummary>,
    recent: Option<Recent>,
    started: Instant,
    details: bool,
}
impl App {
    fn new(config_dir: Option<PathBuf>) -> Self {
        let (tx, rx) = mpsc::sync_channel(64);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let recent = preferences(config_dir.clone())
            .and_then(|store| Ok(store.load()?))
            .ok()
            .filter(|recent| !recent.path.as_os_str().is_empty());
        let (browser, discovery_error) = match Browser::start() {
            Ok(browser) => (Some(browser), None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
            screen: Screen::Home,
            action: Action::Copy,
            selection: 0,
            folder: Folder::new(cwd.clone()),
            path: cwd,
            host: String::new(),
            port: DEFAULT_PORT,
            token: std::env::var("XFER_TOKEN").unwrap_or_default(),
            bind: "::".into(),
            excludes: Vec::new(),
            allow_sync: false,
            conflict_policy: crate::transfer::ConflictPolicy::Preserve,
            config_dir,
            edit: None,
            error: None,
            browser,
            peers: Vec::new(),
            discovery_error,
            addresses: net::local_addresses()
                .unwrap_or_default()
                .iter()
                .map(ToString::to_string)
                .collect(),
            tx,
            rx,
            control: Arc::new(TransferControl::default()),
            progress: None,
            logs: VecDeque::new(),
            trust: None,
            sas: None,
            summary: None,
            recent,
            started: Instant::now(),
            details: false,
        }
    }
    fn log(&mut self, message: String) {
        self.logs.push_back(message);
        while self.logs.len() > 80 {
            self.logs.pop_front();
        }
    }
    fn poll(&mut self) {
        for _ in 0..128 {
            let Some(peer) = self.browser.as_ref().and_then(Browser::try_recv) else {
                break;
            };
            if !peer.secure {
                continue;
            }
            if let Some((existing, time)) = self
                .peers
                .iter_mut()
                .find(|(existing, _)| existing.address == peer.address)
            {
                *existing = peer;
                *time = Instant::now();
            } else {
                self.peers.push((peer, Instant::now()));
            }
        }
        self.peers.retain(|(_, time)| time.elapsed() < PEER_TTL);
        if self.screen == Screen::Computer {
            self.selection = self.selection.min(self.peers.len());
        }
        for _ in 0..128 {
            let Ok(event) = self.rx.try_recv() else {
                break;
            };
            match event {
                WorkerEvent::Status(message) => self.log(message),
                WorkerEvent::Progress(progress) => self.progress = Some(progress),
                WorkerEvent::Sas(sas, fingerprint) => {
                    self.sas = Some(sas);
                    self.log(format!("Receiver identity: {fingerprint}"));
                }
                WorkerEvent::Trust(prompt, reply) => {
                    if self.control.check().is_ok() {
                        self.trust = Some((prompt, reply));
                    } else {
                        let _ = reply.send(false);
                    }
                }
                WorkerEvent::Received(summary) => {
                    self.log(format!(
                        "{}: {}",
                        if summary.preview {
                            "Preview complete"
                        } else {
                            "Saved"
                        },
                        summary.destination.display()
                    ));
                    self.summary = Some(summary);
                    self.progress = None;
                    self.sas = None;
                }
                WorkerEvent::Finished(result) => {
                    self.trust = None;
                    match result {
                        Ok(summary) => {
                            let preview = summary.preview;
                            self.summary = Some(summary);
                            self.screen = if preview {
                                Screen::Preview
                            } else {
                                Screen::Result
                            };
                            self.error = None;
                            if !preview {
                                let recent = Recent {
                                    action: self.action,
                                    path: self.path.clone(),
                                    host: self.host.clone(),
                                    port: self.port,
                                };
                                if let Ok(store) = preferences(self.config_dir.clone()) {
                                    let _ = store.save(&recent);
                                }
                                self.recent = Some(recent);
                            }
                        }
                        Err(error) => {
                            self.error = Some(error);
                            self.screen = Screen::Result;
                        }
                    }
                }
            }
        }
    }
    fn cancel(&mut self) {
        self.control.cancel();
        if let Some((_, reply)) = self.trust.take() {
            let _ = reply.send(false);
        }
        self.log("Stopping safely…".into());
    }
    fn edit(&mut self, kind: Edit) {
        let text = match kind {
            Edit::Path => self.folder.directory.display().to_string(),
            Edit::Host => self.host.clone(),
            Edit::Port => self.port.to_string(),
            Edit::Token => self.token.clone(),
            Edit::Excludes => self.excludes.join(", "),
            Edit::Bind => self.bind.clone(),
        };
        self.edit = Some((kind, Input::new(text)));
        self.error = None;
    }
    fn paste(&mut self, text: &str) {
        if let Some((_, input)) = &mut self.edit {
            input.insert(text);
        } else if self.screen == Screen::Folder {
            self.edit = Some((Edit::Path, Input::new(text.trim().into())));
        } else if self.screen == Screen::Computer {
            self.edit = Some((Edit::Host, Input::new(text.trim().into())));
        }
    }
    fn apply_edit(&mut self, kind: Edit, text: String) -> Result<()> {
        match kind {
            Edit::Path => {
                let path = expand_path(&text)?;
                if path.is_dir() {
                    self.folder.directory = path;
                    self.folder.reload()?;
                } else if path.is_file() && self.action == Action::Copy {
                    self.choose_path(path)?;
                } else {
                    return Err(XferError::invalid_input(
                        "Choose an existing folder (or a file for Send a copy).",
                    ));
                }
            }
            Edit::Host => {
                let (host, port) = parse_endpoint(text.trim(), self.port)?;
                self.host = host;
                self.port = port;
                self.screen = Screen::Review;
            }
            Edit::Port => {
                self.port = text
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .ok_or_else(|| XferError::invalid_input("Port must be 1–65535."))?;
            }
            Edit::Token => self.token = text,
            Edit::Excludes => {
                self.excludes = text
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            Edit::Bind => {
                text.parse::<std::net::IpAddr>()
                    .map_err(|_| XferError::invalid_input("Enter an IPv4 or IPv6 address."))?;
                self.bind = text;
            }
        }
        Ok(())
    }
    fn choose_path(&mut self, path: PathBuf) -> Result<()> {
        if self.action != Action::Copy && !path.is_dir() {
            return Err(XferError::invalid_input("This workflow needs a folder."));
        }
        self.path = fs::canonicalize(path)?;
        self.selection = 0;
        self.screen = if self.action == Action::Receive {
            Screen::Review
        } else {
            Screen::Computer
        };
        Ok(())
    }
    fn key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.screen == Screen::Running {
                self.cancel();
                return false;
            }
            return true;
        }
        if let Some((prompt, reply)) = self.trust.take() {
            match key.code {
                KeyCode::Char('y' | 'Y') => {
                    let _ = reply.send(true);
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                    let _ = reply.send(false);
                }
                _ => self.trust = Some((prompt, reply)),
            }
            return false;
        }
        if let Some((kind, mut input)) = self.edit.take() {
            match key.code {
                KeyCode::Esc => self.error = None,
                KeyCode::Enter => {
                    if let Err(error) = self.apply_edit(kind, input.text.clone()) {
                        self.error = Some(error.to_string());
                        self.edit = Some((kind, input));
                    }
                }
                _ => {
                    input.key(key);
                    self.edit = Some((kind, input));
                }
            }
            return false;
        }
        if self.screen != Screen::Result {
            self.error = None;
        }
        match self.screen {
            Screen::Home => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return true,
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                    self.selection =
                        (self.selection + 1).min(if self.recent.is_some() { 4 } else { 3 });
                }
                KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                    self.selection = self.selection.saturating_sub(1);
                }
                KeyCode::Enter => {
                    self.conflict_policy = crate::transfer::ConflictPolicy::Preserve;
                    if self.selection == 4 {
                        if let Some(recent) = &self.recent {
                            self.action = recent.action;
                            self.path = recent.path.clone();
                            self.host = recent.host.clone();
                            self.port = recent.port;
                            self.screen = Screen::Review;
                        }
                    } else {
                        self.action = [Action::Copy, Action::Receive, Action::Sync, Action::TwoWay]
                            [self.selection];
                        self.screen = Screen::Folder;
                        self.folder.selected = 0;
                    }
                }
                _ => {}
            },
            Screen::Folder => match key.code {
                KeyCode::Esc => {
                    self.screen = Screen::Home;
                    self.selection = 0;
                }
                KeyCode::Char('p' | '/') => self.edit(Edit::Path),
                KeyCode::Char('h') => {
                    self.folder.hidden = !self.folder.hidden;
                    if let Err(e) = self.folder.reload() {
                        self.error = Some(e.to_string());
                    }
                }
                KeyCode::Down => {
                    self.folder.selected = (self.folder.selected + 1).min(self.folder.count() - 1);
                }
                KeyCode::Up => self.folder.selected = self.folder.selected.saturating_sub(1),
                KeyCode::Enter => {
                    let result = if self.folder.selected == 0 {
                        self.choose_path(self.folder.directory.clone())
                    } else {
                        let target = if self.folder.selected == 1 {
                            self.folder
                                .directory
                                .parent()
                                .unwrap_or(&self.folder.directory)
                                .to_path_buf()
                        } else {
                            self.folder.entries[self.folder.selected - 2].clone()
                        };
                        if target.is_dir() {
                            self.folder.directory = target;
                            self.folder.reload()
                        } else {
                            self.choose_path(target)
                        }
                    };
                    if let Err(error) = result {
                        self.error = Some(error.to_string());
                    }
                }
                _ => {}
            },
            Screen::Computer => match key.code {
                KeyCode::Esc => self.screen = Screen::Folder,
                KeyCode::Char('m') => self.edit(Edit::Host),
                KeyCode::Down => self.selection = (self.selection + 1).min(self.peers.len()),
                KeyCode::Up => self.selection = self.selection.saturating_sub(1),
                KeyCode::Enter => {
                    if let Some((peer, _)) = self.peers.get(self.selection) {
                        self.host = peer.address.ip().to_string();
                        self.port = peer.address.port();
                        self.screen = Screen::Review;
                    } else {
                        self.edit(Edit::Host);
                    }
                }
                _ => {}
            },
            Screen::Review => match key.code {
                KeyCode::Esc => {
                    self.screen = if self.action == Action::Receive {
                        Screen::Folder
                    } else {
                        Screen::Computer
                    }
                }
                KeyCode::Char('p') => self.edit(Edit::Port),
                KeyCode::Char('t') => self.edit(Edit::Token),
                KeyCode::Char('e') if self.action != Action::Receive => self.edit(Edit::Excludes),
                KeyCode::Char('s') if self.action == Action::Receive => {
                    self.allow_sync = !self.allow_sync;
                }
                KeyCode::Char('b') if self.action == Action::Receive => self.edit(Edit::Bind),
                KeyCode::Enter => self.start(self.action.syncing()),
                _ => {}
            },
            Screen::Running => match key.code {
                KeyCode::Esc => self.cancel(),
                KeyCode::Char('d') => self.details = !self.details,
                _ => {}
            },
            Screen::Preview => match key.code {
                KeyCode::Char('l') if self.action == Action::TwoWay => {
                    self.conflict_policy = crate::transfer::ConflictPolicy::PreferLocal;
                    self.start(true);
                }
                KeyCode::Char('r') if self.action == Action::TwoWay => {
                    self.conflict_policy = crate::transfer::ConflictPolicy::PreferRemote;
                    self.start(true);
                }
                KeyCode::Char('s') if self.action == Action::TwoWay => {
                    self.conflict_policy = crate::transfer::ConflictPolicy::Preserve;
                    self.start(true);
                }

                KeyCode::Esc => self.screen = Screen::Review,
                KeyCode::Enter => self.start(false),
                KeyCode::Char('d') => self.details = !self.details,
                _ => {}
            },
            Screen::Result => match key.code {
                KeyCode::Enter => self.screen = Screen::Review,
                KeyCode::Esc | KeyCode::Char('h') => {
                    self.screen = Screen::Home;
                    self.selection = 0;
                }
                KeyCode::Char('q') => return true,
                KeyCode::Char('d') => self.details = !self.details,
                _ => {}
            },
        }
        false
    }
    fn start(&mut self, preview: bool) {
        if !self.path.exists() {
            self.error =
                Some("The selected path no longer exists. Go back and choose it again.".into());
            return;
        }
        self.control = Arc::new(TransferControl::default());
        self.progress = None;
        self.summary = None;
        self.logs.clear();
        self.error = None;
        self.sas = None;
        self.details = false;
        self.started = Instant::now();
        self.screen = Screen::Running;
        let control = Arc::clone(&self.control);
        let tx = self.tx.clone();
        let action = self.action;
        let sender = SendOptions {
            conflict_policy: self.conflict_policy,
            sync: action.syncing(),
            preview,
            two_way: action == Action::TwoWay,
            host: self.host.clone(),
            port: self.port,
            input: self.path.clone(),
            excludes: self.excludes.clone(),
            follow_links: false,
            secure: true,
            token: (!self.token.is_empty()).then(|| self.token.clone()),
            connect_timeout: Duration::from_secs(30),
            config_dir: self.config_dir.clone(),
        };
        let receiver = ReceiveOptions {
            allow_sync: self.allow_sync,
            sync_into: true,
            bind: self.bind.clone(),
            port: self.port,
            output: self.path.clone(),
            overwrite: false,
            discoverable: true,
            secure: true,
            token: sender.token.clone(),
            config_dir: self.config_dir.clone(),
        };
        thread::spawn(move || {
            let reporter = UiReporter { tx: tx.clone() };
            let result = if action == Action::Receive {
                loop {
                    match receive_controlled(&receiver, &reporter, &control) {
                        Ok(summary) => {
                            if tx.send(WorkerEvent::Received(summary)).is_err() {
                                break Err(XferError::Cancelled);
                            }
                        }
                        Err(error) => break Err(error),
                    }
                }
            } else {
                send_controlled(&sender, &reporter, &control)
            };
            let _ = tx.send(WorkerEvent::Finished(
                result.map_err(|error| error.to_string()),
            ));
        });
    }
    fn draw(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if area.width < 58 || area.height < 18 {
            frame.render_widget(Paragraph::new("XFER needs a terminal at least 58 columns × 18 rows.\nResize this window; your work is still here.").wrap(Wrap{trim:true}),area);
            return;
        }
        let width = area.width.min(100);
        let area = Rect::new(
            area.x + (area.width - width) / 2,
            area.y,
            width,
            area.height,
        );
        let sections = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Min(10),
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
                Span::raw("  Secure file transfer & sync"),
            ])),
            sections[0],
        );
        let steps = match self.screen {
            Screen::Home => "What would you like to do?",
            Screen::Folder => "1  Choose content   →   2  Choose computer   →   3  Review",
            Screen::Computer => "✓  Content chosen   →   2  Choose computer   →   3  Review",
            Screen::Review => "✓  Content chosen   →   ✓  Computer   →   3  Review",
            Screen::Running => "Working · Esc stops safely",
            Screen::Preview => "Comparison complete · Nothing has been changed",
            Screen::Result => "Session complete",
        };
        frame.render_widget(
            Paragraph::new(steps).style(Style::default().fg(Color::DarkGray)),
            sections[1],
        );
        let body = sections[2];
        match self.screen {
            Screen::Home => self.draw_home(frame, body),
            Screen::Folder => self.draw_folder(frame, body),
            Screen::Computer => self.draw_computer(frame, body),
            Screen::Review => self.draw_review(frame, body),
            Screen::Running => self.draw_running(frame, body),
            Screen::Preview | Screen::Result => self.draw_result(frame, body),
        }
        let help = match self.screen {
            Screen::Home => "↑ ↓ Choose   Enter Continue   q Quit",
            Screen::Folder => "↑↓ Browse   Enter Open / Select   p Type path   h Hidden   Esc Back",
            Screen::Computer => "↑↓ Choose   Enter Select   m Manual address   Esc Back",
            Screen::Review if self.action == Action::Receive => {
                "Enter Start   s Sync access   p Port   t Token   b Bind   Esc Back"
            }
            Screen::Review => "Enter Continue   e Exclusions   p Port   t Token   Esc Back",
            Screen::Running => "Esc Stop   d Details",
            Screen::Preview => {
                "Enter Apply   l Keep local conflicts   r Keep remote   s Skip conflicts   Esc Back"
            }
            Screen::Result => "Enter Review / Retry   h Home   d Details   q Quit",
        };
        frame.render_widget(
            Paragraph::new(help).style(Style::default().fg(Color::Cyan)),
            sections[3],
        );
        if let Some(error) = &self.error
            && self.screen != Screen::Result
        {
            let rect = Rect::new(
                body.x + 1,
                body.y + body.height.saturating_sub(2),
                body.width.saturating_sub(2),
                2,
            );
            frame.render_widget(
                Paragraph::new(error.as_str())
                    .style(Style::default().fg(Color::Red))
                    .wrap(Wrap { trim: true }),
                rect,
            );
        }
        if let Some((kind, input)) = &self.edit {
            let rect = modal(area, 8);
            frame.render_widget(Clear, rect);
            let (title, hint) = match kind {
                Edit::Path => ("Enter a path", "Absolute paths and ~/ are supported."),
                Edit::Host => (
                    "Computer address",
                    "IP or host name, optionally :port. IPv6: [address]:port",
                ),
                Edit::Port => ("Port", "Use the same port on both computers."),
                Edit::Token => (
                    "Shared token",
                    "Optional. Enter the same token on both computers.",
                ),
                Edit::Excludes => (
                    "Exclude patterns",
                    "Separate patterns with commas, for example: .git, target/**",
                ),
                Edit::Bind => (
                    "Listen address",
                    ":: listens on all IPv4 and IPv6 interfaces.",
                ),
            };
            let displayed = if matches!(kind, Edit::Token) {
                "•".repeat(input.text.chars().count())
            } else {
                input.text.clone()
            };
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(hint),
                    Line::from(""),
                    Line::styled(format!("> {displayed}"), Style::default().fg(Color::Cyan)),
                    Line::from(""),
                    Line::from(
                        self.error
                            .as_deref()
                            .unwrap_or("Enter Save   Esc Cancel   Ctrl+U Clear"),
                    ),
                ])
                .wrap(Wrap { trim: false })
                .block(panel(title)),
                rect,
            );
        }
        if let Some((prompt, _)) = &self.trust {
            let rect = modal(area, 11);
            frame.render_widget(Clear, rect);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        if prompt.changed {
                            "This computer’s identity has changed"
                        } else {
                            "Connect to a new computer"
                        },
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::from(prompt.endpoint.clone()),
                    Line::from(""),
                    Line::from("Compare this code with the receiving screen:"),
                    Line::styled(
                        prompt.sas.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::from(""),
                    Line::from("y  Codes match — trust this computer"),
                    Line::from("n  Cancel connection"),
                ])
                .block(panel("Verify computer"))
                .wrap(Wrap { trim: true }),
                rect,
            );
        }
    }
    fn draw_home(&self, frame: &mut Frame<'_>, area: Rect) {
        let rows = Layout::vertical([Constraint::Min(6), Constraint::Length(3)]).split(area);
        let actions = [
            (
                "Send a copy",
                "Send a file or folder. Existing copies stay safe.",
            ),
            (
                "Receive here",
                "Choose where files arrive. Stay ready for transfers.",
            ),
            (
                "Sync one way",
                "Update the other computer using only changed data.",
            ),
            (
                "Sync both ways",
                "Bring changes from both sides. Review conflicts first.",
            ),
        ];
        let mut items = actions
            .iter()
            .map(|(title, _)| ListItem::new(*title))
            .collect::<Vec<_>>();
        if let Some(recent) = &self.recent {
            items.push(ListItem::new(format!(
                "Repeat last: {}",
                recent.action.title()
            )));
        }
        render_list(frame, rows[0], items, self.selection, "Choose an action");
        let description = if self.selection < actions.len() {
            actions[self.selection].1.to_owned()
        } else {
            self.recent
                .as_ref()
                .map(|recent| recent.path.display().to_string())
                .unwrap_or_default()
        };
        frame.render_widget(
            Paragraph::new(description)
                .style(dim())
                .wrap(Wrap { trim: true }),
            rows[1],
        );
    }
    fn draw_folder(&self, frame: &mut Frame<'_>, area: Rect) {
        let rows = Layout::vertical([Constraint::Length(2), Constraint::Min(5)]).split(area);
        frame.render_widget(
            Paragraph::new(sanitize_peer_text(
                &self.folder.directory.display().to_string(),
            ))
            .style(dim())
            .wrap(Wrap { trim: false }),
            rows[0],
        );
        let mut items = vec![
            ListItem::new("Use this folder"),
            ListItem::new("../  Parent folder"),
        ];
        items.extend(self.folder.entries.iter().map(|path| {
            let name = sanitize_peer_text(&path.file_name().unwrap_or_default().to_string_lossy());
            ListItem::new(format!(
                "{} {name}{}",
                if path.is_dir() { "▸" } else { " " },
                if path.is_dir() { "/" } else { "" }
            ))
        }));
        render_list(
            frame,
            rows[1],
            items,
            self.folder.selected,
            if self.action == Action::Receive {
                "Where should received files go?"
            } else {
                "Choose a folder or open a file"
            },
        );
    }
    fn draw_computer(&self, frame: &mut Frame<'_>, area: Rect) {
        let rows = Layout::vertical([Constraint::Length(5), Constraint::Min(4)]).split(area);
        let message = if self.discovery_error.is_some() {
            "Nearby discovery is unavailable. Use Manual address below."
        } else {
            "On the other computer, open XFER and choose Receive.\nFor sync, enable Sync access on its review screen.\nNearby computers appear here automatically."
        };
        frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: true }), rows[0]);
        let mut items = self
            .peers
            .iter()
            .map(|(peer, _)| ListItem::new(format!("{}   {}", peer.name, peer.address)))
            .collect::<Vec<_>>();
        items.push(ListItem::new(if self.host.is_empty() {
            "Enter address manually".into()
        } else {
            format!("Enter address manually  ·  last: {}", self.host)
        }));
        render_list(
            frame,
            rows[1],
            items,
            self.selection,
            "Choose the other computer",
        );
    }
    fn draw_review(&self, frame: &mut Frame<'_>, area: Rect) {
        let mut lines = vec![
            Line::styled(self.action.title(), accent()),
            Line::from(""),
            Line::from(format!("Folder   {}", self.path.display())),
        ];
        if self.action == Action::Receive {
            lines.extend([
                Line::from(format!("Port     {}", self.port)),
                Line::from(""),
                Line::from(if self.allow_sync {
                    "Sync access ON — sync directly into the selected folder."
                } else {
                    "Copies only. Press s to allow folder syncs."
                }),
                Line::styled("Keep this screen open on the receiving computer.", dim()),
                Line::from(""),
                Line::styled("[ Enter · Start receiving ]", accent()),
            ]);
        } else {
            lines.extend([
                Line::from(format!("Computer {}:{}", self.host, self.port)),
                Line::from(""),
                Line::from(match self.action {
                    Action::Copy => "Creates a separate copy; existing files are not replaced.",
                    Action::Sync => {
                        "This folder updates its matching folder on the other computer."
                    }
                    _ => "Changes go both ways. Conflicting edits are preserved.",
                }),
                Line::from(if self.action.syncing() {
                    "Destination-only files stay. No deletions are propagated."
                } else {
                    ""
                }),
                Line::from(format!(
                    "Exclusions: {}",
                    if self.excludes.is_empty() {
                        "none".into()
                    } else {
                        self.excludes.join(", ")
                    }
                )),
                Line::from(""),
                Line::styled(
                    if self.action.syncing() {
                        "[ Enter · Compare before syncing ]"
                    } else {
                        "[ Enter · Send copy ]"
                    },
                    accent(),
                ),
            ]);
        }
        lines.push(Line::styled(
            if self.token.is_empty() {
                "Encrypted connection · Verify a new computer once"
            } else {
                "Encrypted connection · Shared token configured"
            },
            dim(),
        ));
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(panel("Review")),
            area,
        );
    }
    fn draw_running(&self, frame: &mut Frame<'_>, area: Rect) {
        let rows = Layout::vertical([
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Min(3),
        ])
        .split(area);
        let mut lines = vec![Line::styled(
            if self.action == Action::Receive && self.progress.is_none() {
                "Ready for another computer"
            } else {
                "Working…"
            },
            accent(),
        )];
        if self.action == Action::Receive {
            lines.push(Line::from(format!(
                "{} {}",
                if self.allow_sync {
                    "Syncing into"
                } else {
                    "Saving inside"
                },
                self.path.display()
            )));
            lines.push(Line::from(format!(
                "Connect to {}",
                self.addresses
                    .iter()
                    .take(2)
                    .map(|ip| format!("{ip}:{}", self.port))
                    .collect::<Vec<_>>()
                    .join(" or ")
            )));
            if let Some(sas) = &self.sas {
                lines.push(Line::styled(format!("Security code: {sas}"), accent()));
            }
        } else {
            lines.push(Line::from(format!(
                "{} → {}",
                self.path.display(),
                self.host
            )));
            lines.push(Line::from(
                self.logs.back().map_or("Preparing…", String::as_str),
            ));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), rows[0]);
        if let Some(progress) = &self.progress {
            let ratio = if progress.total == 0 {
                0.0
            } else {
                (progress.transferred as f64 / progress.total as f64).clamp(0.0, 1.0)
            };
            frame.render_widget(
                Gauge::default()
                    .ratio(ratio)
                    .gauge_style(Style::default().fg(Color::Cyan))
                    .label(format!(
                        "{} · {} / {} files",
                        progress.phase, progress.files_done, progress.files_total
                    )),
                rows[1],
            );
        } else {
            frame.render_widget(
                Paragraph::new(if self.action == Action::Receive {
                    "Listening · stays ready between previews and syncs"
                } else {
                    "Comparing and verifying data…"
                })
                .style(dim()),
                rows[1],
            );
        }
        if self.details {
            self.draw_logs(frame, rows[2]);
        } else if let Some(summary) = &self.summary {
            frame.render_widget(
                Paragraph::new(format!(
                    "Last session: {}\n{}",
                    if summary.preview {
                        "Preview completed — no files changed.\nPress Enter on the sending computer to apply the sync."
                    } else {
                        "Files saved"
                    },
                    summary.destination.display()
                ))
                .wrap(Wrap { trim: true }),
                rows[2],
            );
        }
    }
    fn draw_result(&self, frame: &mut Frame<'_>, area: Rect) {
        if self.details {
            self.draw_logs(frame, area);
            return;
        }
        let mut lines = Vec::new();
        if let Some(error) = &self.error {
            lines.push(Line::styled(
                if error.contains("cancelled") {
                    "Stopped"
                } else {
                    "Could not finish"
                },
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::from(""));
            lines.push(Line::from(error.clone()));
            lines.push(Line::from(""));
            lines.push(Line::from(
                "Enter to review settings and retry, or h for Home.",
            ));
        } else if let Some(summary) = &self.summary {
            lines.push(Line::styled(
                if summary.preview {
                    "Review the changes"
                } else if summary.conflicts.is_empty() {
                    "All done"
                } else {
                    "Finished with conflicts to resolve"
                },
                accent(),
            ));
            lines.push(Line::from(""));
            if let Some(stats) = summary.sync_stats {
                lines.push(Line::from(format!(
                    "{} file(s) {}    {} unchanged",
                    stats.changed_files,
                    if summary.preview {
                        "to update"
                    } else {
                        "updated"
                    },
                    stats.unchanged_files
                )));
                lines.push(Line::from(format!(
                    "{} {}    {} reused",
                    human_bytes(stats.sent_bytes),
                    if summary.preview { "to send" } else { "sent" },
                    human_bytes(stats.reused_bytes)
                )));
            } else {
                lines.push(Line::from(format!(
                    "{} copied across {} file(s)",
                    human_bytes(summary.total_bytes),
                    summary.file_count
                )));
            }
            lines.push(Line::from(format!(
                "Destination: {}",
                summary.destination.display()
            )));
            lines.push(Line::from(""));
            if !summary.conflicts.is_empty() {
                lines.push(Line::styled(
                    format!(
                        "{} conflict(s) — both versions preserved",
                        summary.conflicts.len()
                    ),
                    Style::default().fg(Color::Yellow),
                ));
                for conflict in summary.conflicts.iter().take(4) {
                    lines.push(Line::from(format!("  {conflict}")));
                }
                lines.push(Line::from(
                    "l Keep local conflicts · r Keep remote · s Skip conflicts",
                ));
            }
            if summary.preview {
                if self.conflict_policy != crate::transfer::ConflictPolicy::Preserve {
                    lines.push(Line::styled(
                        match self.conflict_policy {
                            crate::transfer::ConflictPolicy::PreferLocal => {
                                "Conflicts: this computer’s version will replace the other."
                            }
                            _ => "Conflicts: the other computer’s version will replace this one.",
                        },
                        Style::default().fg(Color::Yellow),
                    ));
                }
                lines.push(Line::from(""));
                lines.push(Line::styled(
                    "[ Enter · Apply safe changes ]   Esc · Go back",
                    accent(),
                ));
            } else {
                lines.push(Line::styled(
                    "Enter · Review / Repeat    h · Home",
                    accent(),
                ));
            }
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(panel(if self.screen == Screen::Preview {
                    "Sync preview"
                } else {
                    "Result"
                })),
            area,
        );
    }
    fn draw_logs(&self, frame: &mut Frame<'_>, area: Rect) {
        let lines = self
            .logs
            .iter()
            .rev()
            .take(area.height.saturating_sub(2) as usize)
            .rev()
            .map(|line| Line::from(line.as_str()))
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(panel("Details")),
            area,
        );
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.control.cancel();
    }
}
fn preferences(config: Option<PathBuf>) -> Result<LockedJsonStore<Recent>> {
    Ok(LockedJsonStore::new(
        SecureDir::discover("xfer", config)?,
        "recent-workflow.json",
    ))
}
fn expand_path(text: &str) -> Result<PathBuf> {
    let path = text.trim();
    if path == "~" {
        return dirs::home_dir()
            .ok_or_else(|| XferError::invalid_input("Home directory is unavailable."));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return Ok(dirs::home_dir()
            .ok_or_else(|| XferError::invalid_input("Home directory is unavailable."))?
            .join(rest));
    }
    Ok(PathBuf::from(path))
}
fn parse_endpoint(text: &str, default_port: u16) -> Result<(String, u16)> {
    if let Ok(address) = text.parse::<SocketAddr>() {
        if address.port() == 0 {
            return Err(XferError::invalid_input("Port must not be zero."));
        }
        return Ok((address.ip().to_string(), address.port()));
    }
    if text.parse::<std::net::IpAddr>().is_ok() {
        return Ok((text.into(), default_port));
    }
    let (host, port) = if let Some((host, port)) = text.rsplit_once(':') {
        (
            host,
            port.parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| XferError::invalid_input("Use host:port, or [IPv6]:port."))?,
        )
    } else {
        (text, default_port)
    };
    if host.is_empty()
        || host
            .chars()
            .any(|c| c.is_whitespace() || "/\\[]:".contains(c))
    {
        return Err(XferError::invalid_input("Enter a host name or IP address."));
    }
    Ok((host.into(), port))
}
fn accent() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}
fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}
fn panel(title: &str) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(dim())
}
fn modal(area: Rect, height: u16) -> Rect {
    let width = area.width.saturating_sub(6).min(76);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}
fn render_list(
    frame: &mut Frame<'_>,
    area: Rect,
    items: Vec<ListItem<'_>>,
    selected: usize,
    title: &str,
) {
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(panel(title))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(" › "),
        area,
        &mut state,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    #[test]
    fn unicode_input_supports_cursor_editing_and_paste() {
        let mut input = Input::new("aéz".into());
        input.key(key(KeyCode::Left));
        input.key(key(KeyCode::Backspace));
        input.insert("🙂");
        assert_eq!(input.text, "a🙂z");
        input.key(key(KeyCode::Delete));
        assert_eq!(input.text, "a🙂");
    }
    #[test]
    fn endpoints_accept_ipv4_ipv6_and_host_ports() {
        assert_eq!(
            parse_endpoint("computer:9100", 9000).unwrap(),
            ("computer".into(), 9100)
        );
        assert_eq!(
            parse_endpoint("[::1]:9100", 9000).unwrap(),
            ("::1".into(), 9100)
        );
        assert_eq!(parse_endpoint("::1", 9000).unwrap(), ("::1".into(), 9000));
        assert!(parse_endpoint("host:0", 9000).is_err());
    }
    #[test]
    fn workflow_uses_enter_and_can_go_back() {
        let config = tempfile::tempdir().unwrap();
        let mut app = App::new(Some(config.path().into()));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Enter));
        assert_eq!(app.action, Action::Sync);
        assert_eq!(app.screen, Screen::Folder);
        app.key(key(KeyCode::Enter));
        assert_eq!(app.screen, Screen::Computer);
        app.key(key(KeyCode::Esc));
        assert_eq!(app.screen, Screen::Folder);
    }
    #[test]
    fn cancellation_rejects_pending_trust() {
        let config = tempfile::tempdir().unwrap();
        let mut app = App::new(Some(config.path().into()));
        let (tx, rx) = mpsc::sync_channel(1);
        app.screen = Screen::Running;
        app.trust = Some((
            TrustPrompt {
                endpoint: "peer".into(),
                fingerprint: "hash".into(),
                sas: "code".into(),
                changed: false,
            },
            tx,
        ));
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!rx.try_recv().unwrap());
        assert!(app.control.check().is_err());
    }
    #[test]
    fn all_screens_render_at_standard_and_small_terminal_sizes() {
        let config = tempfile::tempdir().unwrap();
        let mut app = App::new(Some(config.path().into()));
        for size in [(80, 24), (60, 18), (120, 40), (30, 8)] {
            let backend = ratatui::backend::TestBackend::new(size.0, size.1);
            let mut terminal = Terminal::new(backend).unwrap();
            for screen in [
                Screen::Home,
                Screen::Folder,
                Screen::Computer,
                Screen::Review,
                Screen::Running,
                Screen::Preview,
                Screen::Result,
            ] {
                app.screen = screen;
                terminal.draw(|frame| app.draw(frame)).unwrap();
            }
        }
    }
    #[test]
    fn send_and_receive_are_consecutive_list_items() {
        let config = tempfile::tempdir().unwrap();
        let mut app = App::new(Some(config.path().into()));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let lines = (0..24)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let send_row = lines
            .iter()
            .position(|line| line.contains("Send a copy"))
            .unwrap();
        let receive_row = lines
            .iter()
            .position(|line| line.contains("Receive here"))
            .unwrap();
        assert_eq!(receive_row, send_row + 1);
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Enter));
        assert_eq!(app.action, Action::Receive);
    }
}

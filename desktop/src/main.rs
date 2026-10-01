#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod assets;
mod input;
mod menus;
mod theme;
mod ui;
use gpui::{
    App, Application, Bounds, Context, Div, ElementId, Entity, FocusHandle, Focusable, KeyBinding,
    PathPromptOptions, SharedString, Stateful, Subscription, TitlebarOptions, Window, WindowBounds,
    WindowOptions, actions, div, img, prelude::*, px, relative, size, uniform_list,
};
use input::TextInput;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};
use xfer::{
    config::{Paths, TrustStore},
    discovery::{Browser, DiscoveredPeer, PEER_TTL},
    reporter::{Progress, TrustPrompt},
    transfer::{ConflictPolicy, ReceiveOptions, TransferSummary, human_bytes},
    workflow::{self, Action, Job, Recent, TransferRates, WorkerEvent},
};

actions!(desktop, [Submit, Cancel, NextField, PreviousField, Quit]);
const RELEASES: &str = "https://github.com/cdenihan/XFER/releases/latest";
const LOG_LIMIT: usize = 256;

#[derive(Clone, Copy, PartialEq)]
enum View {
    Workflow,
    Settings,
    Trusted,
    Transfers,
}
#[derive(Clone, Copy)]
enum Toggle {
    Gitignore,
    Secure,
    SyncAccess,
    Overwrite,
    FollowLinks,
    Discovery,
}
enum Message {
    Loaded(std::result::Result<(Recent, Vec<(String, String)>), String>),
    Peers(Vec<DiscoveredPeer>),
    Notice(String),
    Settings(std::result::Result<Vec<(String, String)>, String>),
}
fn remembered(config: Option<PathBuf>) -> xfer::error::Result<Vec<(String, String)>> {
    let store = TrustStore::load(&Paths::discover(config)?)?;
    Ok(store
        .iter()
        .map(|(endpoint, peer)| {
            (
                endpoint.to_string(),
                xfer::crypto::display_fingerprint(&peer.fingerprint),
            )
        })
        .collect())
}
fn launch_services(config: Option<PathBuf>, tx: SyncSender<Message>, stop: Arc<AtomicBool>) {
    thread::spawn(move || {
        let loaded = (|| {
            Ok((
                workflow::preferences(config.clone())?.load()?,
                remembered(config)?,
            ))
        })();
        let loaded: xfer::error::Result<(Recent, Vec<(String, String)>)> = loaded;
        if tx
            .send(Message::Loaded(loaded.map_err(|e| e.to_string())))
            .is_err()
        {
            return;
        }
        let browser = match Browser::start() {
            Ok(browser) => browser,
            Err(error) => {
                let _ = tx.try_send(Message::Notice(format!(
                    "Discovery unavailable: {error}. Enter an address manually."
                )));
                return;
            }
        };
        let mut peers: Vec<(DiscoveredPeer, Instant)> = vec![];
        let mut previous = vec![];
        while !stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            for _ in 0..256 {
                let Some(peer) = browser.try_recv() else {
                    break;
                };
                if let Some(found) = peers.iter_mut().find(|(p, _)| p.address == peer.address) {
                    *found = (peer, now);
                } else if peers.len() < 256 {
                    peers.push((peer, now));
                }
            }
            peers.retain(|(_, seen)| now.duration_since(*seen) < PEER_TTL);
            let mut current = peers.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>();
            current.sort_by_key(|p| (p.name.clone(), p.address));
            if current != previous && tx.try_send(Message::Peers(current.clone())).is_ok() {
                previous = current;
            }
            thread::sleep(Duration::from_millis(100));
        }
        // Browser's join occurs on this service thread, never the UI thread.
    });
}

struct Desktop {
    config: Option<PathBuf>,
    view: View,
    theme: theme::Theme,
    action: Action,
    inputs: Vec<Entity<TextInput>>,
    focus: FocusHandle,
    logo: Arc<gpui::Image>,
    secure: bool,
    gitignore: bool,
    allow_sync: bool,
    overwrite: bool,
    follow_links: bool,
    discoverable: bool,
    policy: ConflictPolicy,
    operation: u64,
    revision: u64,
    preview_revision: Option<u64>,
    job: Option<Job>,
    retiring: Option<Job>,
    job_revision: Option<u64>,
    trust: Option<(TrustPrompt, SyncSender<bool>)>,
    summary: Option<TransferSummary>,
    progress: Option<Progress>,
    rates: TransferRates,
    logs: VecDeque<String>,
    error: Option<String>,
    sas: Option<(String, String)>,
    receiver_addresses: Option<String>,
    details: bool,
    advanced: bool,
    confirm_clear: bool,
    peers: Vec<DiscoveredPeer>,
    known: Vec<(String, String)>,
    ready: bool,
    closing: bool,
    messages: Receiver<Message>,
    tx: SyncSender<Message>,
    stop: Arc<AtomicBool>,
    _subscriptions: Vec<Subscription>,
}
impl Desktop {
    fn new(config: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let placeholders = [
            "Choose a file or folder",
            "Receiver address or hostname",
            "9000",
            "Shared token (optional)",
            "Exclusion globs, separated by ;",
            "::",
        ];
        let inputs = placeholders
            .iter()
            .enumerate()
            .map(|(i, p)| cx.new(|cx| TextInput::new(p, i == 3, cx)))
            .collect::<Vec<_>>();
        inputs[2].update(cx, |i, cx| i.set("9000".into(), cx));
        inputs[5].update(cx, |i, cx| i.set("::".into(), cx));
        let mut subscriptions = vec![];
        for input in &inputs {
            subscriptions.push(
                cx.subscribe(input, |this, _, _: &input::ContentChanged, cx| {
                    this.revision = this.revision.wrapping_add(1);
                    this.preview_revision = None;
                    cx.notify();
                }),
            );
        }
        let (tx, messages) = mpsc::sync_channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        if !cfg!(test) {
            launch_services(config.clone(), tx.clone(), Arc::clone(&stop));
        }
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if this.update(cx, |this, cx| this.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        subscriptions.push(cx.on_app_quit(|this, cx| {
            this.stop.store(true, Ordering::Relaxed);
            if let Some((_, reply)) = this.trust.take() {
                let _ = reply.try_send(false);
            }
            let job = this.job.take().or_else(|| this.retiring.take());
            if let Some(job) = &job {
                job.control.cancel();
            }
            let executor = cx.background_executor().clone();
            async move {
                // GPUI bounds native OS quit callbacks to its shutdown timeout.
                if let Some(job) = job {
                    while !job.is_finished() {
                        while job.try_recv().is_some() {}
                        executor.timer(Duration::from_millis(10)).await;
                    }
                }
            }
        }));
        Self {
            config,
            view: View::Workflow,
            theme: theme::Theme::new(cx.window_appearance()),
            action: Action::Copy,
            inputs,
            focus: cx.focus_handle(),
            logo: Arc::new(gpui::Image::from_bytes(
                gpui::ImageFormat::Png,
                include_bytes!("../assets/xfer.png").to_vec(),
            )),
            secure: true,
            gitignore: false,
            allow_sync: false,
            overwrite: false,
            follow_links: false,
            discoverable: true,
            policy: ConflictPolicy::Preserve,
            operation: 0,
            revision: 0,
            preview_revision: None,
            job: None,
            retiring: None,
            job_revision: None,
            trust: None,
            summary: None,
            progress: None,
            rates: TransferRates::default(),
            logs: VecDeque::new(),
            error: None,
            sas: None,
            receiver_addresses: None,
            details: false,
            advanced: false,
            confirm_clear: false,
            peers: vec![],
            known: vec![],
            ready: false,
            closing: false,
            messages,
            tx,
            stop,
            _subscriptions: subscriptions,
        }
    }
    fn log(&mut self, text: String) {
        self.logs.push_back(
            text.chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .take(2048)
                .collect(),
        );
        if self.logs.len() > LOG_LIMIT {
            self.logs.pop_front();
        }
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        if let Some(job) = &self.retiring {
            while job.try_recv().is_some() {}
            if job.is_finished() {
                self.retiring = None;
                changed = true;
            }
        }
        while let Ok(message) = self.messages.try_recv() {
            changed = true;
            match message {
                Message::Loaded(Ok((recent, known))) => {
                    self.action = recent.action;
                    self.gitignore = recent.gitignore;
                    self.known = known;
                    for (index, text) in [
                        (0, recent.path.to_string_lossy().into_owned()),
                        (1, recent.host),
                        (2, recent.port.to_string()),
                    ] {
                        self.inputs[index].update(cx, |i, cx| i.set(text, cx));
                    }
                    self.ready = true;
                }
                Message::Loaded(Err(error)) => {
                    self.ready = true;
                    self.error = Some(error);
                }
                Message::Peers(peers) => self.peers = peers,
                Message::Notice(text) => self.log(text),
                Message::Settings(result) => match result {
                    Ok(known) => self.known = known,
                    Err(error) => self.error = Some(error),
                },
            }
        }
        let mut events = vec![];
        if let Some(job) = &self.job {
            for _ in 0..64 {
                if let Some(event) = job.try_recv() {
                    events.push(event);
                } else {
                    break;
                }
            }
            if let Some(progress) = job.take_progress() {
                self.progress = Some(progress);
                changed = true;
            }
        }
        for envelope in events {
            if envelope.operation != self.operation {
                continue;
            }
            changed = true;
            match envelope.event {
                WorkerEvent::Status(text) => {
                    if let Some(addresses) = text.strip_prefix("receiver addresses: ") {
                        self.receiver_addresses = Some(addresses.to_string());
                    }
                    self.log(text);
                }
                WorkerEvent::Sas(sas, fingerprint) => self.sas = Some((sas, fingerprint)),
                WorkerEvent::Trust(prompt, reply) => self.trust = Some((prompt, reply)),
                WorkerEvent::Received(summary) => {
                    self.version_notice(&summary);
                    self.log(format!(
                        "Received {} across {} files",
                        human_bytes(summary.total_bytes),
                        summary.file_count
                    ));
                    self.summary = Some(summary);
                    self.progress = None;
                    self.rates.finish();
                }
                WorkerEvent::Finished(result) => self.finish(result),
            }
        }
        if self.job.is_some()
            && let Some(progress) = &self.progress
        {
            self.rates.sample(progress, Instant::now());
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }
    fn finish(&mut self, result: xfer::error::Result<TransferSummary>) {
        self.job = None;
        self.trust = None;
        let revision = self.job_revision.take();
        match result {
            Ok(summary) => {
                self.version_notice(&summary);
                if summary.preview {
                    self.preview_revision = revision.filter(|revision| *revision == self.revision);
                    if self.preview_revision.is_none() {
                        self.error = Some(
                            "Inputs changed during preview. Preview again before applying.".into(),
                        );
                    }
                }
                self.summary = Some(summary);
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        if let Some(progress) = &self.progress {
            self.rates.sample(progress, Instant::now());
        }
        self.rates.finish();
    }
    fn version_notice(&mut self, summary: &TransferSummary) {
        if summary.peer_version.as_deref() != Some(xfer::VERSION) {
            self.log(format!(
                "Peer version: {}. Local version: {}. Align releases before the next transfer.",
                summary.peer_version.as_deref().unwrap_or("unknown"),
                xfer::VERSION
            ));
        }
    }
    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, reply)) = self.trust.take() {
            let _ = reply.try_send(false);
        }
        if let Some(job) = self.job.take() {
            job.control.cancel();
            self.retiring = Some(job);
            self.log("Cancelled. Already completed files are retained.".into());
        }
        self.operation = self.operation.wrapping_add(1);
        self.job_revision = None;
        self.preview_revision = None;
        self.progress = None;
        self.sas = None;
        self.rates.finish();
        cx.notify();
    }
    fn shutdown(&mut self, cx: &mut Context<Self>) {
        if self.closing {
            return;
        }
        self.closing = true;
        self.stop.store(true, Ordering::Relaxed);
        if let Some((_, reply)) = self.trust.take() {
            let _ = reply.try_send(false);
        }
        let job = self.job.take().or_else(|| self.retiring.take());
        if let Some(job) = &job {
            job.control.cancel();
        }
        // Let cancellation release sockets and staged files without blocking the UI.
        // DNS and filesystem locks can outlive this bounded application shutdown.
        cx.spawn(async move |_, cx| {
            let deadline = Instant::now() + Duration::from_secs(2);
            if let Some(job) = job {
                while !job.is_finished() && Instant::now() < deadline {
                    while job.try_recv().is_some() {}
                    cx.background_executor()
                        .timer(Duration::from_millis(10))
                        .await;
                }
            }
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    }
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        self.start(false, cx);
    }
    fn start(&mut self, force_preview: bool, cx: &mut Context<Self>) {
        if self.closing
            || !self.ready
            || self.job.is_some()
            || self.retiring.is_some()
            || self.view != View::Workflow
        {
            return;
        }
        let values = self
            .inputs
            .iter()
            .map(|i| i.read(cx).value())
            .collect::<Vec<_>>();
        let port = match values[2].trim().parse::<u16>() {
            Ok(port) if port != 0 => port,
            _ => {
                self.error = Some("Port must be between 1 and 65535.".into());
                cx.notify();
                return;
            }
        };
        if values[0].trim().is_empty()
            || (self.action != Action::Receive && values[1].trim().is_empty())
        {
            self.error = Some("Choose a path and a receiver address before starting.".into());
            cx.notify();
            return;
        }
        let path = match workflow::expand_path(&values[0]) {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        if !self.secure && !values[3].is_empty() {
            self.error = Some("Shared tokens require secure mode.".into());
            cx.notify();
            return;
        }
        let recent = Recent {
            action: self.action,
            path,
            host: values[1].trim().into(),
            port,
            gitignore: self.gitignore,
        };
        let mut sender = workflow::send_options(&recent, self.config.clone());
        sender.preview = self.action.syncing()
            && (force_preview || self.preview_revision != Some(self.revision));
        sender.conflict_policy = self.policy;
        sender.secure = self.secure;
        sender.follow_links = self.follow_links && self.action == Action::Copy;
        sender.token = (!values[3].is_empty()).then(|| values[3].clone());
        sender.excludes = values[4]
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let receiver = ReceiveOptions {
            allow_sync: self.allow_sync,
            sync_into: true,
            bind: values[5].trim().into(),
            port,
            output: recent.path.clone(),
            overwrite: self.overwrite,
            discoverable: self.discoverable,
            secure: self.secure,
            token: sender.token.clone(),
            config_dir: self.config.clone(),
        };
        self.operation = self.operation.wrapping_add(1);
        self.error = None;
        self.summary = None;
        self.progress = None;
        self.sas = None;
        self.receiver_addresses = None;
        self.trust = None;
        self.rates = TransferRates::default();
        self.logs.clear();
        match Job::start(self.operation, recent, sender, receiver) {
            Ok(job) => {
                self.job_revision = Some(self.revision);
                self.job = Some(job);
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        cx.notify();
    }
    fn choose(&mut self, folder: bool, cx: &mut Context<Self>) {
        if self.job.is_some() || self.retiring.is_some() {
            return;
        }
        let request = cx.prompt_for_paths(PathPromptOptions {
            files: !folder,
            directories: folder,
            multiple: false,
            prompt: Some("Choose content for XFER".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = request.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.first() {
                            this.inputs[0]
                                .update(cx, |i, cx| i.set(path.to_string_lossy().into_owned(), cx));
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => this.error = Some(error.to_string()),
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn answer(&mut self, accepted: bool, cx: &mut Context<Self>) {
        if let Some((_, reply)) = self.trust.take() {
            let _ = reply.try_send(accepted);
        }
        cx.notify();
    }
    fn next_field(&mut self, _: &NextField, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_field(1, window, cx);
    }
    fn previous_field(&mut self, _: &PreviousField, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_field(-1, window, cx);
    }
    fn focus_field(&self, direction: isize, window: &mut Window, cx: &App) {
        if self.view != View::Workflow {
            return;
        }
        let mut visible = if self.action == Action::Receive {
            vec![0]
        } else {
            vec![0, 1, 2]
        };
        if self.advanced {
            if self.action == Action::Receive {
                visible.push(2);
            }
            visible.push(3);
            visible.push(if self.action == Action::Receive { 5 } else { 4 });
        }
        let current = visible
            .iter()
            .position(|i| self.inputs[*i].focus_handle(cx).is_focused(window));
        let next = current.map_or(0, |i| {
            (i as isize + direction).rem_euclid(visible.len() as isize) as usize
        });
        window.focus(&self.inputs[visible[next]].focus_handle(cx));
    }
    fn toggle(&mut self, toggle: Toggle, cx: &mut Context<Self>) {
        if self.job.is_some() || self.retiring.is_some() {
            return;
        }
        match toggle {
            Toggle::Gitignore => self.gitignore = !self.gitignore,
            Toggle::Secure => self.secure = !self.secure,
            Toggle::SyncAccess => self.allow_sync = !self.allow_sync,
            Toggle::Overwrite => self.overwrite = !self.overwrite,
            Toggle::FollowLinks => self.follow_links = !self.follow_links,
            Toggle::Discovery => self.discoverable = !self.discoverable,
        }
        self.revision = self.revision.wrapping_add(1);
        self.preview_revision = None;
        cx.notify();
    }
    fn refresh_peers(&self) {
        let config = self.config.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Message::Settings(
                remembered(config).map_err(|e| e.to_string()),
            ));
        });
    }
    fn peer_update(&mut self, endpoint: Option<String>, cx: &mut Context<Self>) {
        let config = self.config.clone();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = (|| {
                let paths = Paths::discover(config.clone())?;
                TrustStore::update(&paths, |store| {
                    if let Some(endpoint) = endpoint {
                        store.remove(&endpoint);
                    } else {
                        store.clear();
                    }
                    Ok(())
                })?;
                remembered(config)
            })();
            let _ = tx.send(Message::Settings(
                result.map_err(|e: xfer::error::XferError| e.to_string()),
            ));
        });
        cx.notify();
    }
    fn card(&self, title: &str) -> Div {
        div()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(self.color(0x293747))
            .bg(self.color(0x151e29))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_color(self.color(0xb8c9dc))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title.to_string()),
            )
    }
    fn color(&self, value: u32) -> gpui::Rgba {
        self.theme.color(value)
    }
    fn button(&self, id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
        let id = id.into();
        let primary = id == ElementId::from("primary");
        let theme = self.theme;
        div()
            .id(id)
            .map(|mut button| {
                button.style().align_self = Some(gpui::AlignItems::FlexStart);
                button
            })
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(self.color(0x2b3a4d))
            .bg(self.color(0x1c2735))
            .text_color(self.color(0xe6edf5))
            .cursor_pointer()
            .hover(move |s| {
                s.bg(if primary {
                    theme.color(0x8bedda)
                } else {
                    theme.color(0x293a4d)
                })
            })
            .child(text.into())
    }
    fn field(&self, label: &str, index: usize) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .text_color(self.color(0x95a5b8))
                    .child(label.to_string()),
            )
            .child(self.inputs[index].clone())
    }
    fn checkbox(
        &self,
        label: &str,
        enabled: bool,
        toggle: Toggle,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.button(
            SharedString::from(label.to_string()),
            format!("{} {label}", if enabled { "✓" } else { "○" }),
        )
        .on_click(cx.listener(move |this, _, _, cx| this.toggle(toggle, cx)))
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some((_, reply)) = self.trust.take() {
            let _ = reply.try_send(false);
        }
        self.job.take();
        self.retiring.take();
    }
}
impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_ui(window, cx)
    }
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let mut config = std::env::var_os("XFER_CONFIG_DIR").map(PathBuf::from);
    while let Some(arg) = args.next() {
        if arg == "--version" {
            println!("xfer-desktop {}", xfer::VERSION);
            return;
        }
        if arg == "--help" {
            println!("xfer-desktop [--config-dir PATH]\nDesktop file transfer and sync.");
            return;
        }
        if arg == "--config-dir" {
            let Some(path) = args.next() else {
                eprintln!("--config-dir requires a path");
                std::process::exit(2);
            };
            config = Some(PathBuf::from(path));
        } else {
            eprintln!("Unknown argument: {}", arg.to_string_lossy());
            std::process::exit(2);
        }
    }
    let application = Application::new().with_assets(assets::Assets);
    application.on_reopen(|cx| {
        if let Some(window) = cx.windows().first() {
            let _ = window.update(cx, |_, window, _| window.activate_window());
        }
        cx.activate(true);
    });
    application.run(move |cx| {
        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.bind_keys([
            KeyBinding::new("enter", Submit, Some("Desktop")),
            KeyBinding::new("escape", Cancel, Some("Desktop")),
            KeyBinding::new("tab", NextField, Some("Desktop")),
            KeyBinding::new("shift-tab", PreviousField, Some("Desktop")),
            KeyBinding::new("backspace", input::Backspace, Some("TextInput")),
            KeyBinding::new("delete", input::Delete, Some("TextInput")),
            KeyBinding::new("left", input::Left, Some("TextInput")),
            KeyBinding::new("right", input::Right, Some("TextInput")),
            KeyBinding::new("shift-left", input::SelectLeft, Some("TextInput")),
            KeyBinding::new("shift-right", input::SelectRight, Some("TextInput")),
            KeyBinding::new("home", input::Home, Some("TextInput")),
            KeyBinding::new("end", input::End, Some("TextInput")),
            KeyBinding::new(
                &format!("{modifier}-a"),
                input::SelectAll,
                Some("TextInput"),
            ),
            KeyBinding::new(&format!("{modifier}-v"), input::Paste, Some("TextInput")),
            KeyBinding::new(&format!("{modifier}-c"), input::Copy, Some("TextInput")),
            KeyBinding::new(&format!("{modifier}-x"), input::Cut, Some("TextInput")),
            KeyBinding::new(&format!("{modifier}-q"), Quit, None),
        ]);
        cx.bind_keys([
            KeyBinding::new(&format!("{modifier}-,"), menus::Settings, None),
            KeyBinding::new(&format!("{modifier}-o"), menus::ChooseFile, None),
            KeyBinding::new(&format!("{modifier}-shift-o"), menus::ChooseFolder, None),
            KeyBinding::new(&format!("{modifier}-w"), menus::Close, None),
            KeyBinding::new(&format!("{modifier}-m"), menus::Minimize, None),
            KeyBinding::new(&format!("{modifier}-h"), menus::Hide, None),
            KeyBinding::new(&format!("{modifier}-alt-h"), menus::HideOthers, None),
            KeyBinding::new("ctrl-cmd-f", menus::Fullscreen, None),
        ]);
        menus::install(cx);
        let bounds = Bounds::centered(None, size(px(1320.), px(760.)), cx);
        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(480.), px(400.))),
                app_id: Some("com.cdenihan.xfer".into()),
                titlebar: Some(TitlebarOptions {
                    title: Some("XFER".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                let entity = cx.new(|cx| Desktop::new(config, cx));
                let quit = entity.downgrade();
                cx.on_action(move |_: &Quit, cx| {
                    let _ = quit.update(cx, |this, cx| this.shutdown(cx));
                });
                let appearance = entity.downgrade();
                let subscription = window.observe_window_appearance(move |_, cx| {
                    let _ = appearance.update(cx, |this, cx| {
                        for input in &this.inputs {
                            input.update(cx, |_, cx| cx.notify());
                        }
                        cx.notify();
                    });
                });
                entity.update(cx, |this, _| this._subscriptions.push(subscription));
                let weak = entity.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    let _ = weak.update(cx, |this, cx| this.shutdown(cx));
                    false
                });
                window.focus(&entity.read(cx).focus);
                entity
            },
        ) {
            eprintln!("Could not open XFER: {error}");
            cx.quit();
        }
        cx.activate(true);
    });
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    #[gpui::test]
    fn views_render_at_small_sizes_and_input_changes_invalidate_preview(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(|_, cx| Desktop::new(None, cx));
        for action in [Action::Copy, Action::Receive, Action::Sync, Action::TwoWay] {
            for advanced in [false, true] {
                view.update(cx, |this, cx| {
                    this.ready = true;
                    this.action = action;
                    this.advanced = advanced;
                    cx.notify();
                });
                cx.simulate_resize(size(px(480.), px(400.)));
                cx.run_until_parked();
            }
        }
        view.update(cx, |this, cx| {
            this.preview_revision = Some(this.revision);
            this.inputs[1].update(cx, |input, cx| input.set("different-peer".into(), cx));
        });
        cx.run_until_parked();
        view.update(cx, |this, cx| {
            assert!(this.preview_revision.is_none());
            this.view = View::Settings;
            cx.notify();
        });
        cx.run_until_parked();
    }
    fn preview_summary() -> TransferSummary {
        TransferSummary {
            sync_stats: None,
            preview: true,
            conflicts: vec![],
            destination: PathBuf::from("reviewed-folder"),
            file_count: 0,
            total_bytes: 0,
            peer: "127.0.0.1:9000".parse().unwrap(),
            peer_version: Some(xfer::VERSION.into()),
        }
    }
    #[gpui::test]
    fn focus_and_cursor_notifications_preserve_reviewed_preview(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| Desktop::new(None, cx));
        view.update(cx, |this, cx| {
            this.inputs[0].update(cx, |input, cx| input.set("folder".into(), cx));
        });
        cx.run_until_parked();
        let revision = view.update(cx, |this, _| {
            this.preview_revision = Some(this.revision);
            this.revision
        });
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.inputs[0].update(cx, |input, cx| {
                    window.focus(&input.focus_handle(cx));
                    input.set("folder".into(), cx);
                    cx.notify();
                });
            });
        });
        cx.run_until_parked();
        view.update(cx, |this, _| {
            assert_eq!(this.revision, revision);
            assert_eq!(this.preview_revision, Some(revision));
        });
    }
    #[gpui::test]
    fn preview_completion_cannot_approve_changed_inputs(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| Desktop::new(None, cx));
        view.update(cx, |this, cx| {
            this.action = Action::Sync;
            this.job_revision = Some(this.revision);
            this.inputs[0].update(cx, |input, cx| input.set("different-folder".into(), cx));
        });
        cx.run_until_parked();
        view.update(cx, |this, _| {
            this.finish(Ok(preview_summary()));
            assert!(this.preview_revision.is_none());
            assert!(this.error.as_deref().unwrap().contains("Inputs changed"));
            this.error = None;
            this.job_revision = Some(this.revision);
            this.finish(Ok(preview_summary()));
            assert_eq!(this.preview_revision, Some(this.revision));
        });
    }
    #[gpui::test]
    fn cancelled_receiver_blocks_retry_until_worker_exits(cx: &mut gpui::TestAppContext) {
        let config = std::env::temp_dir().join(format!(
            "xfer-gpui-cancel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let recent = Recent {
            action: Action::Receive,
            path: config.join("received"),
            port: 0,
            ..Recent::default()
        };
        let receiver = ReceiveOptions {
            allow_sync: false,
            sync_into: false,
            bind: "127.0.0.1".into(),
            port: 0,
            output: recent.path.clone(),
            overwrite: false,
            discoverable: false,
            secure: false,
            token: None,
            config_dir: Some(config.clone()),
        };
        let job = Job::start(
            7,
            recent.clone(),
            workflow::send_options(&recent, Some(config.clone())),
            receiver,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(event) = job.try_recv() {
                match event.event {
                    WorkerEvent::Status(text) if text.starts_with("listening on") => break,
                    WorkerEvent::Finished(result) => panic!("receiver failed to start: {result:?}"),
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        let (view, cx) = cx.add_window_view(|_, cx| Desktop::new(Some(config.clone()), cx));
        cx.update(|window, cx| {
            view.update(cx, move |this, cx| {
                this.ready = true;
                this.action = Action::Receive;
                this.inputs[0].update(cx, |input, cx| {
                    input.set(recent.path.display().to_string(), cx)
                });
                this.operation = 7;
                this.job = Some(job);
                this.cancel(&Cancel, window, cx);
                assert!(this.retiring.is_some());
                this.start(false, cx);
                assert!(this.job.is_none());
                assert_eq!(this.operation, 8);
            });
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while view.update(cx, |this, cx| {
            this.poll(cx);
            this.retiring.is_some()
        }) {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        std::fs::remove_dir_all(config).unwrap();
    }
    #[gpui::test]
    fn escape_rejects_pending_trust(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| Desktop::new(None, cx));
        let (reply, result) = mpsc::sync_channel(1);
        cx.update(|window, cx| {
            view.update(cx, |this, cx| {
                this.trust = Some((
                    TrustPrompt {
                        endpoint: "peer".into(),
                        fingerprint: "key".into(),
                        sas: "123-456-7890".into(),
                        changed: true,
                    },
                    reply,
                ));
                this.cancel(&Cancel, window, cx);
                assert!(this.trust.is_none());
            })
        });
        assert!(!result.recv_timeout(Duration::from_secs(1)).unwrap());
    }
}

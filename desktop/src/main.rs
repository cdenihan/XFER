#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod input;
use gpui::{
    App, Application, Bounds, Context, Div, ElementId, Entity, FocusHandle, Focusable, KeyBinding,
    PathPromptOptions, SharedString, Stateful, Subscription, TitlebarOptions, Window, WindowBounds,
    WindowOptions, actions, div, img, prelude::*, px, relative, rgb, size, uniform_list,
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
            subscriptions.push(cx.observe(input, |this, _, cx| {
                this.revision = this.revision.wrapping_add(1);
                this.preview_revision = None;
                cx.notify();
            }));
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
            let job = this.job.take();
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
                WorkerEvent::Finished(result) => {
                    self.job = None;
                    self.trust = None;
                    match result {
                        Ok(summary) => {
                            self.version_notice(&summary);
                            if summary.preview {
                                self.preview_revision = Some(self.revision);
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
            self.log("Cancelled. Already completed files are retained.".into());
        }
        self.operation = self.operation.wrapping_add(1);
        self.preview_revision = None;
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
        let job = self.job.take();
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
        if self.closing || !self.ready || self.job.is_some() || self.view != View::Workflow {
            return;
        }
        let values = self
            .inputs
            .iter()
            .map(|i| i.read(cx).value())
            .collect::<Vec<_>>();
        let port = match values[2].parse::<u16>() {
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
            Ok(job) => self.job = Some(job),
            Err(error) => self.error = Some(error.to_string()),
        }
        cx.notify();
    }
    fn choose(&mut self, folder: bool, cx: &mut Context<Self>) {
        if self.job.is_some() {
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
        if self.view == View::Settings {
            return;
        }
        let mut visible = if self.action == Action::Receive {
            vec![0]
        } else {
            vec![0, 1]
        };
        if self.advanced {
            visible.extend([2, 3]);
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
        if self.job.is_some() {
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
    fn card(title: &str) -> Div {
        div()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x293747))
            .bg(rgb(0x151e29))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_color(rgb(0xb8c9dc))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title.to_string()),
            )
    }
    fn form(&self, cx: &mut Context<Self>) -> Div {
        let receiving = self.action == Action::Receive;
        let mut source = Self::card(if receiving {
            "01  Save incoming files"
        } else {
            "01  Choose your content"
        })
        .child(self.field(
            if receiving {
                "Destination folder"
            } else {
                "File or folder"
            },
            0,
        ));
        let mut pick = div().flex().gap_2();
        if self.action == Action::Copy {
            pick = pick.child(
                self.button("file", "Choose file")
                    .on_click(cx.listener(|this, _, _, cx| this.choose(false, cx))),
            );
        }
        source = source.child(
            pick.child(
                self.button("folder", "Choose folder")
                    .on_click(cx.listener(|this, _, _, cx| this.choose(true, cx))),
            ),
        );
        let mut form = div().flex().flex_col().gap_4().child(source);
        if !receiving {
            let mut destination =
                Self::card("02  Connect to a receiver").child(self.field("Address or hostname", 1));
            if self.peers.is_empty() {
                destination = destination.child(div().text_sm().text_color(rgb(0x95a5b8))
                    .child("Nearby computers appear here when they start receiving. You can also enter an address."));
            } else {
                destination = destination
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x95a5b8))
                            .child("Nearby receivers"),
                    )
                    .child(
                        uniform_list(
                            "discovered-peers",
                            self.peers.len(),
                            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                range
                                    .map(|i| {
                                        let peer = this.peers[i].clone();
                                        this.button(
                                            i,
                                            format!(
                                                "{}  ·  {}  ·  {}",
                                                peer.name,
                                                peer.address,
                                                if peer.secure {
                                                    "Encrypted"
                                                } else {
                                                    "Unencrypted"
                                                }
                                            ),
                                        )
                                        .w_full()
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.inputs[1].update(cx, |i, cx| {
                                                    i.set(peer.address.ip().to_string(), cx)
                                                });
                                                this.inputs[2].update(cx, |i, cx| {
                                                    i.set(peer.address.port().to_string(), cx)
                                                });
                                                cx.notify();
                                            }),
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .h(px((self.peers.len().min(3) * 44) as f32)),
                    );
            }
            form = form.child(destination);
        }
        form = form.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(if self.secure {
                            rgb(0x88e5cf)
                        } else {
                            rgb(0xffc981)
                        })
                        .child(if self.secure {
                            "●  End-to-end encryption is on"
                        } else {
                            "●  Unencrypted transfer"
                        }),
                )
                .child(
                    self.button(
                        "advanced",
                        if self.advanced {
                            "Hide options  −"
                        } else {
                            "Transfer options  +"
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.advanced = !this.advanced;
                        cx.notify();
                    })),
                ),
        );
        if self.advanced {
            let mut advanced = Self::card("Transfer options").child(
                div()
                    .flex()
                    .gap_3()
                    .child(div().flex_1().min_w_0().child(self.field("Port", 2)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(self.field("Shared token (optional)", 3)),
                    ),
            );
            advanced = advanced.child(self.field(
                if receiving {
                    "Bind address"
                } else {
                    "Exclude globs (separate with ;)"
                },
                if receiving { 5 } else { 4 },
            ));
            let mut options = div().flex().flex_wrap().gap_2().child(self.checkbox(
                "Encryption",
                self.secure,
                Toggle::Secure,
                cx,
            ));
            if self.action.syncing() {
                options = options.child(self.checkbox(
                    "Respect .gitignore",
                    self.gitignore,
                    Toggle::Gitignore,
                    cx,
                ));
            }
            if self.action == Action::Copy {
                options = options.child(self.checkbox(
                    "Follow safe symlinks",
                    self.follow_links,
                    Toggle::FollowLinks,
                    cx,
                ));
            }
            if receiving {
                options = options
                    .child(self.checkbox("Allow sync", self.allow_sync, Toggle::SyncAccess, cx))
                    .child(self.checkbox("Overwrite copies", self.overwrite, Toggle::Overwrite, cx))
                    .child(self.checkbox(
                        "Nearby discovery",
                        self.discoverable,
                        Toggle::Discovery,
                        cx,
                    ));
                advanced = advanced.child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x95a5b8))
                        .child("Sync updates this folder directly. Copies are saved inside it."),
                );
            }
            form = form.child(advanced.child(options));
        }
        if !self.secure {
            form = form.child(div().p_3().rounded_lg().bg(rgb(0x3b2e18)).text_color(rgb(0xffc981))
                .child("Encryption and identity verification are off. Both computers must select this mode."));
        }
        form
    }
    fn button(&self, id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
        let id = id.into();
        let primary = id == ElementId::from("primary");
        div()
            .id(id)
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(0x2b3a4d))
            .bg(rgb(0x1c2735))
            .text_color(rgb(0xe6edf5))
            .cursor_pointer()
            .hover(move |s| {
                s.bg(if primary {
                    rgb(0x8bedda)
                } else {
                    rgb(0x293a4d)
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
                    .text_color(rgb(0x95a5b8))
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
    }
}
impl Render for Desktop {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.job.is_some();
        let mut navigation = div().flex().gap_1().flex_wrap();
        for action in [Action::Copy, Action::Receive, Action::Sync, Action::TwoWay] {
            navigation = navigation.child(
                self.button(
                    action.title(),
                    match action {
                        Action::Copy => "Send",
                        Action::Receive => "Receive",
                        Action::Sync => "Sync",
                        Action::TwoWay => "Two-way",
                    },
                )
                .when(self.action == action && self.view == View::Workflow, |s| {
                    s.bg(rgb(0x213b39))
                        .border_color(rgb(0x4b9f8f))
                        .text_color(rgb(0x8ff0d8))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.job.is_none() {
                        this.action = action;
                        this.view = View::Workflow;
                        this.preview_revision = None;
                        this.summary = None;
                        cx.notify();
                    }
                })),
            );
        }
        navigation = navigation.child(self.button("settings", "Settings").on_click(cx.listener(
            |this, _, _, cx| {
                if this.job.is_none() {
                    this.view = View::Settings;
                    this.refresh_peers();
                    cx.notify();
                }
            },
        )));
        let mut body = div().w_full().max_w(px(720.)).flex().flex_col().gap_5();
        if self.view == View::Settings {
            body = body
                .child(
                    div()
                        .text_size(px(28.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Settings"),
                )
                .child(
                    div()
                        .text_color(rgb(0x95a5b8))
                        .child("Your identity and remembered peers are shared with the CLI."),
                );
            let mut peers = Self::card("Remembered computers");
            if self.known.is_empty() {
                peers = peers.child(div().text_color(rgb(0x95a5b8)).child("No remembered peers yet. Approve a security code during your first transfer to remember a computer."));
            } else {
                peers =
                    peers
                        .child(
                            uniform_list(
                                "known-peers",
                                self.known.len(),
                                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                    range
                                        .map(|i| {
                                            let (endpoint, fingerprint) = this.known[i].clone();
                                            div()
                                                .h(px(64.))
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap_3()
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .flex()
                                                        .flex_col()
                                                        .gap_1()
                                                        .child(endpoint.clone())
                                                        .child(
                                                            div()
                                                                .text_sm()
                                                                .text_color(rgb(0x95a5b8))
                                                                .child(fingerprint),
                                                        ),
                                                )
                                                .child(
                                                    this.button(i, "Forget")
                                                        .text_color(rgb(0xffbacb))
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.peer_update(
                                                                    Some(endpoint.clone()),
                                                                    cx,
                                                                )
                                                            },
                                                        )),
                                                )
                                        })
                                        .collect::<Vec<_>>()
                                }),
                            )
                            .h(px((self.known.len().min(4) * 64) as f32)),
                        )
                        .child(div().text_sm().text_color(rgb(0x95a5b8)).child(
                            "Forgetting a computer requires comparing its security code again.",
                        ))
                        .child(
                            self.button(
                                "clear-peers",
                                if self.confirm_clear {
                                    "Confirm forget all computers"
                                } else {
                                    "Forget all computers"
                                },
                            )
                            .text_color(rgb(0xffbacb))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.confirm_clear {
                                    this.confirm_clear = false;
                                    this.peer_update(None, cx);
                                } else {
                                    this.confirm_clear = true;
                                    cx.notify();
                                }
                            })),
                        );
            }
            body = body.child(peers).child(
                Self::card("About XFER")
                    .child(format!("Version {}", xfer::VERSION))
                    .child(div().text_sm().text_color(rgb(0x95a5b8)).child(format!(
                            "Configuration: {}",
                            self.config
                                .as_ref()
                                .map_or("~/.xfer".into(), |p| p.display().to_string())
                        )))
                    .child(
                        self.button("releases", "Download desktop releases")
                            .on_click(|_, _, cx| cx.open_url(RELEASES)),
                    ),
            );
        } else {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(28.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(self.action.title()),
                    )
                    .child(div().text_color(rgb(0x95a5b8)).child(match self.action {
                        Action::Copy => "Send a file or folder directly to another computer.",
                        Action::Receive => {
                            "Choose where incoming files land. Keep this window open to receive."
                        }
                        Action::Sync => {
                            "Update another folder. Review the changes before applying them."
                        }
                        Action::TwoWay => {
                            "Keep both folders in step. Review changes and resolve conflicts first."
                        }
                    })),
            );
            if !running {
                body = body.child(self.form(cx));
            } else {
                body = body.child(
                    div()
                        .p_4()
                        .rounded_lg()
                        .bg(rgb(0x172a2b))
                        .text_color(rgb(0x88e5cf))
                        .child(if self.action == Action::Receive {
                            "Listening for incoming transfers"
                        } else {
                            "Transfer in progress"
                        }),
                );
                body = body.child(div().text_sm().text_color(rgb(0x95a5b8)).child(format!(
                    "{}: {}",
                    if self.action == Action::Receive {
                        "Saving to"
                    } else {
                        "Content"
                    },
                    self.inputs[0].read(cx).value()
                )));
                if self.action == Action::Receive {
                    if let Some(addresses) = &self.receiver_addresses {
                        body = body.child(Self::card("Connect from another computer")
                            .child(addresses.clone())
                            .child(div().text_sm().text_color(rgb(0x95a5b8)).child("In Send, choose this computer from Nearby receivers or enter one of these addresses.")));
                    }
                } else {
                    body = body.child(div().text_sm().text_color(rgb(0x95a5b8)).child(format!(
                        "To {}:{}",
                        self.inputs[1].read(cx).value(),
                        self.inputs[2].read(cx).value()
                    )));
                }
            }
            if let Some((prompt, _)) = &self.trust {
                body = body.child(
                    div()
                        .p_4()
                        .rounded_lg()
                        .bg(rgb(0x3b2e18))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(if prompt.changed {
                            "WARNING: saved peer identity changed"
                        } else {
                            "Compare this code on both computers"
                        })
                        .child(div().text_2xl().child(prompt.sas.clone()))
                        .child(prompt.endpoint.clone())
                        .child(prompt.fingerprint.clone())
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    self.button("trust", "Codes match — trust peer").on_click(
                                        cx.listener(|this, _, _, cx| this.answer(true, cx)),
                                    ),
                                )
                                .child(self.button("reject", "Reject").on_click(
                                    cx.listener(|this, _, _, cx| this.answer(false, cx)),
                                )),
                        ),
                );
            } else if let Some((sas, fingerprint)) = &self.sas {
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(format!("Security code: {sas}"))
                        .child(fingerprint.clone()),
                );
            }
            if let Some(progress) = &self.progress {
                let fraction = if progress.total == 0 {
                    0.
                } else {
                    (progress.transferred as f32 / progress.total as f32).clamp(0., 1.)
                };
                body = body
                    .child(format!("{} · {}", progress.phase, progress.current_path))
                    .child(
                        div()
                            .h(px(8.))
                            .rounded_md()
                            .bg(rgb(0x1c2735))
                            .child(div().h_full().w(relative(fraction)).bg(rgb(0x63e6c9))),
                    )
                    .child(format!(
                        "{} / {} · {} / {} files",
                        human_bytes(progress.transferred),
                        human_bytes(progress.total),
                        progress.files_done,
                        progress.files_total
                    ));
            }
            if let Some(summary) = &self.summary {
                body = body.child(format!(
                    "{} · {} files · {}",
                    if summary.preview {
                        "Preview"
                    } else {
                        "Complete"
                    },
                    summary.file_count,
                    summary.destination.display()
                ));
                if let Some(stats) = summary.sync_stats {
                    body = body.child(format!(
                        "{} changes · {} unchanged · {} to send · {} reused",
                        stats.changed_files,
                        stats.unchanged_files,
                        human_bytes(stats.sent_bytes),
                        human_bytes(stats.reused_bytes)
                    ));
                }
                if !summary.conflicts.is_empty() {
                    body = body
                        .child(format!("{} conflicts preserved", summary.conflicts.len()))
                        .child(
                            uniform_list(
                                "conflicts",
                                summary.conflicts.len(),
                                cx.processor(|this, range: std::ops::Range<usize>, _, _| {
                                    range
                                        .map(|i| {
                                            div().h(px(28.)).child(
                                                this.summary.as_ref().unwrap().conflicts[i].clone(),
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                }),
                            )
                            .h(px(110.)),
                        );
                }
                if self.action == Action::TwoWay && summary.preview {
                    let mut choices = div().flex().gap_2().flex_wrap();
                    for (id, label, policy) in [
                        ("preserve", "Preserve both", ConflictPolicy::Preserve),
                        ("local", "Prefer local", ConflictPolicy::PreferLocal),
                        ("remote", "Prefer remote", ConflictPolicy::PreferRemote),
                    ] {
                        choices = choices.child(self.button(id, label).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.policy = policy;
                                this.preview_revision = None;
                                this.start(true, cx);
                            },
                        )));
                    }
                    body = body.child(choices);
                }
            }
            let primary = if running {
                "Cancel"
            } else if self.action == Action::Receive {
                "Start receiving"
            } else if self.action.syncing() && self.preview_revision == Some(self.revision) {
                "Apply reviewed sync"
            } else if self.action.syncing() {
                "Preview sync"
            } else {
                "Send"
            };
            body = body.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        self.button("primary", primary)
                            .bg(rgb(0x63e6c9))
                            .text_color(rgb(0x102821))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.job.is_some() {
                                    this.cancel(&Cancel, window, cx);
                                } else {
                                    this.start(false, cx);
                                }
                            })),
                    )
                    .when(self.action.syncing(), |row| {
                        row.child(
                            self.button("preview", "Refresh preview")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if this.action.syncing() {
                                        this.start(true, cx);
                                    }
                                })),
                        )
                    })
                    .child(self.button("details", "Details").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.details = !this.details;
                            cx.notify();
                        },
                    ))),
            );
            if running && self.progress.is_none() {
                body = body.child("Preparing, connecting, or waiting for a sender…");
            }
            if self.details {
                body = body
                    .child(
                        self.rates
                            .description()
                            .unwrap_or_else(|| "Waiting for payload progress".into()),
                    )
                    .child(
                        uniform_list(
                            "logs",
                            self.logs.len(),
                            cx.processor(|this, range: std::ops::Range<usize>, _, _| {
                                range
                                    .map(|i| div().h(px(30.)).text_sm().child(this.logs[i].clone()))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .h(px(180.)),
                    )
                    .child(
                        self.button("peer-release", "Align versions / download releases")
                            .on_click(|_, _, cx| cx.open_url(RELEASES)),
                    );
            }
        }
        if let Some(error) = &self.error {
            body = body.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(0x422631))
                    .text_color(rgb(0xffbacb))
                    .child(error.clone()),
            );
        }
        div()
            .id("desktop")
            .key_context("Desktop")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::next_field))
            .on_action(cx.listener(Self::previous_field))
            .size_full()
            .bg(rgb(0x0e141c))
            .text_color(rgb(0xe6edf5))
            .font_family(if cfg!(target_os = "macos") {
                ".SystemUIFont"
            } else {
                "sans-serif"
            })
            .text_size(px(14.))
            .flex()
            .flex_col()
            .child(
                div()
                    .px_5()
                    .pt_4()
                    .pb_3()
                    .border_b_1()
                    .border_color(rgb(0x243141))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(img(self.logo.clone()).size(px(36.)))
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("XFER"),
                                    ),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x95a5b8))
                                    .child("Direct. Private. Local."),
                            ),
                    )
                    .child(navigation),
            )
            .child(
                div()
                    .id("content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_5()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(body),
            )
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
    Application::new().run(move |cx| {
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
        let bounds = Bounds::centered(None, size(px(940.), px(780.)), cx);
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

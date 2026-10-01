//! Frontend-independent workflow state and bounded background transfer workers.
use crate::{
    control::TransferControl,
    error::{Result, XferError},
    protocol::DEFAULT_PORT,
    reporter::{Progress, Reporter, TrustPrompt},
    secure_store::{LockedJsonStore, SecureDir},
    transfer::{
        ConflictPolicy, ReceiveOptions, SendOptions, TransferSummary, human_bytes,
        receive_on_listener_controlled, send_controlled, validate_receive_options,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    #[default]
    Copy,
    Sync,
    TwoWay,
    Receive,
}
impl Action {
    pub fn syncing(self) -> bool {
        matches!(self, Self::Sync | Self::TwoWay)
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Copy => "Send",
            Self::Sync => "One-way Sync",
            Self::TwoWay => "Two-way Sync",
            Self::Receive => "Receive",
        }
    }
}

/// Compatible with the terminal frontend's recent-workflow.json. Tokens never enter this type.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recent {
    #[serde(default)]
    pub gitignore: bool,
    pub action: Action,
    pub path: PathBuf,
    pub host: String,
    pub port: u16,
}
impl Default for Recent {
    fn default() -> Self {
        Self {
            gitignore: false,
            action: Action::Copy,
            path: PathBuf::new(),
            host: String::new(),
            port: DEFAULT_PORT,
        }
    }
}
pub fn preferences(config: Option<PathBuf>) -> Result<LockedJsonStore<Recent>> {
    Ok(LockedJsonStore::new(
        SecureDir::discover("xfer", config)?,
        "recent-workflow.json",
    ))
}
pub fn expand_path(text: &str) -> Result<PathBuf> {
    let text = text.trim();
    if text == "~" || text.starts_with("~/") {
        return Ok(dirs::home_dir()
            .ok_or_else(|| XferError::invalid_input("Home directory is unavailable."))?
            .join(text.strip_prefix("~/").unwrap_or("")));
    }
    Ok(PathBuf::from(text))
}

#[derive(Default)]
pub struct TransferRates {
    samples: VecDeque<(Instant, u64)>,
    first: Option<(Instant, u64)>,
    phase: &'static str,
    total: u64,
    finished: bool,
}
impl TransferRates {
    pub fn sample(&mut self, progress: &Progress, now: Instant) {
        if self.finished
            || self.phase != progress.phase
            || self.total != progress.total
            || self
                .samples
                .back()
                .is_some_and(|(_, bytes)| *bytes > progress.transferred)
        {
            *self = Self::default();
        }
        self.phase = progress.phase;
        self.total = progress.total;
        self.first.get_or_insert((now, progress.transferred));
        self.samples.push_back((now, progress.transferred));
        while self.samples.len() > 2
            && now.duration_since(self.samples[1].0) >= Duration::from_secs(2)
        {
            self.samples.pop_front();
        }
    }
    pub fn finish(&mut self) {
        self.finished = true;
    }
    fn rate(start: (Instant, u64), end: (Instant, u64)) -> String {
        let nanos = end.0.duration_since(start.0).as_nanos();
        if nanos == 0 {
            return "Measuring…".into();
        }
        let bytes = u128::from(end.1.saturating_sub(start.1)) * 1_000_000_000 / nanos;
        format!(
            "{}/s",
            human_bytes(u64::try_from(bytes).unwrap_or(u64::MAX))
        )
    }
    pub fn description(&self) -> Option<String> {
        let first = self.first?;
        let recent = *self.samples.front()?;
        let last = *self.samples.back()?;
        Some(format!(
            "{} · Current: {} · Average: {}\n{} / {} · Measured: {}s",
            self.phase,
            Self::rate(recent, last),
            Self::rate(first, last),
            human_bytes(last.1),
            human_bytes(self.total),
            last.0.duration_since(first.0).as_secs()
        ))
    }
}

pub enum WorkerEvent {
    Status(String),
    Sas(String, String),
    Trust(TrustPrompt, SyncSender<bool>),
    Received(TransferSummary),
    Finished(Result<TransferSummary>),
}
pub struct Envelope {
    pub operation: u64,
    pub event: WorkerEvent,
}
pub struct WorkerReporter {
    operation: u64,
    tx: SyncSender<Envelope>,
    progress: Arc<Mutex<Option<Progress>>>,
    control: Arc<TransferControl>,
}
impl WorkerReporter {
    fn send(&self, event: WorkerEvent) -> Result<()> {
        self.tx
            .send(Envelope {
                operation: self.operation,
                event,
            })
            .map_err(|_| XferError::Cancelled)
    }
}
impl Reporter for WorkerReporter {
    fn status(&self, message: &str) {
        let _ = self.send(WorkerEvent::Status(message.into()));
    }
    fn progress(&self, progress: &Progress) {
        *self.progress.lock().expect("progress mutex") = Some(progress.clone());
    }
    fn show_sas(&self, sas: &str, fingerprint: &str) {
        let _ = self.send(WorkerEvent::Sas(sas.into(), fingerprint.into()));
    }
    fn confirm_peer(&self, prompt: &TrustPrompt) -> Result<bool> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(WorkerEvent::Trust(prompt.clone(), tx))?;
        loop {
            self.control.check()?;
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(answer) => return Ok(answer),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(XferError::Cancelled),
            }
        }
    }
}

/// Dropping a job closes its event queue and cancels socket I/O without joining on the UI thread.
pub struct Job {
    pub operation: u64,
    pub control: Arc<TransferControl>,
    events: Receiver<Envelope>,
    worker: thread::JoinHandle<()>,
    progress: Arc<Mutex<Option<Progress>>>,
}
impl Job {
    pub fn start(
        operation: u64,
        recent: Recent,
        sender: SendOptions,
        receiver: ReceiveOptions,
    ) -> Result<Self> {
        let (tx, events) = mpsc::sync_channel(64);
        let control = Arc::new(TransferControl::default());
        let progress = Arc::default();
        let reporter = WorkerReporter {
            operation,
            tx,
            progress: Arc::clone(&progress),
            control: Arc::clone(&control),
        };
        let worker = thread::Builder::new()
            .name("xfer-transfer".into())
            .spawn(move || {
                // Persistence and path validation happen here, never in a frontend event handler.
                let result = (|| {
                    reporter.control.check()?;
                    preferences(sender.config_dir.clone())?.save(&recent)?;
                    if recent.action == Action::Receive {
                        validate_receive_options(&receiver)?;
                        let listener = crate::net::bind(&receiver.bind, receiver.port)?;
                        loop {
                            reporter.control.check()?;
                            let summary = receive_on_listener_controlled(
                                &listener,
                                &receiver,
                                &reporter,
                                &reporter.control,
                            )?;
                            reporter.send(WorkerEvent::Received(summary))?;
                        }
                    } else {
                        send_controlled(&sender, &reporter, &reporter.control)
                    }
                })();
                let _ = reporter.send(WorkerEvent::Finished(result));
            })?;
        Ok(Self {
            operation,
            control,
            events,
            worker,
            progress,
        })
    }
    /// True after the worker has finished transfer cleanup, including staged files.
    pub fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }
    pub fn try_recv(&self) -> Option<Envelope> {
        self.events.try_recv().ok()
    }
    pub fn take_progress(&self) -> Option<Progress> {
        self.progress.lock().expect("progress mutex").take()
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

pub fn send_options(recent: &Recent, config: Option<PathBuf>) -> SendOptions {
    SendOptions {
        conflict_policy: ConflictPolicy::Preserve,
        sync: recent.action.syncing(),
        preview: recent.action.syncing(),
        two_way: recent.action == Action::TwoWay,
        host: recent.host.clone(),
        port: recent.port,
        input: recent.path.clone(),
        excludes: vec![],
        gitignore: recent.gitignore && recent.action.syncing(),
        follow_links: false,
        secure: true,
        token: None,
        connect_timeout: Duration::from_secs(30),
        config_dir: config,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_read_old_workflows_and_never_store_tokens() {
        let value: Recent = serde_json::from_str(
            r#"{"action":"TwoWay","path":"project","host":"peer","port":9000}"#,
        )
        .unwrap();
        assert!(!value.gitignore);
        assert!(value.action.syncing());
        assert!(!serde_json::to_string(&value).unwrap().contains("token"));
    }
    #[test]
    fn cancellation_releases_pending_trust_without_frontend_reply() {
        let (tx, rx) = mpsc::sync_channel(1);
        let control = Arc::new(TransferControl::default());
        let reporter = WorkerReporter {
            operation: 1,
            tx,
            progress: Arc::default(),
            control: Arc::clone(&control),
        };
        let (done, result) = mpsc::channel();
        thread::spawn(move || {
            done.send(reporter.confirm_peer(&TrustPrompt {
                endpoint: "peer".into(),
                fingerprint: "key".into(),
                sas: "123".into(),
                changed: true,
            }))
            .unwrap();
        });
        let event = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        control.cancel();
        assert!(matches!(
            result.recv_timeout(Duration::from_secs(2)).unwrap(),
            Err(XferError::Cancelled)
        ));
        drop(event);
    }
    #[test]
    fn progress_does_not_fill_event_queue() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let reporter = WorkerReporter {
            operation: 1,
            tx,
            progress: Arc::default(),
            control: Arc::default(),
        };
        for transferred in 0..10000 {
            reporter.progress(&Progress {
                phase: "sending",
                current_path: "file".into(),
                transferred,
                total: 10000,
                files_done: 0,
                files_total: 1,
            });
        }
        assert_eq!(
            reporter
                .progress
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .transferred,
            9999
        );
    }
    #[test]
    fn rates_measure_stalls_and_phase_resets() {
        let mut rates = TransferRates::default();
        let now = Instant::now();
        let mut progress = Progress {
            phase: "sending",
            current_path: "f".into(),
            transferred: 0,
            total: 10000,
            files_done: 0,
            files_total: 1,
        };
        rates.sample(&progress, now);
        progress.transferred = 1000;
        rates.sample(&progress, now + Duration::from_secs(1));
        assert!(rates.description().unwrap().contains("Current: 1000 B/s"));
        rates.sample(&progress, now + Duration::from_secs(4));
        rates.sample(&progress, now + Duration::from_secs(5));
        assert!(rates.description().unwrap().contains("Current: 0 B/s"));
        progress.phase = "receiving";
        progress.transferred = 0;
        rates.sample(&progress, now + Duration::from_secs(6));
        assert!(rates.description().unwrap().contains("Measuring"));
    }
}

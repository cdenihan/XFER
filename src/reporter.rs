#[cfg(feature = "cli")]
use std::{
    io::{self, IsTerminal, Write},
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::error::{Result, XferError};

#[derive(Clone, Debug, Serialize)]
pub struct Progress {
    pub phase: &'static str,
    pub current_path: String,
    pub transferred: u64,
    pub total: u64,
    pub files_done: u64,
    pub files_total: u64,
}

#[derive(Clone, Debug)]
pub struct TrustPrompt {
    pub endpoint: String,
    pub fingerprint: String,
    pub sas: String,
    pub changed: bool,
}

pub trait Reporter: Send + Sync {
    fn status(&self, message: &str);
    fn progress(&self, progress: &Progress);
    fn show_sas(&self, sas: &str, fingerprint: &str);
    fn confirm_peer(&self, prompt: &TrustPrompt) -> Result<bool>;
}

#[derive(Default)]
pub struct SilentReporter;

impl Reporter for SilentReporter {
    fn status(&self, _message: &str) {}

    fn progress(&self, _progress: &Progress) {}

    fn show_sas(&self, _sas: &str, _fingerprint: &str) {}

    fn confirm_peer(&self, _prompt: &TrustPrompt) -> Result<bool> {
        Err(XferError::security(
            "the silent reporter cannot trust an unseen peer; provide an explicit trust policy",
        ))
    }
}

#[cfg(feature = "cli")]
pub struct CliReporter {
    progress: Mutex<Option<Instant>>,
    accept_new: bool,
    json: bool,
}

#[cfg(feature = "cli")]
impl CliReporter {
    pub fn new(accept_new: bool, json: bool) -> Self {
        Self {
            progress: Mutex::new(None),
            accept_new,
            json,
        }
    }

    pub fn finish(&self) {
        if self
            .progress
            .lock()
            .expect("progress mutex")
            .take()
            .is_some()
        {
            eprintln!();
        }
    }

    fn emit_json<T: Serialize>(event: &str, value: &T) {
        let object = serde_json::json!({
            "event": event,
            "data": value,
        });
        println!("{object}");
    }
}

#[cfg(feature = "cli")]
impl Reporter for CliReporter {
    fn status(&self, message: &str) {
        if self.json {
            Self::emit_json("status", &serde_json::json!({ "message": message }));
        } else {
            eprintln!("• {message}");
        }
    }

    fn progress(&self, snapshot: &Progress) {
        if self.json {
            Self::emit_json("progress", snapshot);
            return;
        }

        if !io::stderr().is_terminal() {
            return;
        }
        let mut last = self.progress.lock().expect("progress mutex");
        let now = Instant::now();
        if last.is_some_and(|time| now.duration_since(time) < Duration::from_millis(100)) {
            return;
        }
        *last = Some(now);
        // A single bounded line avoids terminal queries and background tick threads.
        let path = crate::protocol::sanitize_peer_text(&snapshot.current_path)
            .chars()
            .take(80)
            .collect::<String>();
        eprint!(
            "\r{} {}: {} / {} ({}/{})        ",
            snapshot.phase,
            path,
            crate::transfer::human_bytes(snapshot.transferred),
            crate::transfer::human_bytes(snapshot.total),
            snapshot.files_done,
            snapshot.files_total
        );
        let _ = io::stderr().flush();
    }

    fn show_sas(&self, sas: &str, fingerprint: &str) {
        if self.json {
            Self::emit_json(
                "sas",
                &serde_json::json!({ "sas": sas, "fingerprint": fingerprint }),
            );
        } else {
            eprintln!("Security code: {sas}");
            eprintln!("Peer fingerprint: {fingerprint}");
        }
    }

    fn confirm_peer(&self, prompt: &TrustPrompt) -> Result<bool> {
        self.finish();
        self.show_sas(&prompt.sas, &prompt.fingerprint);
        if prompt.changed {
            eprintln!(
                "WARNING: the saved identity for {} changed. Refusing automatic trust.",
                prompt.endpoint
            );
        } else if self.accept_new {
            self.status(&format!("trusting new peer {}", prompt.endpoint));
            return Ok(true);
        }
        if self.json || !io::stdin().is_terminal() {
            return Err(XferError::security(
                "peer trust requires an interactive terminal; compare the SAS and retry, or use --accept-new for a new peer",
            ));
        }

        eprint!(
            "Compare this code on the receiver. Trust {}? [y/N] ",
            prompt.endpoint
        );
        io::stderr().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        Ok(matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_reporter_rejects_unseen_peers() {
        let prompt = TrustPrompt {
            endpoint: "127.0.0.1:9000".into(),
            fingerprint: "fingerprint".into(),
            sas: "123-456".into(),
            changed: false,
        };
        assert!(SilentReporter.confirm_peer(&prompt).is_err());
    }
}

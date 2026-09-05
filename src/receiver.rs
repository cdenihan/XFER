//! An explicit receive state machine. Frames cannot bypass entry validation,
//! file verification, or the final manifest check on their way to publication.
use std::{
    collections::HashMap,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::{
    crypto::update_manifest,
    error::{Result, XferError},
    filesystem::{portable_path_key, safe_relative_path, validate_wire_name},
    protocol::{
        EntryEnd, EntryKind, EntryStart, FrameKind, Offer, TransferEnd, TransferKind,
        sanitize_peer_text,
    },
    reporter::Progress,
    storage::{Installed, Transaction},
    version::validate_peer_release_version,
};

const MAX_ENTRIES: u64 = 1_000_000;
const MAX_PATH_BYTES: usize = 4096;
const MAX_PATH_DEPTH: usize = 128;
const MAX_PATH_STORAGE: usize = 64 * 1024 * 1024;

struct IncomingFile {
    file: File,
    path: String,
    size: u64,
    received: u64,
    hash: Sha256,
}

enum State {
    BetweenEntries,
    File(IncomingFile),
    Verified,
    Failed,
}

struct PathEntry {
    spelling: PathBuf,
    kind: EntryKind,
    explicit: bool,
}

/// Tracks implicit parent directories as well as explicitly announced entries.
/// This prevents files from becoming parents and cross-platform path aliases.
#[derive(Default)]
pub(crate) struct PathRegistry {
    paths: HashMap<String, PathEntry>,
    storage: usize,
}

impl PathRegistry {
    pub(crate) fn insert(&mut self, path: &Path, kind: EntryKind) -> Result<()> {
        let ancestors = path
            .ancestors()
            .filter(|part| !part.as_os_str().is_empty())
            .collect::<Vec<_>>();
        if ancestors.len() > MAX_PATH_DEPTH {
            return Err(XferError::protocol("entry path exceeds depth limit"));
        }
        for ancestor in ancestors.into_iter().rev() {
            let explicit = ancestor == path;
            let expected_kind = if explicit { kind } else { EntryKind::Directory };
            let key = portable_path_key(ancestor)?;
            if let Some(previous) = self.paths.get_mut(&key) {
                if previous.spelling != ancestor {
                    return Err(XferError::protocol(
                        "entry paths contain colliding directory names",
                    ));
                }
                if previous.kind != expected_kind {
                    return Err(XferError::protocol(
                        "entry path conflicts with a file or directory",
                    ));
                }
                if explicit && previous.explicit {
                    return Err(XferError::protocol("duplicate entry path"));
                }
                previous.explicit |= explicit;
            } else {
                // Charge for both stored strings and the map entry, including
                // implicit ancestors, before retaining peer-controlled metadata.
                let cost =
                    key.len() + ancestor.as_os_str().len() + std::mem::size_of::<PathEntry>() + 64;
                if cost > MAX_PATH_STORAGE - self.storage {
                    return Err(XferError::protocol(
                        "entry paths exceed metadata memory limit",
                    ));
                }
                self.storage += cost;
                self.paths.insert(
                    key,
                    PathEntry {
                        spelling: ancestor.into(),
                        kind: expected_kind,
                        explicit,
                    },
                );
            }
        }
        Ok(())
    }
}

pub(crate) struct Receiver {
    offer: Offer,
    // Close the active file before removing staging (required on Windows).
    state: State,
    transaction: Transaction,
    paths: PathRegistry,
    entries: u64,
    files: u64,
    bytes: u64,
    manifest: Sha256,
    current_path: String,
}

/// Only the verified state can produce this value. Publication consumes it.
pub(crate) struct VerifiedTransfer {
    transaction: Transaction,
    pub file_count: u64,
    pub total_bytes: u64,
    pub peer_version: Option<String>,
}

impl VerifiedTransfer {
    pub fn commit(self) -> Result<(Installed, u64, u64, Option<String>)> {
        Ok((
            self.transaction.commit()?,
            self.file_count,
            self.total_bytes,
            self.peer_version,
        ))
    }
}

impl Receiver {
    pub fn new(offer: Offer, output: &Path, overwrite: bool) -> Result<Self> {
        validate_offer(&offer)?;
        let transaction = Transaction::begin(output, &offer.root_name, offer.kind, overwrite)?;
        Ok(Self {
            offer,
            transaction,
            state: State::BetweenEntries,
            paths: PathRegistry::default(),
            entries: 0,
            files: 0,
            bytes: 0,
            manifest: Sha256::new(),
            current_path: String::new(),
        })
    }

    /// Returns true only after accepting a valid final manifest. Every error
    /// poisons the machine, closing any active file and preventing publication.
    pub fn accept(&mut self, kind: FrameKind, payload: &[u8]) -> Result<bool> {
        let state = std::mem::replace(&mut self.state, State::Failed);
        if kind == FrameKind::Error {
            return Err(XferError::protocol(format!(
                "remote error: {}",
                sanitize_peer_text(&decode::<String>(payload)?)
            )));
        }
        self.state = match (state, kind) {
            (State::BetweenEntries, FrameKind::EntryStart) => self.start(decode(payload)?)?,
            (State::File(mut file), FrameKind::Data) => {
                if payload.is_empty() {
                    return Err(XferError::protocol("empty data frame makes no progress"));
                }
                if payload.len() as u64 > file.size - file.received {
                    return Err(XferError::protocol(format!(
                        "{} exceeded its declared size",
                        file.path
                    )));
                }
                file.file.write_all(payload)?;
                file.hash.update(payload);
                file.received += payload.len() as u64;
                self.bytes += payload.len() as u64;
                State::File(file)
            }
            (State::File(file), FrameKind::EntryEnd) => {
                let end: EntryEnd = decode(payload)?;
                if file.received != file.size {
                    return Err(XferError::protocol(format!(
                        "{} ended at {} bytes, expected {}",
                        file.path, file.received, file.size
                    )));
                }
                let digest: [u8; 32] = file.hash.finalize().into();
                if digest != end.sha256 {
                    return Err(XferError::security(format!(
                        "SHA-256 mismatch for {}",
                        file.path
                    )));
                }
                file.file.sync_all()?;
                update_manifest(&mut self.manifest, &file.path, &digest);
                self.files += 1;
                State::BetweenEntries
            }
            (State::BetweenEntries, FrameKind::TransferEnd) => {
                let end: TransferEnd = decode(payload)?;
                let digest: [u8; 32] = self.manifest.clone().finalize().into();
                if self.entries != self.offer.entry_count
                    || self.files != self.offer.file_count
                    || self.bytes != self.offer.total_bytes
                    || end.file_count != self.files
                    || end.total_bytes != self.bytes
                    || end.manifest_sha256 != digest
                {
                    return Err(XferError::security(
                        "transfer totals or manifest digest did not verify",
                    ));
                }
                State::Verified
            }
            (State::Verified | State::Failed, _) => {
                return Err(XferError::protocol("transfer is already closed"));
            }
            (State::BetweenEntries, other) => {
                return Err(XferError::protocol(format!(
                    "expected entry or transfer end, received {other:?}"
                )));
            }
            (State::File(_), other) => {
                return Err(XferError::protocol(format!(
                    "expected file data or entry end, received {other:?}"
                )));
            }
        };
        Ok(matches!(self.state, State::Verified))
    }

    fn start(&mut self, entry: EntryStart) -> Result<State> {
        if self.entries >= self.offer.entry_count {
            return Err(XferError::protocol("entry count exceeds offered total"));
        }
        if entry.path.len() > MAX_PATH_BYTES {
            return Err(XferError::protocol("entry path exceeds length limit"));
        }
        let relative = safe_relative_path(&entry.path)?;
        if self.offer.kind == TransferKind::File
            && (entry.kind != EntryKind::File || entry.path != self.offer.root_name)
        {
            return Err(XferError::protocol(
                "file transfer contained an unexpected entry",
            ));
        }
        match entry.kind {
            EntryKind::Directory if entry.size != 0 => {
                return Err(XferError::protocol("directory entry has a non-zero size"));
            }
            EntryKind::File
                if self.files >= self.offer.file_count
                    || entry.size > self.offer.total_bytes - self.bytes =>
            {
                return Err(XferError::protocol(
                    "entry exceeds the offered transfer totals",
                ));
            }
            _ => {}
        }
        self.paths.insert(&relative, entry.kind)?;
        self.entries += 1;
        self.current_path.clone_from(&entry.path);
        match entry.kind {
            EntryKind::Directory => {
                self.transaction.directory(&relative)?;
                Ok(State::BetweenEntries)
            }
            EntryKind::File => Ok(State::File(IncomingFile {
                file: self.transaction.file(&relative, self.offer.kind)?,
                path: entry.path,
                size: entry.size,
                received: 0,
                hash: Sha256::new(),
            })),
        }
    }

    pub fn progress(&self) -> Progress {
        Progress {
            phase: "Receiving",
            current_path: self.current_path.clone(),
            transferred: self.bytes,
            total: self.offer.total_bytes,
            files_done: self.files,
            files_total: self.offer.file_count,
        }
    }

    pub fn into_verified(self) -> Result<VerifiedTransfer> {
        if !matches!(self.state, State::Verified) {
            return Err(XferError::protocol("cannot publish an unverified transfer"));
        }
        Ok(VerifiedTransfer {
            transaction: self.transaction,
            file_count: self.files,
            total_bytes: self.bytes,
            peer_version: self.offer.release_version,
        })
    }
}

fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T> {
    serde_json::from_slice(payload).map_err(|error| XferError::Serialization(error.to_string()))
}

pub(crate) fn validate_offer(offer: &Offer) -> Result<()> {
    validate_wire_name(&offer.root_name)?;
    validate_peer_release_version(offer.release_version.as_deref())?;
    if offer.entry_count > MAX_ENTRIES {
        return Err(XferError::protocol("entry count exceeds safety limit"));
    }
    if offer.root_name.len() > MAX_PATH_BYTES {
        return Err(XferError::protocol("transfer root exceeds length limit"));
    }
    if offer.file_count == 0 && offer.total_bytes != 0 {
        return Err(XferError::protocol("transfer has bytes but no files"));
    }
    match offer.kind {
        TransferKind::File if offer.entry_count != 1 || offer.file_count != 1 => {
            Err(XferError::protocol("invalid file transfer counts"))
        }
        TransferKind::Directory if offer.file_count > offer.entry_count => {
            Err(XferError::protocol("file count exceeds entry count"))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use std::fs;
    use tempfile::tempdir;

    fn offer() -> Offer {
        Offer {
            root_name: "payload".into(),
            kind: TransferKind::Directory,
            total_bytes: 3,
            file_count: 1,
            entry_count: 1,
            release_version: None,
        }
    }

    fn message<T: Serialize>(receiver: &mut Receiver, kind: FrameKind, value: &T) -> Result<bool> {
        receiver.accept(kind, &serde_json::to_vec(value).unwrap())
    }

    fn start_file(receiver: &mut Receiver) {
        assert!(
            !message(
                receiver,
                FrameKind::EntryStart,
                &EntryStart {
                    path: "file".into(),
                    kind: EntryKind::File,
                    size: 3,
                }
            )
            .unwrap()
        );
    }

    #[test]
    fn dropping_an_open_file_closes_it_before_removing_staging() {
        let output = tempdir().unwrap();
        let mut receiver = Receiver::new(offer(), output.path(), false).unwrap();
        start_file(&mut receiver);
        receiver.accept(FrameKind::Data, b"a").unwrap();
        drop(receiver);
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }

    #[test]
    fn unverified_transfer_cannot_be_published() {
        let output = tempdir().unwrap();
        let receiver = Receiver::new(offer(), output.path(), false).unwrap();
        assert!(receiver.into_verified().is_err());
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }

    #[test]
    fn unexpected_frame_poisoning_prevents_recovery_and_publication() {
        for active_file in [false, true] {
            let output = tempdir().unwrap();
            let mut receiver = Receiver::new(offer(), output.path(), false).unwrap();
            if active_file {
                start_file(&mut receiver);
            }
            assert!(receiver.accept(FrameKind::Ready, b"null").is_err());
            assert!(receiver.accept(FrameKind::Data, b"abc").is_err());
            assert!(receiver.into_verified().is_err());
            assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn entry_end_before_declared_size_is_rejected() {
        let output = tempdir().unwrap();
        let mut receiver = Receiver::new(offer(), output.path(), false).unwrap();
        start_file(&mut receiver);
        receiver.accept(FrameKind::Data, b"a").unwrap();
        let error = message(
            &mut receiver,
            FrameKind::EntryEnd,
            &EntryEnd {
                sha256: Sha256::digest(b"a").into(),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("ended at 1 bytes, expected 3"));
        assert!(receiver.into_verified().is_err());
    }

    #[test]
    fn file_data_requires_an_open_file_and_nonempty_payload() {
        let output = tempdir().unwrap();
        let mut receiver = Receiver::new(offer(), output.path(), false).unwrap();
        assert!(receiver.accept(FrameKind::Data, b"abc").is_err());
        let mut receiver = Receiver::new(offer(), output.path(), false).unwrap();
        start_file(&mut receiver);
        assert!(receiver.accept(FrameKind::Data, b"").is_err());
    }

    #[test]
    fn valid_transfer_is_invisible_until_manifest_verifies_and_commit_runs() {
        let output = tempdir().unwrap();
        let mut receiver = Receiver::new(offer(), output.path(), false).unwrap();
        start_file(&mut receiver);
        receiver.accept(FrameKind::Data, b"a").unwrap();
        receiver.accept(FrameKind::Data, b"bc").unwrap();
        let digest: [u8; 32] = Sha256::digest(b"abc").into();
        message(
            &mut receiver,
            FrameKind::EntryEnd,
            &EntryEnd { sha256: digest },
        )
        .unwrap();
        assert!(!output.path().join("payload").exists());
        let mut manifest = Sha256::new();
        update_manifest(&mut manifest, "file", &digest);
        assert!(
            message(
                &mut receiver,
                FrameKind::TransferEnd,
                &TransferEnd {
                    file_count: 1,
                    total_bytes: 3,
                    manifest_sha256: manifest.finalize().into(),
                }
            )
            .unwrap()
        );
        let verified = receiver.into_verified().unwrap();
        assert!(!output.path().join("payload").exists());
        // Occupying the destination during the transfer must not lose the data.
        fs::write(output.path().join("payload"), b"existing").unwrap();
        let (installed, files, bytes, _) = verified.commit().unwrap();
        assert_eq!((files, bytes), (1, 3));
        assert_eq!(installed.destination, output.path().join("payload (1)"));
        assert_eq!(
            fs::read(installed.destination.join("file")).unwrap(),
            b"abc"
        );
        assert_eq!(
            fs::read(output.path().join("payload")).unwrap(),
            b"existing"
        );
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 2);
    }

    #[test]
    fn incorrect_final_manifest_never_produces_a_verified_transfer() {
        let output = tempdir().unwrap();
        let mut receiver = Receiver::new(offer(), output.path(), false).unwrap();
        start_file(&mut receiver);
        receiver.accept(FrameKind::Data, b"abc").unwrap();
        message(
            &mut receiver,
            FrameKind::EntryEnd,
            &EntryEnd {
                sha256: Sha256::digest(b"abc").into(),
            },
        )
        .unwrap();
        assert!(
            message(
                &mut receiver,
                FrameKind::TransferEnd,
                &TransferEnd {
                    file_count: 1,
                    total_bytes: 3,
                    manifest_sha256: [0; 32]
                }
            )
            .is_err()
        );
        assert!(receiver.into_verified().is_err());
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }

    #[test]
    fn empty_directory_can_be_verified_and_published() {
        let output = tempdir().unwrap();
        let mut offer = offer();
        offer.entry_count = 0;
        offer.file_count = 0;
        offer.total_bytes = 0;
        let mut receiver = Receiver::new(offer, output.path(), false).unwrap();
        message(
            &mut receiver,
            FrameKind::TransferEnd,
            &TransferEnd {
                file_count: 0,
                total_bytes: 0,
                manifest_sha256: Sha256::digest([]).into(),
            },
        )
        .unwrap();
        let (installed, _, _, _) = receiver.into_verified().unwrap().commit().unwrap();
        assert!(installed.destination.is_dir());
    }

    #[test]
    fn path_registry_rejects_file_parents_and_duplicate_entries() {
        let mut paths = PathRegistry::default();
        paths
            .insert(Path::new("parent/child"), EntryKind::File)
            .unwrap();
        // An implicit parent may later receive an explicit directory entry.
        paths
            .insert(Path::new("parent"), EntryKind::Directory)
            .unwrap();
        assert!(
            paths
                .insert(Path::new("parent"), EntryKind::Directory)
                .is_err()
        );
        assert!(paths.insert(Path::new("parent"), EntryKind::File).is_err());
        assert!(
            paths
                .insert(Path::new("parent/child/nested"), EntryKind::File)
                .is_err()
        );
    }

    #[test]
    fn excessive_path_depth_and_metadata_are_rejected() {
        let mut paths = PathRegistry::default();
        let deep = vec!["dir"; MAX_PATH_DEPTH + 1].join("/");
        assert!(
            paths
                .insert(Path::new(&deep), EntryKind::Directory)
                .is_err()
        );
        paths.storage = MAX_PATH_STORAGE;
        assert!(paths.insert(Path::new("file"), EntryKind::File).is_err());
    }
}

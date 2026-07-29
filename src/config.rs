use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rust_cli_toolkit::{LockedJsonStore, SecureDir};
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::error::{Result, XferError};

const IDENTITY_FILE: &str = "identity.key";
const PEERS_FILE: &str = "known_peers.json";

/// `~/.xfer`, or an explicit override.
///
/// The private-directory, atomic-write, and advisory-lock mechanics live in
/// `rust_cli_toolkit::SecureDir`; this type is the XFER-specific naming on top
/// of it.
#[derive(Clone, Debug)]
pub struct Paths {
    directory: SecureDir,
}

impl Paths {
    pub fn discover(override_root: Option<PathBuf>) -> Result<Self> {
        Ok(Self {
            directory: SecureDir::discover("xfer", override_root)?,
        })
    }

    pub fn root(&self) -> &Path {
        self.directory.root()
    }

    pub fn identity(&self) -> PathBuf {
        self.directory.path(IDENTITY_FILE)
    }

    pub fn peers(&self) -> PathBuf {
        self.directory.path(PEERS_FILE)
    }

    pub fn ensure(&self) -> Result<()> {
        self.directory.ensure()?;
        Ok(())
    }

    /// The peer store, which locks around its own read-modify-write cycle so
    /// concurrent transfers cannot lose each other's entries.
    fn peer_store(&self) -> LockedJsonStore<TrustStore> {
        LockedJsonStore::new(self.directory.clone(), PEERS_FILE)
    }
}

pub struct Identity {
    secret: StaticSecret,
}

impl Identity {
    pub fn load_or_create(paths: &Paths) -> Result<Self> {
        paths.ensure()?;
        let path = paths.identity();
        loop {
            if let Some(bytes) = paths.directory.read(IDENTITY_FILE)? {
                return Self::from_bytes(bytes, &path);
            }

            let mut secret_bytes = [0_u8; 32];
            getrandom::fill(&mut secret_bytes).map_err(|error| {
                XferError::Configuration(format!("could not generate receiver identity: {error}"))
            })?;
            // Racing processes must agree on one identity, so the loser of the
            // create reads back the winner's key rather than overwriting it.
            let created = paths
                .directory
                .write_private_noclobber(IDENTITY_FILE, &secret_bytes)?;
            if created {
                let secret = StaticSecret::from(secret_bytes);
                secret_bytes.zeroize();
                return Ok(Self { secret });
            }
            secret_bytes.zeroize();
        }
    }

    pub fn secret(&self) -> &StaticSecret {
        &self.secret
    }

    pub fn public(&self) -> PublicKey {
        PublicKey::from(&self.secret)
    }

    fn from_bytes(mut bytes: Vec<u8>, path: &Path) -> Result<Self> {
        if bytes.len() != 32 {
            bytes.zeroize();
            return Err(XferError::Configuration(format!(
                "{} must contain exactly 32 bytes",
                path.display()
            )));
        }
        let mut secret_bytes = [0_u8; 32];
        secret_bytes.copy_from_slice(&bytes);
        bytes.zeroize();
        let secret = StaticSecret::from(secret_bytes);
        secret_bytes.zeroize();
        Ok(Self { secret })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KnownPeer {
    pub fingerprint: String,
    pub first_seen: u64,
    pub last_seen: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TrustStore {
    peers: BTreeMap<String, KnownPeer>,
}

impl TrustStore {
    pub fn load(paths: &Paths) -> Result<Self> {
        Ok(paths.peer_store().load()?)
    }

    pub fn get(&self, endpoint: &str) -> Option<&KnownPeer> {
        self.peers.get(endpoint)
    }

    pub fn remember(&mut self, endpoint: String, fingerprint: String) {
        let now = unix_timestamp();
        self.peers
            .entry(endpoint)
            .and_modify(|peer| {
                peer.fingerprint.clone_from(&fingerprint);
                peer.last_seen = now;
            })
            .or_insert(KnownPeer {
                fingerprint,
                first_seen: now,
                last_seen: now,
            });
    }

    pub fn remove(&mut self, endpoint: &str) -> bool {
        self.peers.remove(endpoint).is_some()
    }

    pub fn clear(&mut self) {
        self.peers.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &KnownPeer)> {
        self.peers
            .iter()
            .map(|(endpoint, peer)| (endpoint.as_str(), peer))
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        paths.peer_store().save(self)?;
        Ok(())
    }

    /// Locks the store, applies `operation`, and saves the result. A failing
    /// `operation` leaves the stored peers untouched.
    pub fn update<T>(paths: &Paths, operation: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        // The shared store speaks its own error type, which cannot represent
        // every `XferError`. The real error is carried out of the closure here
        // and the returned one is only a signal to abandon the save.
        let mut failure = None;
        let outcome = paths.peer_store().update(|store| match operation(store) {
            Ok(value) => Ok(value),
            Err(error) => {
                failure = Some(error);
                Err(rust_cli_toolkit::Error::Configuration(
                    "peer store was left unchanged".into(),
                ))
            }
        });
        match outcome {
            Ok(value) => Ok(value),
            Err(error) => Err(failure.unwrap_or_else(|| error.into())),
        }
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{Arc, Barrier},
        thread,
    };

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn identity_is_stable() {
        let directory = tempdir().unwrap();
        let paths = Paths::discover(Some(directory.path().to_path_buf())).unwrap();
        let first = Identity::load_or_create(&paths).unwrap().public();
        let second = Identity::load_or_create(&paths).unwrap().public();
        assert_eq!(first.as_bytes(), second.as_bytes());
    }

    #[test]
    fn concurrent_identity_creation_keeps_one_winner() {
        let directory = tempdir().unwrap();
        let paths = Arc::new(Paths::discover(Some(directory.path().to_path_buf())).unwrap());
        let handles = (0..8)
            .map(|_| {
                let paths = Arc::clone(&paths);
                thread::spawn(move || Identity::load_or_create(&paths).unwrap().public())
            })
            .collect::<Vec<_>>();
        let identities = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();

        assert!(
            identities
                .windows(2)
                .all(|pair| pair[0].as_bytes() == pair[1].as_bytes())
        );
        assert_eq!(
            Identity::load_or_create(&paths)
                .unwrap()
                .public()
                .as_bytes(),
            identities[0].as_bytes()
        );
    }

    #[test]
    fn trust_store_round_trips() {
        let directory = tempdir().unwrap();
        let paths = Paths::discover(Some(directory.path().to_path_buf())).unwrap();
        let mut store = TrustStore::default();
        store.remember("127.0.0.1:9000".into(), "abcd".into());
        store.save(&paths).unwrap();

        let loaded = TrustStore::load(&paths).unwrap();
        assert_eq!(loaded.get("127.0.0.1:9000").unwrap().fingerprint, "abcd");
    }

    #[test]
    fn concurrent_trust_store_updates_are_merged() {
        let directory = tempdir().unwrap();
        let paths = Arc::new(Paths::discover(Some(directory.path().to_path_buf())).unwrap());
        let barrier = Arc::new(Barrier::new(8));
        let handles = (0..8)
            .map(|index| {
                let paths = Arc::clone(&paths);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    TrustStore::update(&paths, |store| {
                        store.remember(format!("receiver-{index}:9000"), format!("{index:064x}"));
                        Ok(())
                    })
                    .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }

        let store = TrustStore::load(&paths).unwrap();
        for index in 0..8 {
            assert!(store.get(&format!("receiver-{index}:9000")).is_some());
        }
    }

    #[test]
    fn invalid_identity_length_is_rejected() {
        let directory = tempdir().unwrap();
        let paths = Paths::discover(Some(directory.path().to_path_buf())).unwrap();
        paths.ensure().unwrap();
        fs::write(paths.identity(), [7_u8; 31]).unwrap();
        assert!(Identity::load_or_create(&paths).is_err());
    }

    #[test]
    fn invalid_peer_store_is_rejected() {
        let directory = tempdir().unwrap();
        let paths = Paths::discover(Some(directory.path().to_path_buf())).unwrap();
        paths.ensure().unwrap();
        fs::write(paths.peers(), b"{not json").unwrap();
        assert!(TrustStore::load(&paths).is_err());
    }

    #[test]
    fn remembering_peer_preserves_first_seen_and_updates_identity() {
        let mut store = TrustStore::default();
        store.remember("receiver:9000".into(), "first".into());
        let first_seen = store.get("receiver:9000").unwrap().first_seen;
        store.remember("receiver:9000".into(), "second".into());
        let peer = store.get("receiver:9000").unwrap();
        assert_eq!(peer.first_seen, first_seen);
        assert_eq!(peer.fingerprint, "second");
        assert!(peer.last_seen >= peer.first_seen);
    }

    #[cfg(unix)]
    #[test]
    fn configuration_files_use_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().unwrap();
        let paths = Paths::discover(Some(directory.path().join("config"))).unwrap();
        Identity::load_or_create(&paths).unwrap();
        let mut store = TrustStore::default();
        store.remember("receiver:9000".into(), "abcd".into());
        store.save(&paths).unwrap();

        assert_eq!(
            fs::metadata(paths.root()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(paths.identity()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(paths.peers()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

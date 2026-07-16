use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::error::{Result, XferError};

const IDENTITY_FILE: &str = "identity.key";
const PEERS_FILE: &str = "known_peers.json";

#[derive(Clone, Debug)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    pub fn discover(override_root: Option<PathBuf>) -> Result<Self> {
        if let Some(root) = override_root {
            return Ok(Self { root });
        }

        let home = dirs::home_dir().ok_or_else(|| {
            XferError::Configuration("could not determine the home directory".into())
        })?;
        Ok(Self {
            root: home.join(".xfer"),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn identity(&self) -> PathBuf {
        self.root.join(IDENTITY_FILE)
    }

    pub fn peers(&self) -> PathBuf {
        self.root.join(PEERS_FILE)
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        set_private_dir_permissions(&self.root)?;
        Ok(())
    }
}

pub struct Identity {
    secret: StaticSecret,
}

impl Identity {
    pub fn load_or_create(paths: &Paths) -> Result<Self> {
        paths.ensure()?;
        let path = paths.identity();
        if path.exists() {
            let mut bytes = fs::read(&path)?;
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
            return Ok(Self { secret });
        }

        let mut secret_bytes = [0_u8; 32];
        getrandom::fill(&mut secret_bytes).map_err(|error| {
            XferError::Configuration(format!("could not generate receiver identity: {error}"))
        })?;
        write_private_file(&path, &secret_bytes)?;
        let secret = StaticSecret::from(secret_bytes);
        secret_bytes.zeroize();
        Ok(Self { secret })
    }

    pub fn secret(&self) -> &StaticSecret {
        &self.secret
    }

    pub fn public(&self) -> PublicKey {
        PublicKey::from(&self.secret)
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
        let path = paths.peers();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read(&path)?;
        serde_json::from_slice(&contents)
            .map_err(|error| XferError::Configuration(format!("invalid peer store: {error}")))
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
        paths.ensure()?;
        let encoded = serde_json::to_vec_pretty(self).map_err(|error| {
            XferError::Configuration(format!("could not encode peer store: {error}"))
        })?;
        let destination = paths.peers();
        let mut temporary = tempfile::NamedTempFile::new_in(paths.root())?;
        temporary.write_all(&encoded)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        set_private_file_permissions(temporary.path())?;
        temporary
            .persist(&destination)
            .map_err(|error| XferError::Io(error.error))?;
        Ok(())
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| XferError::Configuration("identity path has no parent".into()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    set_private_file_permissions(temporary.path())?;
    temporary
        .persist(path)
        .map_err(|error| XferError::Io(error.error))?;
    Ok(())
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
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
    fn trust_store_round_trips() {
        let directory = tempdir().unwrap();
        let paths = Paths::discover(Some(directory.path().to_path_buf())).unwrap();
        let mut store = TrustStore::default();
        store.remember("127.0.0.1:9000".into(), "abcd".into());
        store.save(&paths).unwrap();

        let loaded = TrustStore::load(&paths).unwrap();
        assert_eq!(loaded.get("127.0.0.1:9000").unwrap().fingerprint, "abcd");
    }
}

//! A private, OS-appropriate application data directory with atomic writes,
//! advisory-locked JSON state, and standalone locks for whole operations,
//! factored out of per-consumer duplication.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    marker::PhantomData,
    path::{Path, PathBuf},
};

use serde::{Serialize, de::DeserializeOwned};

use crate::error::{Result, XferError as Error};

/// A directory under the user's home, created with owner-only permissions.
#[derive(Clone, Debug)]
pub struct SecureDir {
    root: PathBuf,
}

impl SecureDir {
    /// Resolves to `~/.{app_name}`, or `override_root` when given.
    pub fn discover(app_name: &str, override_root: Option<PathBuf>) -> Result<Self> {
        if let Some(root) = override_root {
            return Ok(Self { root });
        }
        let home = dirs::home_dir()
            .ok_or_else(|| Error::Configuration("could not determine the home directory".into()))?;
        Ok(Self {
            root: home.join(format!(".{app_name}")),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        #[cfg(unix)]
        set_private_permissions(&self.root)?;
        Ok(())
    }

    /// Reads `name`, or `None` if it does not exist.
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        match fs::read(self.path(name)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Atomically replaces `name` with owner-only (0600) permissions.
    pub fn write_private(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.ensure()?;
        let temp = write_temp(&self.root, bytes)?;
        temp.persist(self.path(name))
            .map_err(|error| Error::Io(error.error))?;
        Ok(())
    }

    /// Like [`write_private`](Self::write_private), but leaves an existing
    /// file untouched and reports `false` instead of overwriting it.
    pub fn write_private_noclobber(&self, name: &str, bytes: &[u8]) -> Result<bool> {
        self.ensure()?;
        let temp = write_temp(&self.root, bytes)?;
        match temp.persist_noclobber(self.path(name)) {
            Ok(_) => Ok(true),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(Error::Io(error.error)),
        }
    }

    /// Takes an exclusive advisory lock called `name`, waiting for any other
    /// holder to release it.
    ///
    /// Use this to serialize a whole operation — a sync, a migration, a
    /// multi-file rewrite — rather than an individual file write. For the
    /// narrower case of one JSON document, [`LockedJsonStore::update`] already
    /// locks around its own read-modify-write cycle.
    pub fn lock_exclusive(&self, name: &str) -> Result<LockGuard> {
        FileLock::acquire(&self.private_lock_path(name)?)
    }

    /// Like [`lock_exclusive`](Self::lock_exclusive), but reports `None`
    /// immediately when another process holds the lock instead of waiting.
    ///
    /// Prefer this when a command should tell the user that a concurrent run is
    /// in progress rather than appear to hang.
    pub fn try_lock_exclusive(&self, name: &str) -> Result<Option<LockGuard>> {
        FileLock::try_acquire(&self.private_lock_path(name)?)
    }

    /// Creates the lock file, if it is absent, and makes it owner-only before
    /// anyone waits on it.
    fn private_lock_path(&self, name: &str) -> Result<PathBuf> {
        self.ensure()?;
        let path = self.path(name);
        drop(open_lock_file(&path)?);
        #[cfg(unix)]
        set_private_permissions(&path)?;
        Ok(path)
    }
}

/// An exclusive advisory lock on an arbitrary path.
///
/// [`SecureDir`] locks files it owns; this locks anywhere, for when the lock
/// belongs beside the thing it guards — a build directory, a staging area —
/// rather than in the application's own state directory. Permissions are left
/// to the caller's umask, because such a file is not necessarily private.
pub struct FileLock;

impl FileLock {
    /// Takes the lock, waiting for any other holder to release it.
    pub fn acquire(path: &Path) -> Result<LockGuard> {
        let file = open_lock_file(path)?;
        file.lock()?;
        Ok(LockGuard { file })
    }

    /// Like [`acquire`](Self::acquire), but reports `None` immediately when
    /// another process holds the lock instead of waiting.
    pub fn try_acquire(path: &Path) -> Result<Option<LockGuard>> {
        let file = open_lock_file(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(LockGuard { file })),
            // Contention is distinct from an actual locking failure.
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }
}

fn open_lock_file(path: &Path) -> Result<fs::File> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    // Never truncate: holders may keep state in the lock file. The operating
    // system lock belongs to the open file handle, not the file's contents.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    Ok(file)
}

/// An exclusive advisory lock on a file in a [`SecureDir`].
///
/// The lock is released when the guard is dropped, and the operating system
/// releases it if the process exits or is killed, so an interrupted run never
/// leaves a stale lock behind.
#[derive(Debug)]
pub struct LockGuard {
    file: fs::File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn write_temp(root: &Path, bytes: &[u8]) -> Result<tempfile::NamedTempFile> {
    let mut temp = tempfile::NamedTempFile::new_in(root)?;
    temp.write_all(bytes)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    #[cfg(unix)]
    set_private_permissions(temp.path())?;
    Ok(temp)
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if path.is_dir() { 0o700 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// A JSON-serialized value in a [`SecureDir`], guarded by an advisory lock
/// file so concurrent processes serialize their read-modify-write cycles.
pub struct LockedJsonStore<T> {
    dir: SecureDir,
    file_name: String,
    lock_name: String,
    _marker: PhantomData<T>,
}

impl<T: Default + Serialize + DeserializeOwned> LockedJsonStore<T> {
    pub fn new(dir: SecureDir, file_name: impl Into<String>) -> Self {
        let file_name = file_name.into();
        let lock_name = format!(".{file_name}.lock");
        Self {
            dir,
            file_name,
            lock_name,
            _marker: PhantomData,
        }
    }

    /// Loads the stored value, or `T::default()` if nothing has been saved yet.
    pub fn load(&self) -> Result<T> {
        match self.dir.read(&self.file_name)? {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                Error::Configuration(format!("invalid {}: {error}", self.file_name))
            }),
            None => Ok(T::default()),
        }
    }

    pub fn save(&self, value: &T) -> Result<()> {
        let _lock = self.lock()?;
        self.save_unlocked(value)
    }

    /// Locks, loads, runs `operation` against the in-memory value, then
    /// atomically saves the result before releasing the lock.
    pub fn update<R>(&self, operation: impl FnOnce(&mut T) -> Result<R>) -> Result<R> {
        let _lock = self.lock()?;
        let mut value = self.load()?;
        let result = operation(&mut value)?;
        self.save_unlocked(&value)?;
        Ok(result)
    }

    fn save_unlocked(&self, value: &T) -> Result<()> {
        let encoded = serde_json::to_vec_pretty(value).map_err(|error| {
            Error::Configuration(format!("could not encode {}: {error}", self.file_name))
        })?;
        self.dir.write_private(&self.file_name, &encoded)
    }

    fn lock(&self) -> Result<LockGuard> {
        self.dir.lock_exclusive(&self.lock_name)
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    use tempfile::tempdir;

    fn dir() -> (tempfile::TempDir, SecureDir) {
        let temp = tempdir().unwrap();
        let secure = SecureDir::discover("test", Some(temp.path().join("store"))).unwrap();
        (temp, secure)
    }

    #[test]
    fn an_exclusive_lock_excludes_a_second_holder() {
        let (_temp, secure) = dir();
        let held = secure.lock_exclusive("sync.lock").unwrap();
        assert!(
            secure.try_lock_exclusive("sync.lock").unwrap().is_none(),
            "a second attempt must not succeed while the first is held"
        );
        drop(held);
        assert!(
            secure.try_lock_exclusive("sync.lock").unwrap().is_some(),
            "dropping the guard releases the lock"
        );
    }

    #[test]
    fn distinct_names_do_not_contend() {
        let (_temp, secure) = dir();
        let _first = secure.lock_exclusive("one.lock").unwrap();
        assert!(secure.try_lock_exclusive("two.lock").unwrap().is_some());
    }

    #[test]
    fn locking_creates_the_directory_on_demand() {
        let temp = tempdir().unwrap();
        let secure = SecureDir::discover("test", Some(temp.path().join("missing/nested"))).unwrap();
        let _guard = secure.lock_exclusive("sync.lock").unwrap();
        assert!(secure.root().is_dir());
    }

    #[test]
    fn a_path_lock_excludes_a_second_holder() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("build/.lock");
        let held = FileLock::acquire(&path).unwrap();
        assert!(FileLock::try_acquire(&path).unwrap().is_none());
        drop(held);
        assert!(FileLock::try_acquire(&path).unwrap().is_some());
    }

    #[test]
    fn a_path_lock_creates_missing_parent_directories() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("deeply/nested/build.lock");
        let _guard = FileLock::acquire(&path).unwrap();
        assert!(path.is_file());
    }

    /// Acquiring a lock must preserve existing file contents.
    #[test]
    fn a_path_lock_does_not_truncate_an_existing_file() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("build.lock");
        std::fs::write(&path, b"owner=1234").unwrap();
        let guard = FileLock::acquire(&path).unwrap();
        assert_eq!(guard.file.metadata().unwrap().len(), 10);
        // Windows denies reads through a second handle while the lock is held.
        drop(guard);
        assert_eq!(std::fs::read(&path).unwrap(), b"owner=1234");
    }

    #[cfg(unix)]
    #[test]
    fn the_lock_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, secure) = dir();
        let _guard = secure.lock_exclusive("sync.lock").unwrap();
        let mode = std::fs::metadata(secure.path("sync.lock"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Barrier},
        thread,
    };

    use serde::Deserialize;
    use tempfile::tempdir;

    use super::*;

    #[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
    struct Counters {
        values: BTreeMap<String, u32>,
    }

    fn dir_in(root: &Path) -> SecureDir {
        SecureDir::discover("ignored", Some(root.to_path_buf())).unwrap()
    }

    #[test]
    fn write_and_read_round_trip() {
        let directory = tempdir().unwrap();
        let dir = dir_in(directory.path());
        dir.write_private("value.txt", b"hello").unwrap();
        assert_eq!(dir.read("value.txt").unwrap().unwrap(), b"hello");
    }

    #[test]
    fn read_missing_file_is_none() {
        let directory = tempdir().unwrap();
        let dir = dir_in(directory.path());
        assert!(dir.read("missing.txt").unwrap().is_none());
    }

    #[test]
    fn noclobber_refuses_to_overwrite() {
        let directory = tempdir().unwrap();
        let dir = dir_in(directory.path());
        assert!(dir.write_private_noclobber("value.txt", b"first").unwrap());
        assert!(!dir.write_private_noclobber("value.txt", b"second").unwrap());
        assert_eq!(dir.read("value.txt").unwrap().unwrap(), b"first");
    }

    #[test]
    fn locked_store_round_trips() {
        let directory = tempdir().unwrap();
        let store: LockedJsonStore<Counters> =
            LockedJsonStore::new(dir_in(directory.path()), "counters.json");
        store
            .update(|counters| {
                counters.values.insert("a".into(), 1);
                Ok(())
            })
            .unwrap();
        assert_eq!(store.load().unwrap().values.get("a"), Some(&1));
    }

    #[test]
    fn concurrent_updates_are_serialized() {
        let directory = tempdir().unwrap();
        let store = Arc::new(LockedJsonStore::<Counters>::new(
            dir_in(directory.path()),
            "counters.json",
        ));
        let barrier = Arc::new(Barrier::new(8));
        let handles = (0..8)
            .map(|index| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    store
                        .update(|counters| {
                            counters.values.insert(format!("key-{index}"), index);
                            Ok(())
                        })
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }
        let loaded = store.load().unwrap();
        for index in 0..8_u32 {
            assert_eq!(loaded.values.get(&format!("key-{index}")), Some(&index));
        }
    }

    #[cfg(unix)]
    #[test]
    fn files_and_directories_use_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().unwrap();
        let dir = dir_in(&directory.path().join("nested"));
        dir.write_private("value.txt", b"secret").unwrap();

        assert_eq!(
            fs::metadata(dir.root()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(dir.path("value.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

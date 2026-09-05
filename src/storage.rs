//! Receive staging and destination publication, independent of the wire protocol.
use crate::{
    error::{Result, XferError},
    filesystem::{choose_destination, path_exists},
    protocol::TransferKind,
};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Owns all incoming data until the receiver verifies the complete transfer.
/// Dropping an unfinished transaction removes its private staging directory.
pub(crate) struct Transaction {
    staging: tempfile::TempDir,
    output: PathBuf,
    root_name: String,
    overwrite: bool,
}

pub(crate) struct Installed {
    pub destination: PathBuf,
    pub warning: Option<String>,
}

impl Transaction {
    pub fn begin(
        output: &Path,
        root_name: &str,
        kind: TransferKind,
        overwrite: bool,
    ) -> Result<Self> {
        fs::create_dir_all(output)?;
        let staging = tempfile::Builder::new()
            .prefix(".xfer-stage-")
            .tempdir_in(output)?;
        let transaction = Self {
            staging,
            output: output.into(),
            root_name: root_name.into(),
            overwrite,
        };
        if kind == TransferKind::Directory {
            fs::create_dir(transaction.payload())?;
        }
        Ok(transaction)
    }

    fn payload(&self) -> PathBuf {
        self.staging.path().join("payload")
    }

    fn target(&self, relative: &Path, kind: TransferKind) -> PathBuf {
        match kind {
            TransferKind::File => self.payload(),
            TransferKind::Directory => self.payload().join(relative),
        }
    }

    pub fn directory(&self, relative: &Path) -> Result<()> {
        fs::create_dir_all(self.payload().join(relative))?;
        Ok(())
    }

    pub fn file(&self, relative: &Path, kind: TransferKind) -> Result<File> {
        let target = self.target(relative, kind);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        // Even a validation bug must never truncate a previously staged file.
        Ok(fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?)
    }

    pub fn commit(self) -> Result<Installed> {
        // Choose at publication time, so a name occupied while data was arriving
        // simply gets the next suffix instead of wasting the transfer.
        loop {
            let destination = choose_destination(&self.output, &self.root_name, self.overwrite)?;
            match install_staged(&self.payload(), &destination, self.overwrite) {
                Ok(warning) => {
                    return Ok(Installed {
                        destination,
                        warning,
                    });
                }
                Err(XferError::Io(error))
                    if !self.overwrite
                        && error.kind() == io::ErrorKind::AlreadyExists
                        && path_exists(&destination)? => {}
                Err(error) => return Err(error),
            }
        }
    }
}

fn remove_existing(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub(crate) fn install_staged(
    stage: &Path,
    destination: &Path,
    overwrite: bool,
) -> Result<Option<String>> {
    if !overwrite {
        install_noclobber(stage, destination)?;
        return Ok(None);
    }
    if !path_exists(destination)? {
        fs::rename(stage, destination)?;
        return Ok(None);
    }

    let parent = destination
        .parent()
        .ok_or_else(|| XferError::invalid_input("destination has no parent directory"))?;
    let backup_directory = tempfile::Builder::new()
        .prefix(".xfer-backup-")
        .tempdir_in(parent)?
        .keep();
    let backup = backup_directory.join("original");

    if let Err(error) = fs::rename(destination, &backup) {
        let _ = fs::remove_dir(&backup_directory);
        return Err(error.into());
    }
    if let Err(install_error) = fs::rename(stage, destination) {
        return match fs::rename(&backup, destination) {
            Ok(()) => {
                let _ = fs::remove_dir(&backup_directory);
                Err(install_error.into())
            }
            Err(rollback_error) => Err(XferError::Io(std::io::Error::new(
                install_error.kind(),
                format!(
                    "could not install {} ({install_error}); the previous destination remains at {} because rollback failed: {rollback_error}",
                    destination.display(),
                    backup.display()
                ),
            ))),
        };
    }

    let cleanup_warning = cleanup_overwrite_backup(&backup, &backup_directory);
    Ok(cleanup_warning)
}

fn install_noclobber(stage: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(stage)?;
    if metadata.is_file() {
        return install_file_noclobber(stage, destination, &metadata);
    }
    if !metadata.is_dir() {
        return Err(XferError::invalid_input(format!(
            "staged path {} is not a file or directory",
            stage.display()
        )));
    }

    fs::create_dir(destination)?;
    let result = (|| {
        for entry in fs::read_dir(stage)? {
            let entry = entry?;
            install_noclobber(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(destination, metadata.permissions())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = remove_existing(destination);
    }
    result
}

fn install_file_noclobber(stage: &Path, destination: &Path, metadata: &fs::Metadata) -> Result<()> {
    match fs::hard_link(stage, destination) {
        Ok(()) => return Ok(()),
        Err(error) if path_exists(destination)? => return Err(error.into()),
        Err(_) => {}
    }

    let mut source = File::open(stage)?;
    let mut target = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let result = (|| {
        io::copy(&mut source, &mut target)?;
        target.flush()?;
        target.sync_all()?;
        fs::set_permissions(destination, metadata.permissions())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(destination);
    }
    result
}

fn cleanup_overwrite_backup(backup: &Path, backup_directory: &Path) -> Option<String> {
    let cleanup = remove_existing(backup).and_then(|()| {
        fs::remove_dir(backup_directory)?;
        Ok(())
    });
    cleanup.err().map(|error| {
        format!(
            "installed the replacement, but could not remove the previous destination at {}: {error}",
            backup.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn overwrite_install_replaces_the_previous_destination() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("payload");
        let stage = directory.path().join("stage");
        fs::write(&destination, b"old").unwrap();
        fs::write(&stage, b"new").unwrap();

        install_staged(&stage, &destination, true).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"new");
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".xfer-backup-")
        }));
    }

    #[test]
    fn failed_overwrite_install_restores_the_previous_destination() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("payload");
        let missing_stage = directory.path().join("missing-stage");
        fs::write(&destination, b"old").unwrap();

        assert!(install_staged(&missing_stage, &destination, true).is_err());

        assert_eq!(fs::read(&destination).unwrap(), b"old");
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".xfer-backup-")
        }));
    }

    #[test]
    fn no_overwrite_install_does_not_replace_a_raced_file() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("payload");
        let stage = directory.path().join("stage");
        fs::write(&destination, b"race winner").unwrap();
        fs::write(&stage, b"incoming").unwrap();

        assert!(install_staged(&stage, &destination, false).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"race winner");
    }

    #[test]
    fn no_overwrite_install_does_not_replace_a_raced_directory() {
        let directory = tempdir().unwrap();
        let destination = directory.path().join("payload");
        let stage = directory.path().join("stage");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("winner.txt"), b"race winner").unwrap();
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("incoming.txt"), b"incoming").unwrap();

        assert!(install_staged(&stage, &destination, false).is_err());
        assert_eq!(
            fs::read(destination.join("winner.txt")).unwrap(),
            b"race winner"
        );
        assert!(!destination.join("incoming.txt").exists());
    }

    #[test]
    fn backup_cleanup_failure_is_reported_after_install_success() {
        let directory = tempdir().unwrap();
        let backup_directory = directory.path().join(".xfer-backup");
        fs::create_dir(&backup_directory).unwrap();
        let warning =
            cleanup_overwrite_backup(&backup_directory.join("missing"), &backup_directory);
        assert!(warning.is_some_and(|message| message.contains("installed the replacement")));
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_replaces_dangling_symlink_without_touching_target() {
        let directory = tempdir().unwrap();
        let stage = directory.path().join("stage");
        let destination = directory.path().join("destination");
        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("file"), b"new").unwrap();
        std::os::unix::fs::symlink("missing", &destination).unwrap();
        install_staged(&stage, &destination, true).unwrap();
        assert_eq!(fs::read(destination.join("file")).unwrap(), b"new");
        assert!(!directory.path().join("missing").exists());
    }
}

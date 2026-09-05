//! Incremental directory updates. Only verified changed files are replaced;
//! destination-only files and identical files remain untouched.
use crate::{
    control::TransferControl,
    crypto::update_manifest,
    delta::{self, BasisHeader, Instruction, Signature, SyncStats},
    error::{Result, XferError},
    filesystem::{TransferPlan, open_planned_file, path_to_wire, safe_relative_path},
    protocol::{
        CHUNK_SIZE, Complete, Decision, EntryEnd, EntryKind, FrameKind, Offer, RecordStream,
        TransferEnd, TransferKind,
    },
    receiver::{PathRegistry, validate_offer},
    reporter::{Progress, Reporter},
    transfer::{ReceiveOptions, SendOptions, TransferSummary},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    net::TcpStream,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
struct SyncEntry {
    path: String,
    kind: EntryKind,
    size: u64,
    sha256: [u8; 32],
}
#[derive(Serialize, Deserialize)]
struct FilePlan {
    sent: u64,
    reused: u64,
    sha256: [u8; 32],
}

pub(crate) fn hash_file(file: &mut File, control: &TransferControl) -> Result<[u8; 32]> {
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; CHUNK_SIZE];
    loop {
        control.check()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(hash.finalize().into())
}

pub(crate) fn send(
    session: &mut RecordStream<TcpStream>,
    plan: &TransferPlan,
    options: &SendOptions,
    reporter: &dyn Reporter,
    control: &TransferControl,
    expected: Option<&std::collections::BTreeMap<String, [u8; 32]>>,
) -> Result<TransferSummary> {
    if plan.kind != TransferKind::Directory {
        return Err(XferError::invalid_input("sync requires a directory"));
    }
    let offer = Offer {
        root_name: plan.root_name.clone(),
        kind: plan.kind,
        total_bytes: plan.total_bytes,
        file_count: plan.file_count,
        entry_count: plan.entries.len() as u64,
        release_version: Some(crate::VERSION.into()),
    };
    session.send_message(
        if options.preview {
            FrameKind::PreviewOffer
        } else {
            FrameKind::SyncOffer
        },
        &offer,
    )?;
    if let Decision::Reject(reason) = session.receive_message(FrameKind::Decision)? {
        return Err(XferError::Rejected(reason));
    }
    let mut stats = SyncStats::default();
    let mut manifest = Sha256::new();
    let mut done = 0;
    for entry in &plan.entries {
        control.check()?;
        let path = path_to_wire(&entry.relative)?;
        if entry.kind == EntryKind::Directory {
            session.send_message(
                FrameKind::EntryStart,
                &SyncEntry {
                    path,
                    kind: entry.kind,
                    size: 0,
                    sha256: [0; 32],
                },
            )?;
            continue;
        }
        reporter.status(&format!("Comparing {path}"));
        let mut file = open_planned_file(entry, options.follow_links)?;
        let digest = hash_file(&mut file, control)?;
        if expected.is_some_and(|files| files.get(&path) != Some(&digest)) {
            return Err(XferError::security(
                "source changed after two-way comparison; retry",
            ));
        }
        session.send_message(
            FrameKind::EntryStart,
            &SyncEntry {
                path: path.clone(),
                kind: entry.kind,
                size: entry.size,
                sha256: digest,
            },
        )?;
        let basis: BasisHeader = session.receive_message(FrameKind::Basis)?;
        if basis.count > delta::MAX_BLOCKS {
            return Err(XferError::protocol("too many basis signatures"));
        }
        let mut blocks = Vec::with_capacity(basis.count);
        while blocks.len() < basis.count {
            let page: Vec<Signature> = session.receive_message(FrameKind::Basis)?;
            if page.is_empty() || page.len() > 256 || page.len() > basis.count - blocks.len() {
                return Err(XferError::protocol("invalid signature page"));
            }
            blocks.extend(page);
        }
        delta::validate_basis(&basis, &blocks)?;
        if basis.unchanged {
            if basis.size != entry.size || basis.count != 0 {
                return Err(XferError::protocol("invalid unchanged-file response"));
            }
            stats.unchanged_files += 1;
            stats.reused_bytes += entry.size;
            session.send_message(FrameKind::EntryEnd, &EntryEnd { sha256: digest })?;
        } else {
            let (delta, actual) = delta::encode(file, &basis, &blocks, control, |instruction| {
                if options.preview {
                    return Ok(());
                }
                match instruction {
                    Instruction::Literal(bytes) => session.send_frame(FrameKind::Data, bytes),
                    Instruction::Reuse(index) => session.send_message(FrameKind::Reuse, &index),
                }
            })?;
            if actual != digest || delta.sent_bytes + delta.reused_bytes != entry.size {
                return Err(XferError::invalid_input(
                    "source changed while syncing; retry",
                ));
            }
            if options.preview {
                session.send_message(
                    FrameKind::FilePlan,
                    &FilePlan {
                        sent: delta.sent_bytes,
                        reused: delta.reused_bytes,
                        sha256: digest,
                    },
                )?;
            } else {
                session.send_message(FrameKind::EntryEnd, &EntryEnd { sha256: digest })?;
            }
            stats.changed_files += 1;
            stats.sent_bytes += delta.sent_bytes;
            stats.reused_bytes += delta.reused_bytes;
        }
        update_manifest(&mut manifest, &path, &digest);
        done += 1;
        reporter.progress(&Progress {
            phase: if options.preview {
                "Comparing"
            } else {
                "Syncing"
            },
            current_path: path,
            transferred: stats.sent_bytes + stats.reused_bytes,
            total: plan.total_bytes,
            files_done: done,
            files_total: plan.file_count,
        });
    }
    session.send_message(
        FrameKind::TransferEnd,
        &TransferEnd {
            file_count: plan.file_count,
            total_bytes: plan.total_bytes,
            manifest_sha256: manifest.finalize().into(),
        },
    )?;
    let complete: Complete = session.receive_message(FrameKind::Complete)?;
    if complete.file_count != plan.file_count
        || complete.total_bytes != plan.total_bytes
        || complete.sync_stats != Some(stats)
        || complete.preview != options.preview
    {
        return Err(XferError::security(
            "sync completion did not match the sent data",
        ));
    }
    Ok(TransferSummary {
        destination: complete.destination.into(),
        file_count: plan.file_count,
        total_bytes: plan.total_bytes,
        peer: session.get_mut().peer_addr()?,
        peer_version: complete.release_version,
        sync_stats: Some(stats),
        preview: options.preview,
        conflicts: Vec::new(),
    })
}

/// Refuse symlinks and type changes throughout the managed tree. The configured
/// output directory itself may be a user-chosen symlink; its children may not.
pub(crate) fn checked_target(root: &Path, relative: &Path) -> Result<PathBuf> {
    let mut current = root.to_path_buf();
    let mut paths = vec![current.clone()];
    for part in relative.components() {
        current.push(part);
        paths.push(current.clone());
    }
    for (index, path) in paths.iter().enumerate() {
        match fs::symlink_metadata(path) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    || (index + 1 < paths.len() && !metadata.is_dir()) =>
            {
                return Err(XferError::security(format!(
                    "sync path is a link or conflicting file: {}",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(current)
}

#[derive(PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}
fn stamp(path: &Path) -> Result<Option<Stamp>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(XferError::security(
                    "sync file conflicts with a directory or symlink",
                ));
            }
            Ok(Some(Stamp {
                size: metadata.len(),
                modified: metadata.modified().ok(),
                #[cfg(unix)]
                dev: metadata.dev(),
                #[cfg(unix)]
                ino: metadata.ino(),
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn receive(
    session: &mut RecordStream<TcpStream>,
    offer: Offer,
    options: &ReceiveOptions,
    preview: bool,
    reporter: &dyn Reporter,
    control: &TransferControl,
    expected: Option<&std::collections::BTreeMap<String, [u8; 32]>>,
) -> Result<TransferSummary> {
    if !options.allow_sync {
        return Err(XferError::Rejected(
            "receiver must enable sync updates (receive --sync)".into(),
        ));
    }
    validate_offer(&offer)?;
    if offer.kind != TransferKind::Directory {
        return Err(XferError::protocol("sync requires a directory"));
    }
    if !preview {
        fs::create_dir_all(&options.output)?;
    }
    let output = if options.output.exists() {
        fs::canonicalize(&options.output)?
    } else {
        options.output.clone()
    };
    let root = output.join(&offer.root_name);
    checked_target(&root, Path::new(""))?;
    if root.exists() && !root.is_dir() {
        return Err(XferError::invalid_input(
            "sync destination is not a directory",
        ));
    }
    if !preview {
        fs::create_dir_all(&root)?;
    }
    session.send_message(FrameKind::Decision, &Decision::Accept)?;
    let mut registry = PathRegistry::default();
    let mut stats = SyncStats::default();
    let mut manifest = Sha256::new();
    let mut bytes = 0;
    let mut files = 0;
    for _ in 0..offer.entry_count {
        control.check()?;
        let entry: SyncEntry = session.receive_message(FrameKind::EntryStart)?;
        if entry.path.len() > 4096 {
            return Err(XferError::protocol("sync path too long"));
        }
        let relative = safe_relative_path(&entry.path)?;
        registry.insert(&relative, entry.kind)?;
        let target = checked_target(&root, &relative)?;
        if entry.kind == EntryKind::Directory {
            if entry.size != 0 {
                return Err(XferError::protocol("directory has nonzero size"));
            }
            if target.exists() && !target.is_dir() {
                return Err(XferError::security("sync directory conflicts with file"));
            }
            if !preview {
                fs::create_dir_all(target)?;
            }
            continue;
        }
        if files >= offer.file_count || entry.size > offer.total_bytes - bytes {
            return Err(XferError::protocol("sync entry exceeds offer totals"));
        }
        reporter.status(&format!("Comparing {}", entry.path));
        let before = stamp(&target)?;
        let mut basis = if before.is_some() {
            Some(File::open(&target)?)
        } else {
            None
        };
        let (mut header, blocks, basis_hash) = if let Some(file) = &mut basis {
            delta::signatures(file, before.as_ref().unwrap().size, control)?
        } else {
            (
                BasisHeader {
                    size: 0,
                    block_size: u32::try_from(delta::MIN_BLOCK).expect("bounded block size"),
                    count: 0,
                    unchanged: false,
                },
                Vec::new(),
                [0; 32],
            )
        };
        if let Some(expected) = expected {
            let actual = before.as_ref().map(|_| &basis_hash);
            if actual != expected.get(&entry.path) {
                return Err(XferError::security(
                    "destination changed after two-way comparison; original preserved",
                ));
            }
        }
        header.unchanged =
            before.is_some() && header.size == entry.size && basis_hash == entry.sha256;
        if header.unchanged {
            header.count = 0;
        }
        session.send_message(FrameKind::Basis, &header)?;
        if !header.unchanged {
            for page in blocks.chunks(256) {
                session.send_message(FrameKind::Basis, &page)?;
            }
        }
        if header.unchanged {
            let end: EntryEnd = session.receive_message(FrameKind::EntryEnd)?;
            if end.sha256 != entry.sha256 || stamp(&target)? != before {
                return Err(XferError::security("unchanged file changed during sync"));
            }
            stats.unchanged_files += 1;
            stats.reused_bytes += entry.size;
        } else if preview {
            let plan: FilePlan = session.receive_message(FrameKind::FilePlan)?;
            if plan.sent.checked_add(plan.reused) != Some(entry.size) || plan.sha256 != entry.sha256
            {
                return Err(XferError::protocol("invalid sync preview totals"));
            }
            stats.changed_files += 1;
            stats.sent_bytes += plan.sent;
            stats.reused_bytes += plan.reused;
        } else {
            let mut staged = tempfile::NamedTempFile::new_in(&output)?;
            let mut received = 0;
            let mut hash = Sha256::new();
            let mut payload = Vec::new();
            loop {
                control.check()?;
                let kind = session.receive_frame_into(&mut payload)?;
                match kind {
                    FrameKind::Data => {
                        if payload.is_empty() || payload.len() as u64 > entry.size - received {
                            return Err(XferError::protocol("invalid sync literal size"));
                        }
                        staged.write_all(&payload)?;
                        hash.update(&payload);
                        received += payload.len() as u64;
                        stats.sent_bytes += payload.len() as u64;
                    }
                    FrameKind::Reuse => {
                        let index: u32 = serde_json::from_slice(&payload)
                            .map_err(|e| XferError::Serialization(e.to_string()))?;
                        let signature = blocks
                            .get(index as usize)
                            .ok_or_else(|| XferError::protocol("invalid reused block index"))?;
                        if u64::from(signature.length) > entry.size - received {
                            return Err(XferError::protocol("reused block exceeds file size"));
                        }
                        let file = basis
                            .as_mut()
                            .ok_or_else(|| XferError::protocol("no basis file to reuse"))?;
                        file.seek(SeekFrom::Start(
                            u64::from(index) * u64::from(header.block_size),
                        ))?;
                        let mut block = vec![0; signature.length as usize];
                        file.read_exact(&mut block)?;
                        if <[u8; 32]>::from(Sha256::digest(&block)) != signature.strong {
                            return Err(XferError::security(
                                "destination changed while reusing blocks; retry",
                            ));
                        }
                        staged.write_all(&block)?;
                        hash.update(&block);
                        received += block.len() as u64;
                        stats.reused_bytes += block.len() as u64;
                    }
                    FrameKind::EntryEnd => {
                        let end: EntryEnd = serde_json::from_slice(&payload)
                            .map_err(|e| XferError::Serialization(e.to_string()))?;
                        if received != entry.size
                            || end.sha256 != entry.sha256
                            || <[u8; 32]>::from(hash.finalize()) != entry.sha256
                        {
                            return Err(XferError::security("synced file digest did not verify"));
                        }
                        break;
                    }
                    _ => return Err(XferError::protocol("unexpected frame while syncing file")),
                }
            }
            staged.as_file().sync_all()?;
            control.check()?;
            checked_target(&root, &relative)?;
            if stamp(&target)? != before {
                return Err(XferError::security(
                    "destination changed during sync; original preserved",
                ));
            }
            if before.is_some() {
                staged
                    .as_file()
                    .set_permissions(fs::metadata(&target)?.permissions())?;
            }
            drop(basis);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            // Close the temporary file before rename on Windows.
            let stage = staged.into_temp_path();
            if let Some(warning) =
                crate::storage::install_staged(&stage, &target, before.is_some())?
            {
                reporter.status(&warning);
            }
            stats.changed_files += 1;
        }
        files += 1;
        bytes += entry.size;
        update_manifest(&mut manifest, &entry.path, &entry.sha256);
        reporter.progress(&Progress {
            phase: if preview { "Comparing" } else { "Syncing" },
            current_path: entry.path,
            transferred: bytes,
            total: offer.total_bytes,
            files_done: files,
            files_total: offer.file_count,
        });
    }
    let end: TransferEnd = session.receive_message(FrameKind::TransferEnd)?;
    if end.file_count != files
        || files != offer.file_count
        || end.total_bytes != bytes
        || bytes != offer.total_bytes
        || end.manifest_sha256 != <[u8; 32]>::from(manifest.finalize())
    {
        return Err(XferError::security("sync manifest did not verify"));
    }
    session.send_message(
        FrameKind::Complete,
        &Complete {
            destination: root.display().to_string(),
            file_count: files,
            total_bytes: bytes,
            release_version: Some(crate::VERSION.into()),
            sync_stats: Some(stats),
            preview,
        },
    )?;
    Ok(TransferSummary {
        destination: root,
        file_count: files,
        total_bytes: bytes,
        peer: session.get_mut().peer_addr()?,
        peer_version: offer.release_version,
        sync_stats: Some(stats),
        preview,
        conflicts: Vec::new(),
    })
}

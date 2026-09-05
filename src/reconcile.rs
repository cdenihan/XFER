//! Two-way reconciliation against the previous successful common file hashes.
//! Deletions are intentionally not propagated. Conflicting edits stay on both
//! machines and are reported for an explicit user resolution.
use crate::{
    control::TransferControl,
    delta::SyncStats,
    error::{Result, XferError},
    filesystem::{TransferPlan, build_plan, open_planned_file, path_to_wire, safe_relative_path},
    protocol::{EntryKind, FrameKind, Offer, RecordStream, TransferKind},
    receiver::{PathRegistry, validate_offer},
    reporter::Reporter,
    transfer::{ReceiveOptions, SendOptions, TransferSummary},
};
use rust_cli_toolkit::{LockedJsonStore, SecureDir};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::TcpStream,
    path::Path,
    time::Duration,
};

const MAX_INVENTORY: usize = 100_000;
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Item {
    path: String,
    kind: EntryKind,
    size: u64,
    digest: [u8; 32],
}
#[derive(Serialize, Deserialize)]
struct InventoryHeader {
    count: usize,
    root: String,
}
#[derive(Serialize, Deserialize)]
struct Request {
    excludes: Vec<String>,
}
#[derive(Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Baseline {
    files: BTreeMap<String, [u8; 32]>,
}
#[derive(Default, Debug)]
struct Choices {
    push: BTreeSet<String>,
    pull: BTreeSet<String>,
    conflicts: Vec<String>,
}

fn inventory(plan: &TransferPlan, control: &TransferControl) -> Result<Vec<Item>> {
    if plan.entries.len() > MAX_INVENTORY {
        return Err(XferError::invalid_input(
            "two-way sync supports up to 100,000 entries",
        ));
    }
    plan.entries
        .iter()
        .map(|entry| {
            control.check()?;
            Ok(Item {
                path: path_to_wire(&entry.relative)?,
                kind: entry.kind,
                size: entry.size,
                digest: if entry.kind == EntryKind::File {
                    crate::sync::hash_file(&mut open_planned_file(entry, false)?, control)?
                } else {
                    [0; 32]
                },
            })
        })
        .collect()
}
fn files(items: &[Item]) -> BTreeMap<String, [u8; 32]> {
    items
        .iter()
        .filter(|item| item.kind == EntryKind::File)
        .map(|item| (item.path.clone(), item.digest))
        .collect()
}
fn choose(local: &[Item], remote: &[Item], baseline: &Baseline) -> Choices {
    let left = local
        .iter()
        .map(|item| (item.path.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    let right = remote
        .iter()
        .map(|item| (item.path.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    let names = left
        .keys()
        .chain(right.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut choices = Choices::default();
    for path in names {
        if choices
            .conflicts
            .iter()
            .any(|parent| path.starts_with(&format!("{parent}/")))
        {
            continue;
        }
        match (left.get(path), right.get(path)) {
            (Some(a), Some(b)) if a.kind != b.kind => choices.conflicts.push(path.into()),
            (Some(a), Some(b)) if a.kind == EntryKind::Directory || a.digest == b.digest => {}
            (Some(a), Some(b)) => match baseline.files.get(path) {
                Some(old) if old == &b.digest => {
                    choices.push.insert(path.into());
                }
                Some(old) if old == &a.digest => {
                    choices.pull.insert(path.into());
                }
                _ => choices.conflicts.push(path.into()),
            },
            (Some(_), None) => {
                choices.push.insert(path.into());
            }
            (None, Some(_)) => {
                choices.pull.insert(path.into());
            }
            _ => {}
        }
    }
    choices
}
fn subset(plan: &TransferPlan, selected: &BTreeSet<String>) -> Result<TransferPlan> {
    let mut result = plan.clone();
    result.entries.clear();
    result.file_count = 0;
    result.total_bytes = 0;
    for entry in &plan.entries {
        if selected.contains(&path_to_wire(&entry.relative)?) {
            if entry.kind == EntryKind::File {
                result.file_count += 1;
                result.total_bytes += entry.size;
            }
            result.entries.push(entry.clone());
        }
    }
    Ok(result)
}
fn add(a: SyncStats, b: SyncStats) -> SyncStats {
    SyncStats {
        sent_bytes: a.sent_bytes + b.sent_bytes,
        reused_bytes: a.reused_bytes + b.reused_bytes,
        changed_files: a.changed_files + b.changed_files,
        unchanged_files: a.unchanged_files + b.unchanged_files,
    }
}

pub(crate) fn send(
    session: &mut RecordStream<TcpStream>,
    plan: &TransferPlan,
    options: &SendOptions,
    reporter: &dyn Reporter,
    control: &TransferControl,
) -> Result<TransferSummary> {
    if plan.kind != TransferKind::Directory || options.follow_links {
        return Err(XferError::invalid_input(
            "two-way sync requires a directory and does not follow symlinks",
        ));
    }
    reporter.status("Comparing both folders with the last sync…");
    let local = inventory(plan, control)?;
    session.send_message(
        if options.preview {
            FrameKind::TwoWayPreview
        } else {
            FrameKind::TwoWayOffer
        },
        &Offer {
            root_name: plan.root_name.clone(),
            kind: plan.kind,
            total_bytes: plan.total_bytes,
            file_count: plan.file_count,
            entry_count: plan.entries.len() as u64,
            release_version: Some(crate::VERSION.into()),
        },
    )?;
    session.send_message(
        FrameKind::FilePlan,
        &Request {
            excludes: options.excludes.clone(),
        },
    )?;
    let header: InventoryHeader = session.receive_message(FrameKind::Basis)?;
    if header.count > MAX_INVENTORY || header.root.len() > 4096 {
        return Err(XferError::protocol("invalid two-way inventory"));
    }
    let mut remote = Vec::<Item>::new();
    let mut registry = PathRegistry::default();
    while remote.len() < header.count {
        let page: Vec<Item> = session.receive_message(FrameKind::Basis)?;
        if page.is_empty() || page.len() > 128 || page.len() > header.count - remote.len() {
            return Err(XferError::protocol("invalid inventory page"));
        }
        for item in &page {
            if item.path.len() > 4096 {
                return Err(XferError::protocol("inventory path too long"));
            }
            registry.insert(&safe_relative_path(&item.path)?, item.kind)?;
        }
        remote.extend(page);
    }
    let source = fs::canonicalize(&options.input)?;
    let key = format!(
        "{}\n{}\n{}",
        session.get_mut().peer_addr()?,
        source.display(),
        header.root
    );
    let name = format!("sync-{}.json", hex::encode(Sha256::digest(key.as_bytes())));
    let directory = SecureDir::discover("xfer", options.config_dir.clone())?;
    let store = LockedJsonStore::<Baseline>::new(directory, &name);
    let baseline = store.load()?;
    let mut choices = choose(&local, &remote, &baseline);
    if options.conflict_policy != crate::transfer::ConflictPolicy::Preserve {
        choices.conflicts.retain(|path| {
            let local_file = local
                .iter()
                .find(|item| &item.path == path && item.kind == EntryKind::File);
            let remote_file = remote
                .iter()
                .find(|item| &item.path == path && item.kind == EntryKind::File);
            if local_file.is_some() && remote_file.is_some() {
                if options.conflict_policy == crate::transfer::ConflictPolicy::PreferLocal {
                    choices.push.insert(path.clone());
                } else {
                    choices.pull.insert(path.clone());
                }
                false
            } else {
                true
            }
        });
    }
    for conflict in &choices.conflicts {
        reporter.status(&format!("Conflict: {conflict} — both versions preserved"));
    }
    session.send_message(FrameKind::FilePlan, &choices.pull.len())?;
    let selected = choices.pull.iter().collect::<Vec<_>>();
    for page in selected.chunks(128) {
        session.send_message(FrameKind::FilePlan, &page)?;
    }
    let local_files = files(&local);
    let remote_files = files(&remote);
    let push = subset(plan, &choices.push)?;
    let mut sent = crate::sync::send(
        session,
        &push,
        options,
        reporter,
        control,
        Some(&local_files),
    )?;
    let (kind, payload) = session.receive_frame()?;
    if kind
        != if options.preview {
            FrameKind::PreviewOffer
        } else {
            FrameKind::SyncOffer
        }
    {
        return Err(XferError::protocol("expected reverse sync offer"));
    }
    let reverse: Offer =
        serde_json::from_slice(&payload).map_err(|e| XferError::Serialization(e.to_string()))?;
    if reverse.root_name != plan.root_name {
        return Err(XferError::protocol(
            "reverse sync changed the destination folder",
        ));
    }
    let receive_options = ReceiveOptions {
        allow_sync: true,
        output: source
            .parent()
            .ok_or_else(|| XferError::invalid_input("cannot sync filesystem root"))?
            .into(),
        bind: String::new(),
        port: 0,
        overwrite: false,
        discoverable: false,
        secure: options.secure,
        token: None,
        config_dir: options.config_dir.clone(),
    };
    let received = crate::sync::receive(
        session,
        reverse,
        &receive_options,
        options.preview,
        reporter,
        control,
        Some(&local_files),
    )?;
    if !options.preview {
        let mut next = baseline.clone();
        for (path, digest) in &local_files {
            if remote_files.get(path) == Some(digest) || choices.push.contains(path) {
                next.files.insert(path.clone(), *digest);
            }
        }
        for path in &choices.pull {
            if let Some(digest) = remote_files.get(path) {
                next.files.insert(path.clone(), *digest);
            }
        }
        store.update(|current| {
            if *current != baseline {
                return Err(rust_cli_toolkit::Error::Configuration(
                    "another two-way sync changed the baseline; retry".into(),
                ));
            }
            *current = next;
            Ok(())
        })?;
    }
    let unchanged = local
        .iter()
        .filter(|item| {
            item.kind == EntryKind::File && remote_files.get(&item.path) == Some(&item.digest)
        })
        .collect::<Vec<_>>();
    let unchanged_bytes = unchanged.iter().map(|item| item.size).sum::<u64>();
    let mut stats = add(
        sent.sync_stats.unwrap_or_default(),
        received.sync_stats.unwrap_or_default(),
    );
    stats.unchanged_files += unchanged.len() as u64;
    stats.reused_bytes += unchanged_bytes;
    sent.sync_stats = Some(stats);
    sent.file_count += received.file_count + unchanged.len() as u64;
    sent.total_bytes += received.total_bytes + unchanged_bytes;
    sent.conflicts = choices.conflicts;
    Ok(sent)
}

pub(crate) fn receive(
    session: &mut RecordStream<TcpStream>,
    offer: Offer,
    options: &ReceiveOptions,
    preview: bool,
    reporter: &dyn Reporter,
    control: &TransferControl,
) -> Result<TransferSummary> {
    if !options.allow_sync {
        return Err(XferError::Rejected(
            "receiver must enable sync updates (receive --sync)".into(),
        ));
    }
    validate_offer(&offer)?;
    if offer.kind != TransferKind::Directory {
        return Err(XferError::protocol("two-way sync requires a directory"));
    }
    let request: Request = session.receive_message(FrameKind::FilePlan)?;
    if request.excludes.len() > 256 {
        return Err(XferError::protocol("too many exclusion patterns"));
    }
    let root = options.output.join(&offer.root_name);
    crate::sync::checked_target(&root, Path::new(""))?;
    let plan = if root.exists() {
        build_plan(&root, &request.excludes, false)?
    } else {
        TransferPlan {
            root_name: offer.root_name.clone(),
            kind: TransferKind::Directory,
            entries: Vec::new(),
            total_bytes: 0,
            file_count: 0,
            skipped_count: 0,
        }
    };
    if plan.kind != TransferKind::Directory {
        return Err(XferError::security(
            "two-way destination is not a directory",
        ));
    }
    let remote = inventory(&plan, control)?;
    let root_identity = if root.exists() {
        fs::canonicalize(&root)?
    } else {
        fs::canonicalize(&options.output)
            .unwrap_or_else(|_| options.output.clone())
            .join(&offer.root_name)
    };
    session.send_message(
        FrameKind::Basis,
        &InventoryHeader {
            count: remote.len(),
            root: root_identity.display().to_string(),
        },
    )?;
    for page in remote.chunks(128) {
        session.send_message(FrameKind::Basis, &page)?;
    }
    let count: usize = session.receive_message(FrameKind::FilePlan)?;
    if count > remote.len() {
        return Err(XferError::protocol("reverse sync requested too many paths"));
    }
    let allowed = remote
        .iter()
        .map(|item| item.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut selected = BTreeSet::new();
    while selected.len() < count {
        let page: Vec<String> = session.receive_message(FrameKind::FilePlan)?;
        if page.is_empty() || page.len() > 128 || page.len() > count - selected.len() {
            return Err(XferError::protocol("invalid reverse sync selection"));
        }
        for path in page {
            if !allowed.contains(path.as_str()) || !selected.insert(path) {
                return Err(XferError::protocol("invalid reverse sync path"));
            }
        }
    }
    let remote_files = files(&remote);
    let (kind, payload) = session.receive_frame()?;
    if kind
        != if preview {
            FrameKind::PreviewOffer
        } else {
            FrameKind::SyncOffer
        }
    {
        return Err(XferError::protocol("expected forward sync offer"));
    }
    let forward: Offer =
        serde_json::from_slice(&payload).map_err(|e| XferError::Serialization(e.to_string()))?;
    if forward.root_name != offer.root_name {
        return Err(XferError::protocol("forward sync changed destination"));
    }
    let mut received = crate::sync::receive(
        session,
        forward,
        options,
        preview,
        reporter,
        control,
        Some(&remote_files),
    )?;
    let reverse = subset(&plan, &selected)?;
    let send_options = SendOptions {
        conflict_policy: crate::transfer::ConflictPolicy::Preserve,
        sync: true,
        preview,
        two_way: false,
        input: root,
        host: String::new(),
        port: 0,
        excludes: Vec::new(),
        follow_links: false,
        secure: options.secure,
        token: None,
        connect_timeout: Duration::from_secs(30),
        config_dir: options.config_dir.clone(),
    };
    let sent = crate::sync::send(
        session,
        &reverse,
        &send_options,
        reporter,
        control,
        Some(&remote_files),
    )?;
    received.sync_stats = Some(add(
        received.sync_stats.unwrap_or_default(),
        sent.sync_stats.unwrap_or_default(),
    ));
    received.file_count += sent.file_count;
    received.total_bytes += sent.total_bytes;
    Ok(received)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(path: &str, digest: u8) -> Item {
        Item {
            path: path.into(),
            kind: EntryKind::File,
            size: 1,
            digest: [digest; 32],
        }
    }
    #[test]
    fn two_way_choices_propagate_single_sided_edits_and_preserve_conflicts() {
        let local = vec![
            item("push", 2),
            item("pull", 1),
            item("conflict", 2),
            item("new-local", 3),
        ];
        let remote = vec![
            item("push", 1),
            item("pull", 2),
            item("conflict", 3),
            item("new-remote", 4),
        ];
        let baseline = Baseline {
            files: [
                ("push".into(), [1; 32]),
                ("pull".into(), [1; 32]),
                ("conflict".into(), [1; 32]),
            ]
            .into(),
        };
        let choices = choose(&local, &remote, &baseline);
        assert_eq!(
            choices.push,
            BTreeSet::from(["push".into(), "new-local".into()])
        );
        assert_eq!(
            choices.pull,
            BTreeSet::from(["pull".into(), "new-remote".into()])
        );
        assert_eq!(choices.conflicts, vec!["conflict"]);
        assert_eq!(
            choose(&[item("same", 1)], &[item("same", 2)], &Baseline::default()).conflicts,
            vec!["same"]
        );
    }
}

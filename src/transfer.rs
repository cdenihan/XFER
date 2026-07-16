use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::{
    config::{Identity, Paths, TrustStore},
    crypto::{derive_session_keys, display_fingerprint, fingerprint, sas},
    discovery::Advertiser,
    error::{Result, XferError},
    filesystem::{
        TransferPlan, build_plan, choose_destination, open_planned_file, path_to_wire,
        portable_path_key, safe_relative_path, validate_wire_name,
    },
    net,
    protocol::{
        CHUNK_SIZE, ClientHello, Complete, Decision, EntryEnd, EntryKind, EntryStart, FrameKind,
        Offer, RecordStream, Role, ServerHello, TransferEnd, TransferKind, client_negotiate,
        read_client_hello, read_server_hello, sanitize_peer_text, server_negotiate,
        write_client_hello, write_server_hello,
    },
    reporter::{Progress, Reporter, TrustPrompt},
};

#[derive(Clone, Debug)]
pub struct SendOptions {
    pub host: String,
    pub port: u16,
    pub input: PathBuf,
    pub excludes: Vec<String>,
    pub follow_links: bool,
    pub secure: bool,
    pub token: Option<String>,
    pub connect_timeout: Duration,
    pub config_dir: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct ReceiveOptions {
    pub bind: String,
    pub port: u16,
    pub output: PathBuf,
    pub overwrite: bool,
    pub discoverable: bool,
    pub secure: bool,
    pub token: Option<String>,
    pub config_dir: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct TransferSummary {
    pub destination: PathBuf,
    pub file_count: u64,
    pub total_bytes: u64,
    pub peer: SocketAddr,
}

pub fn send(options: &SendOptions, reporter: &dyn Reporter) -> Result<TransferSummary> {
    validate_secure_token(options.secure, options.token.as_deref())?;
    let plan = build_plan(&options.input, &options.excludes, options.follow_links)?;
    reporter.status(&format_plan(&plan));
    let (stream, deadline) =
        net::connect_with_deadline(&options.host, options.port, options.connect_timeout)?;
    let peer = stream.peer_addr()?;
    reporter.status(&format!("connected to {peer}"));
    let paths = Paths::discover(options.config_dir.clone())?;
    let mut session = establish_client(
        stream,
        options.secure,
        options.token.as_deref(),
        &paths,
        reporter,
        deadline,
    )?;

    let offer = Offer {
        root_name: plan.root_name.clone(),
        kind: plan.kind,
        total_bytes: plan.total_bytes,
        file_count: plan.file_count,
        entry_count: plan.entries.len() as u64,
    };
    session.send_message(FrameKind::Offer, &offer)?;
    match session.receive_message::<Decision>(FrameKind::Decision)? {
        Decision::Accept => {}
        Decision::Reject(reason) => {
            return Err(XferError::Rejected(sanitize_peer_text(&reason)));
        }
    }

    let mut transferred = 0_u64;
    let mut files_done = 0_u64;
    let mut manifest = Sha256::new();
    let mut buffer = vec![0_u8; CHUNK_SIZE];
    let mut last_progress = Instant::now();

    for entry in &plan.entries {
        let wire_path = path_to_wire(&entry.relative)?;
        session.send_message(
            FrameKind::EntryStart,
            &EntryStart {
                path: wire_path.clone(),
                kind: entry.kind,
                size: entry.size,
            },
        )?;
        if entry.kind == EntryKind::Directory {
            continue;
        }

        let mut file = open_planned_file(entry, options.follow_links)?;
        let mut hash = Sha256::new();
        loop {
            let count = read_retry(&mut file, &mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            session.send_frame(FrameKind::Data, &buffer[..count])?;
            transferred += count as u64;
            if last_progress.elapsed() >= PROGRESS_INTERVAL {
                reporter.progress(&Progress {
                    phase: "Sending",
                    current_path: wire_path.clone(),
                    transferred,
                    total: plan.total_bytes,
                    files_done,
                    files_total: plan.file_count,
                });
                last_progress = Instant::now();
            }
        }
        let digest: [u8; 32] = hash.finalize().into();
        update_manifest(&mut manifest, &wire_path, &digest);
        session.send_message(FrameKind::EntryEnd, &EntryEnd { sha256: digest })?;
        files_done += 1;
        reporter.progress(&Progress {
            phase: "Sending",
            current_path: wire_path,
            transferred,
            total: plan.total_bytes,
            files_done,
            files_total: plan.file_count,
        });
    }

    let manifest_sha256 = manifest.finalize().into();
    session.send_message(
        FrameKind::TransferEnd,
        &TransferEnd {
            file_count: files_done,
            total_bytes: transferred,
            manifest_sha256,
        },
    )?;
    let complete: Complete = session.receive_message(FrameKind::Complete)?;
    validate_completion(&complete, files_done, transferred)?;
    reporter.status(&format!(
        "receiver verified {} across {} file(s)",
        human_bytes(complete.total_bytes),
        complete.file_count
    ));
    Ok(TransferSummary {
        destination: PathBuf::from(complete.destination),
        file_count: complete.file_count,
        total_bytes: complete.total_bytes,
        peer,
    })
}

pub fn receive(options: &ReceiveOptions, reporter: &dyn Reporter) -> Result<TransferSummary> {
    validate_receive_options(options)?;
    let listener = net::bind(&options.bind, options.port)?;
    let local = listener.local_addr()?;
    let port = local.port();
    match net::listener_endpoints(local.ip(), port) {
        Ok(endpoints) if !endpoints.is_empty() => {
            reporter.status(&format!(
                "receiver addresses: {}",
                endpoints
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        Ok(_) => {}
        Err(error) => {
            reporter.status(&format!(
                "could not enumerate receiver addresses; use the bind address manually: {error}"
            ));
        }
    }
    let _advertiser = if options.discoverable {
        match Advertiser::start(port, options.secure, local.ip()) {
            Ok(advertiser) => {
                reporter.status("LAN discovery is on (local multicast only)");
                Some(advertiser)
            }
            Err(error) => {
                reporter.status(&format!(
                    "LAN discovery unavailable; manual IP entry still works: {error}"
                ));
                None
            }
        }
    } else {
        reporter.status("LAN discovery is off");
        None
    };
    receive_on_listener(&listener, options, reporter)
}

pub fn receive_on_listener(
    listener: &TcpListener,
    options: &ReceiveOptions,
    reporter: &dyn Reporter,
) -> Result<TransferSummary> {
    validate_receive_options(options)?;
    let local = listener.local_addr()?;
    reporter.status(&format!("listening on {local}"));
    let (stream, peer) = listener.accept()?;
    net::configure_stream(&stream)?;
    reporter.status(&format!("connection from {peer}"));
    let paths = Paths::discover(options.config_dir.clone())?;
    let mut session = establish_server(
        stream,
        options.secure,
        options.token.as_deref(),
        &paths,
        reporter,
    )?;

    match receive_transfer(&mut session, options, reporter, peer) {
        Ok(summary) => Ok(summary),
        Err(error) => {
            let _ = session.send_error(&error.to_string());
            Err(error)
        }
    }
}

fn receive_transfer(
    session: &mut RecordStream<TcpStream>,
    options: &ReceiveOptions,
    reporter: &dyn Reporter,
    peer: SocketAddr,
) -> Result<TransferSummary> {
    let offer: Offer = session.receive_message(FrameKind::Offer)?;
    net::restore_read_timeout(session.get_mut())?;
    validate_offer(&offer)?;
    session.send_message(FrameKind::Decision, &Decision::Accept)?;
    reporter.status(&format!(
        "receiving {} ({}, {} file(s))",
        offer.root_name,
        human_bytes(offer.total_bytes),
        offer.file_count
    ));

    fs::create_dir_all(&options.output)?;
    let destination = choose_destination(&options.output, &offer.root_name, options.overwrite)?;
    let staging = tempfile::Builder::new()
        .prefix(".xfer-stage-")
        .tempdir_in(&options.output)?;
    let stage_path = staging.path().join("payload");
    if offer.kind == TransferKind::Directory {
        fs::create_dir(&stage_path)?;
    }

    let mut transferred = 0_u64;
    let mut files_done = 0_u64;
    let mut manifest = Sha256::new();
    let mut seen_paths = HashSet::new();
    let mut payload = Vec::with_capacity(CHUNK_SIZE + 16);
    let mut last_progress = Instant::now();
    for _ in 0..offer.entry_count {
        let entry: EntryStart = session.receive_message(FrameKind::EntryStart)?;
        let relative = safe_relative_path(&entry.path)?;
        let portable_path = portable_path_key(&relative)?;
        if !seen_paths.insert(portable_path) {
            return Err(XferError::protocol(format!(
                "duplicate entry path: {}",
                entry.path
            )));
        }
        let target = match offer.kind {
            TransferKind::File => {
                if entry.kind != EntryKind::File || entry.path != offer.root_name {
                    return Err(XferError::protocol(
                        "file transfer contained an unexpected entry",
                    ));
                }
                stage_path.clone()
            }
            TransferKind::Directory => stage_path.join(relative),
        };

        match entry.kind {
            EntryKind::Directory => {
                if entry.size != 0 {
                    return Err(XferError::protocol("directory entry has a non-zero size"));
                }
                fs::create_dir_all(&target)?;
            }
            EntryKind::File => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut file = File::create(&target)?;
                let mut received_for_file = 0_u64;
                let mut hash = Sha256::new();
                loop {
                    let kind = session.receive_frame_into(&mut payload)?;
                    match kind {
                        FrameKind::Data => {
                            received_for_file = received_for_file
                                .checked_add(payload.len() as u64)
                                .ok_or_else(|| XferError::protocol("file size overflow"))?;
                            if received_for_file > entry.size {
                                return Err(XferError::protocol(format!(
                                    "{} exceeded its declared size",
                                    entry.path
                                )));
                            }
                            file.write_all(&payload)?;
                            hash.update(&payload);
                            transferred += payload.len() as u64;
                            if last_progress.elapsed() >= PROGRESS_INTERVAL {
                                reporter.progress(&Progress {
                                    phase: "Receiving",
                                    current_path: entry.path.clone(),
                                    transferred,
                                    total: offer.total_bytes,
                                    files_done,
                                    files_total: offer.file_count,
                                });
                                last_progress = Instant::now();
                            }
                        }
                        FrameKind::EntryEnd => {
                            let end: EntryEnd = serde_json::from_slice(&payload)
                                .map_err(|error| XferError::Serialization(error.to_string()))?;
                            if received_for_file != entry.size {
                                return Err(XferError::protocol(format!(
                                    "{} ended at {} bytes, expected {}",
                                    entry.path, received_for_file, entry.size
                                )));
                            }
                            let digest: [u8; 32] = hash.finalize().into();
                            if digest != end.sha256 {
                                return Err(XferError::security(format!(
                                    "SHA-256 mismatch for {}",
                                    entry.path
                                )));
                            }
                            file.flush()?;
                            file.sync_all()?;
                            update_manifest(&mut manifest, &entry.path, &digest);
                            files_done += 1;
                            reporter.progress(&Progress {
                                phase: "Receiving",
                                current_path: entry.path.clone(),
                                transferred,
                                total: offer.total_bytes,
                                files_done,
                                files_total: offer.file_count,
                            });
                            last_progress = Instant::now();
                            break;
                        }
                        other => {
                            return Err(XferError::protocol(format!(
                                "unexpected {other:?} while receiving {}",
                                entry.path
                            )));
                        }
                    }
                }
            }
        }
    }

    let end: TransferEnd = session.receive_message(FrameKind::TransferEnd)?;
    let manifest_sha256: [u8; 32] = manifest.finalize().into();
    if end.file_count != files_done
        || end.file_count != offer.file_count
        || end.total_bytes != transferred
        || end.total_bytes != offer.total_bytes
        || end.manifest_sha256 != manifest_sha256
    {
        return Err(XferError::security(
            "transfer totals or manifest digest did not verify",
        ));
    }

    if let Some(warning) = install_staged(&stage_path, &destination, options.overwrite)? {
        reporter.status(&warning);
    }
    session.send_message(
        FrameKind::Complete,
        &Complete {
            destination: destination.display().to_string(),
            file_count: files_done,
            total_bytes: transferred,
        },
    )?;
    reporter.status(&format!(
        "saved verified transfer to {}",
        destination.display()
    ));
    Ok(TransferSummary {
        destination,
        file_count: files_done,
        total_bytes: transferred,
        peer,
    })
}

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

fn validate_receive_options(options: &ReceiveOptions) -> Result<()> {
    validate_secure_token(options.secure, options.token.as_deref())
}

fn validate_secure_token(secure: bool, token: Option<&str>) -> Result<()> {
    if token.is_some_and(str::is_empty) {
        return Err(XferError::invalid_input("--token must not be empty"));
    }
    if token.is_some() && !secure {
        return Err(XferError::invalid_input(
            "--token can only be used with secure transfers",
        ));
    }
    Ok(())
}

fn validate_completion(complete: &Complete, files_done: u64, transferred: u64) -> Result<()> {
    if complete.file_count != files_done || complete.total_bytes != transferred {
        return Err(XferError::security(
            "receiver completion totals did not match the sent transfer",
        ));
    }
    Ok(())
}

fn establish_client(
    mut stream: TcpStream,
    secure: bool,
    token: Option<&str>,
    paths: &Paths,
    reporter: &dyn Reporter,
    deadline: Instant,
) -> Result<RecordStream<TcpStream>> {
    net::apply_deadline(&stream, deadline)?;
    client_negotiate(&mut stream, secure)?;
    if !secure {
        net::restore_io_timeouts(&stream)?;
        return Ok(RecordStream::new(stream, Role::Client, None, None));
    }

    let server_hello = read_server_hello(&mut stream)?;
    let client_secret = random_secret()?;
    let client_public = PublicKey::from(&client_secret);
    let mut client_nonce = [0_u8; 32];
    fill_random(&mut client_nonce)?;
    write_client_hello(
        &mut stream,
        &ClientHello {
            public_key: *client_public.as_bytes(),
            nonce: client_nonce,
        },
    )?;

    let server_public = PublicKey::from(server_hello.public_key);
    let keys = derive_session_keys(
        &client_secret,
        &server_public,
        &server_hello.public_key,
        client_public.as_bytes(),
        &server_hello.nonce,
        &client_nonce,
        token,
    )?;
    let fingerprint = fingerprint(&server_hello.public_key);
    let sas = sas(
        &server_hello.public_key,
        client_public.as_bytes(),
        &server_hello.nonce,
        &client_nonce,
        token,
    );
    let endpoint = stream.peer_addr()?.to_string();
    let mut session = RecordStream::new(
        stream,
        Role::Client,
        Some(keys.client_to_server),
        Some(keys.server_to_client),
    );
    session.send_message(FrameKind::Ready, &())?;
    session.receive_message::<()>(FrameKind::Ready)?;
    net::restore_io_timeouts(session.get_mut())?;
    let trust = TrustStore::load(paths)?;
    let changed = trust
        .get(&endpoint)
        .is_some_and(|peer| peer.fingerprint != fingerprint);
    let known = trust
        .get(&endpoint)
        .is_some_and(|peer| peer.fingerprint == fingerprint);
    if known {
        reporter.status("receiver identity matches the saved peer");
    } else {
        let prompt = TrustPrompt {
            endpoint: endpoint.clone(),
            fingerprint: display_fingerprint(&fingerprint),
            sas,
            changed,
        };
        if !reporter.confirm_peer(&prompt)? {
            return Err(XferError::security("peer was not trusted"));
        }
    }
    TrustStore::update(paths, |trust| {
        if trust
            .get(&endpoint)
            .is_some_and(|peer| peer.fingerprint != fingerprint)
        {
            return Err(XferError::security(
                "receiver identity changed while trust was being confirmed",
            ));
        }
        trust.remember(endpoint, fingerprint);
        Ok(())
    })?;

    Ok(session)
}

fn establish_server(
    mut stream: TcpStream,
    secure: bool,
    token: Option<&str>,
    paths: &Paths,
    reporter: &dyn Reporter,
) -> Result<RecordStream<TcpStream>> {
    server_negotiate(&mut stream, secure)?;
    if !secure {
        return Ok(RecordStream::new(stream, Role::Server, None, None));
    }

    let identity = Identity::load_or_create(paths)?;
    let server_public = identity.public();
    let mut server_nonce = [0_u8; 32];
    fill_random(&mut server_nonce)?;
    write_server_hello(
        &mut stream,
        &ServerHello {
            public_key: *server_public.as_bytes(),
            nonce: server_nonce,
        },
    )?;
    let client_hello = read_client_hello(&mut stream)?;
    let client_public = PublicKey::from(client_hello.public_key);
    let keys = derive_session_keys(
        identity.secret(),
        &client_public,
        server_public.as_bytes(),
        &client_hello.public_key,
        &server_nonce,
        &client_hello.nonce,
        token,
    )?;
    let fingerprint = fingerprint(server_public.as_bytes());
    let sas = sas(
        server_public.as_bytes(),
        &client_hello.public_key,
        &server_nonce,
        &client_hello.nonce,
        token,
    );
    reporter.show_sas(&sas, &display_fingerprint(&fingerprint));
    let mut session = RecordStream::new(
        stream,
        Role::Server,
        Some(keys.client_to_server),
        Some(keys.server_to_client),
    );
    session.receive_message::<()>(FrameKind::Ready)?;
    session.send_message(FrameKind::Ready, &())?;
    net::suspend_read_timeout(session.get_mut())?;
    Ok(session)
}

fn validate_offer(offer: &Offer) -> Result<()> {
    validate_wire_name(&offer.root_name)?;
    if offer.entry_count > 10_000_000 {
        return Err(XferError::protocol("entry count exceeds safety limit"));
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

fn random_secret() -> Result<StaticSecret> {
    let mut bytes = [0_u8; 32];
    fill_random(&mut bytes)?;
    let secret = StaticSecret::from(bytes);
    bytes.zeroize();
    Ok(secret)
}

fn fill_random(bytes: &mut [u8]) -> Result<()> {
    getrandom::fill(bytes)
        .map_err(|error| XferError::security(format!("system random source failed: {error}")))
}

fn update_manifest(manifest: &mut Sha256, path: &str, digest: &[u8; 32]) {
    manifest.update((path.len() as u64).to_be_bytes());
    manifest.update(path.as_bytes());
    manifest.update(digest);
}

fn read_retry(reader: &mut impl Read, buffer: &mut [u8]) -> Result<usize> {
    loop {
        match reader.read(buffer) {
            Ok(count) => return Ok(count),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
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

fn install_staged(stage: &Path, destination: &Path, overwrite: bool) -> Result<Option<String>> {
    if !overwrite {
        install_noclobber(stage, destination)?;
        return Ok(None);
    }
    if !destination.exists() {
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
        Err(error) if path_exists(destination) => return Err(error.into()),
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

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
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

fn format_plan(plan: &TransferPlan) -> String {
    let skipped = if plan.skipped_count == 0 {
        String::new()
    } else {
        format!(", {} skipped", plan.skipped_count)
    };
    format!(
        "prepared {}: {}, {} file(s){skipped}",
        plan.root_name,
        human_bytes(plan.total_bytes),
        plan.file_count
    )
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut unit = 0;
    let mut divisor = 1_u128;
    while u128::from(bytes) >= divisor * 1024 && unit < UNITS.len() - 1 {
        divisor *= 1024;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        let tenths = u128::from(bytes) * 10 / divisor;
        format!("{}.{:01} {}", tenths / 10, tenths % 10, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::{TcpListener, TcpStream},
        thread,
    };

    use tempfile::tempdir;

    use crate::reporter::{Progress, SilentReporter, TrustPrompt};

    use super::*;

    struct AcceptNewReporter;

    impl Reporter for AcceptNewReporter {
        fn status(&self, _message: &str) {}

        fn progress(&self, _progress: &Progress) {}

        fn show_sas(&self, _sas: &str, _fingerprint: &str) {}

        fn confirm_peer(&self, prompt: &TrustPrompt) -> Result<bool> {
            Ok(!prompt.changed)
        }
    }

    #[test]
    fn insecure_file_transfer_round_trips() {
        let source_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();
        let source = source_dir.path().join("hello.txt");
        fs::write(&source, b"hello from xfer").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let receiver_output = output_dir.path().to_path_buf();
        let receiver = thread::spawn(move || {
            receive_on_listener(
                &listener,
                &ReceiveOptions {
                    bind: "127.0.0.1".into(),
                    port,
                    output: receiver_output,
                    overwrite: false,
                    discoverable: false,
                    secure: false,
                    token: None,
                    config_dir: None,
                },
                &SilentReporter,
            )
            .unwrap()
        });

        send(
            &SendOptions {
                host: "127.0.0.1".into(),
                port,
                input: source,
                excludes: Vec::new(),
                follow_links: false,
                secure: false,
                token: None,
                connect_timeout: Duration::from_secs(2),
                config_dir: None,
            },
            &SilentReporter,
        )
        .unwrap();
        let summary = receiver.join().unwrap();
        assert_eq!(fs::read(summary.destination).unwrap(), b"hello from xfer");
    }

    #[test]
    fn secure_file_transfer_round_trips() {
        let source_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();
        let sender_config = tempdir().unwrap();
        let receiver_config = tempdir().unwrap();
        let source = source_dir.path().join("secure.txt");
        fs::write(&source, b"encrypted payload").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let receiver_output = output_dir.path().to_path_buf();
        let receiver_config = receiver_config.path().to_path_buf();
        let receiver = thread::spawn(move || {
            receive_on_listener(
                &listener,
                &ReceiveOptions {
                    bind: "127.0.0.1".into(),
                    port,
                    output: receiver_output,
                    overwrite: false,
                    discoverable: false,
                    secure: true,
                    token: Some("shared secret".into()),
                    config_dir: Some(receiver_config),
                },
                &SilentReporter,
            )
            .unwrap()
        });

        send(
            &SendOptions {
                host: "127.0.0.1".into(),
                port,
                input: source,
                excludes: Vec::new(),
                follow_links: false,
                secure: true,
                token: Some("shared secret".into()),
                connect_timeout: Duration::from_secs(2),
                config_dir: Some(sender_config.path().to_path_buf()),
            },
            &AcceptNewReporter,
        )
        .unwrap();
        let summary = receiver.join().unwrap();
        assert_eq!(fs::read(summary.destination).unwrap(), b"encrypted payload");
    }

    #[test]
    fn directory_transfer_preserves_tree_and_empty_directories() {
        let source_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();
        let source = source_dir.path().join("project");
        fs::create_dir_all(source.join("nested/empty")).unwrap();
        fs::write(source.join("README.md"), b"root").unwrap();
        fs::write(source.join("nested/data.bin"), [0_u8, 1, 2, 3]).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let receiver_output = output_dir.path().to_path_buf();
        let receiver = thread::spawn(move || {
            receive_on_listener(
                &listener,
                &ReceiveOptions {
                    bind: "127.0.0.1".into(),
                    port,
                    output: receiver_output,
                    overwrite: false,
                    discoverable: false,
                    secure: false,
                    token: None,
                    config_dir: None,
                },
                &SilentReporter,
            )
            .unwrap()
        });

        send(
            &SendOptions {
                host: "127.0.0.1".into(),
                port,
                input: source,
                excludes: Vec::new(),
                follow_links: false,
                secure: false,
                token: None,
                connect_timeout: Duration::from_secs(2),
                config_dir: None,
            },
            &SilentReporter,
        )
        .unwrap();
        let summary = receiver.join().unwrap();
        assert_eq!(
            fs::read(summary.destination.join("README.md")).unwrap(),
            b"root"
        );
        assert_eq!(
            fs::read(summary.destination.join("nested/data.bin")).unwrap(),
            [0_u8, 1, 2, 3]
        );
        assert!(summary.destination.join("nested/empty").is_dir());
    }

    #[test]
    fn secure_handshake_rejects_wrong_token_before_trust() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_dir = tempdir().unwrap();
        let client_dir = tempdir().unwrap();
        let server_paths = Paths::discover(Some(server_dir.path().to_path_buf())).unwrap();
        let client_paths = Paths::discover(Some(client_dir.path().to_path_buf())).unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            establish_server(stream, true, Some("server"), &server_paths, &SilentReporter)
        });
        let stream = TcpStream::connect(address).unwrap();
        let result = establish_client(
            stream,
            true,
            Some("client"),
            &client_paths,
            &SilentReporter,
            Instant::now() + Duration::from_secs(2),
        );
        assert!(result.is_err());
        assert!(server.join().unwrap().is_err());
        assert!(!client_paths.peers().exists());
    }

    #[test]
    fn known_peer_reconnect_updates_last_seen_and_suspends_server_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_dir = tempdir().unwrap();
        let client_dir = tempdir().unwrap();
        let server_paths = Paths::discover(Some(server_dir.path().to_path_buf())).unwrap();
        let client_paths = Paths::discover(Some(client_dir.path().to_path_buf())).unwrap();
        let server_identity = Identity::load_or_create(&server_paths).unwrap();
        let server_fingerprint = fingerprint(server_identity.public().as_bytes());
        client_paths.ensure().unwrap();
        fs::write(
            client_paths.peers(),
            serde_json::to_vec_pretty(&serde_json::json!({
                "peers": {
                    address.to_string(): {
                        "fingerprint": server_fingerprint,
                        "first_seen": 0,
                        "last_seen": 0,
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut session =
                establish_server(stream, true, None, &server_paths, &SilentReporter).unwrap();
            session.get_mut().read_timeout().unwrap()
        });
        let stream = TcpStream::connect(address).unwrap();
        establish_client(
            stream,
            true,
            None,
            &client_paths,
            &SilentReporter,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();

        assert_eq!(server.join().unwrap(), None);
        assert!(
            TrustStore::load(&client_paths)
                .unwrap()
                .get(&address.to_string())
                .unwrap()
                .last_seen
                > 0
        );
    }

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

    #[test]
    fn zero_byte_file_transfer_round_trips() {
        let source_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();
        let source = source_dir.path().join("empty.bin");
        fs::write(&source, []).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let receiver_output = output_dir.path().to_path_buf();
        let receiver = thread::spawn(move || {
            receive_on_listener(
                &listener,
                &ReceiveOptions {
                    bind: "127.0.0.1".into(),
                    port,
                    output: receiver_output,
                    overwrite: false,
                    discoverable: false,
                    secure: false,
                    token: None,
                    config_dir: None,
                },
                &SilentReporter,
            )
            .unwrap()
        });

        let summary = send(
            &SendOptions {
                host: "127.0.0.1".into(),
                port,
                input: source,
                excludes: Vec::new(),
                follow_links: false,
                secure: false,
                token: None,
                connect_timeout: Duration::from_secs(2),
                config_dir: None,
            },
            &SilentReporter,
        )
        .unwrap();
        assert_eq!(summary.total_bytes, 0);
        assert_eq!(
            fs::metadata(receiver.join().unwrap().destination)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn receive_collision_uses_numbered_destination() {
        let source_dir = tempdir().unwrap();
        let output_dir = tempdir().unwrap();
        let source = source_dir.path().join("payload.txt");
        fs::write(&source, b"new").unwrap();
        fs::write(output_dir.path().join("payload.txt"), b"old").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let receiver_output = output_dir.path().to_path_buf();
        let receiver = thread::spawn(move || {
            receive_on_listener(
                &listener,
                &ReceiveOptions {
                    bind: "127.0.0.1".into(),
                    port,
                    output: receiver_output,
                    overwrite: false,
                    discoverable: false,
                    secure: false,
                    token: None,
                    config_dir: None,
                },
                &SilentReporter,
            )
            .unwrap()
        });

        send(
            &SendOptions {
                host: "127.0.0.1".into(),
                port,
                input: source,
                excludes: Vec::new(),
                follow_links: false,
                secure: false,
                token: None,
                connect_timeout: Duration::from_secs(2),
                config_dir: None,
            },
            &SilentReporter,
        )
        .unwrap();
        let summary = receiver.join().unwrap();
        assert_eq!(
            fs::read(output_dir.path().join("payload.txt")).unwrap(),
            b"old"
        );
        assert_eq!(
            summary.destination,
            output_dir.path().join("payload (1).txt")
        );
        assert_eq!(fs::read(summary.destination).unwrap(), b"new");
    }

    #[test]
    fn changed_pinned_identity_is_rejected_without_store_update() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_dir = tempdir().unwrap();
        let client_dir = tempdir().unwrap();
        let server_paths = Paths::discover(Some(server_dir.path().to_path_buf())).unwrap();
        let client_paths = Paths::discover(Some(client_dir.path().to_path_buf())).unwrap();
        let mut trust = TrustStore::default();
        trust.remember(address.to_string(), "00".repeat(32));
        trust.save(&client_paths).unwrap();

        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            establish_server(stream, true, None, &server_paths, &SilentReporter)
        });
        let stream = TcpStream::connect(address).unwrap();
        assert!(
            establish_client(
                stream,
                true,
                None,
                &client_paths,
                &SilentReporter,
                Instant::now() + Duration::from_secs(2),
            )
            .is_err()
        );
        assert!(server.join().unwrap().is_ok());
        assert_eq!(
            TrustStore::load(&client_paths)
                .unwrap()
                .get(&address.to_string())
                .unwrap()
                .fingerprint,
            "00".repeat(32)
        );
    }

    #[test]
    fn offer_and_receive_option_safety_limits_are_enforced() {
        assert!(
            validate_offer(&Offer {
                root_name: "file.txt".into(),
                kind: TransferKind::File,
                total_bytes: 0,
                file_count: 0,
                entry_count: 1,
            })
            .is_err()
        );
        assert!(
            validate_offer(&Offer {
                root_name: "directory".into(),
                kind: TransferKind::Directory,
                total_bytes: 0,
                file_count: 2,
                entry_count: 1,
            })
            .is_err()
        );
        assert!(
            validate_offer(&Offer {
                root_name: "directory".into(),
                kind: TransferKind::Directory,
                total_bytes: 0,
                file_count: 0,
                entry_count: 10_000_001,
            })
            .is_err()
        );

        assert!(
            validate_receive_options(&ReceiveOptions {
                bind: "::".into(),
                port: 9_000,
                output: PathBuf::from("."),
                overwrite: false,
                discoverable: false,
                secure: false,
                token: Some("secret".into()),
                config_dir: None,
            })
            .is_err()
        );
        assert!(validate_secure_token(true, Some("")).is_err());
        assert!(validate_secure_token(true, Some("secret")).is_ok());
    }

    #[test]
    fn receiver_completion_must_match_sent_totals() {
        let complete = Complete {
            destination: "payload".into(),
            file_count: 2,
            total_bytes: 10,
        };
        assert!(validate_completion(&complete, 2, 10).is_ok());
        assert!(validate_completion(&complete, 1, 10).is_err());
        assert!(validate_completion(&complete, 2, 9).is_err());
    }

    #[test]
    fn connect_timeout_covers_protocol_negotiation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(300));
        });
        let client_dir = tempdir().unwrap();
        let client_paths = Paths::discover(Some(client_dir.path().to_path_buf())).unwrap();
        let stream = TcpStream::connect(address).unwrap();
        let started = Instant::now();
        let result = establish_client(
            stream,
            false,
            None,
            &client_paths,
            &SilentReporter,
            Instant::now() + Duration::from_millis(50),
        );

        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_millis(250));
        server.join().unwrap();
    }

    #[test]
    fn human_byte_formatting_handles_boundaries() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1_023), "1023 B");
        assert_eq!(human_bytes(1_024), "1.0 KiB");
        assert_eq!(human_bytes(1_536), "1.5 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
    }
}

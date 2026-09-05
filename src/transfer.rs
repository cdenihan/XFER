use std::{
    io::Read,
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::{
    config::{Identity, Paths, TrustStore},
    control::TransferControl,
    crypto::{derive_session_keys, display_fingerprint, fingerprint, sas, update_manifest},
    discovery::Advertiser,
    error::{Result, XferError},
    filesystem::{TransferPlan, build_plan_with_gitignore, open_planned_file, path_to_wire},
    net,
    protocol::{
        CHUNK_SIZE, ClientHello, Complete, Decision, EntryEnd, EntryKind, EntryStart, FrameKind,
        Offer, RecordStream, Role, ServerHello, TransferEnd, client_negotiate, read_client_hello,
        read_server_hello, sanitize_peer_text, server_negotiate, write_client_hello,
        write_server_hello,
    },
    receiver::Receiver,
    reporter::{Progress, Reporter, TrustPrompt},
    version::validate_peer_release_version,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ConflictPolicy {
    #[default]
    Preserve,
    PreferLocal,
    PreferRemote,
}

// These flags represent independent user-facing transfer policies.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug)]
pub struct SendOptions {
    pub conflict_policy: ConflictPolicy,
    pub sync: bool,
    pub preview: bool,
    pub two_way: bool,
    pub host: String,
    pub port: u16,
    pub input: PathBuf,
    pub excludes: Vec<String>,
    pub gitignore: bool,
    pub follow_links: bool,
    pub secure: bool,
    pub token: Option<String>,
    pub connect_timeout: Duration,
    pub config_dir: Option<PathBuf>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug)]
pub struct ReceiveOptions {
    pub allow_sync: bool,
    /// Sync directly into output instead of a child named after the source.
    pub sync_into: bool,
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
    pub sync_stats: Option<crate::delta::SyncStats>,
    pub preview: bool,
    pub conflicts: Vec<String>,
    pub destination: PathBuf,
    pub file_count: u64,
    pub total_bytes: u64,
    pub peer: SocketAddr,
    pub peer_version: Option<String>,
}

pub fn send(options: &SendOptions, reporter: &dyn Reporter) -> Result<TransferSummary> {
    send_controlled(options, reporter, &TransferControl::default())
}

pub fn send_controlled(
    options: &SendOptions,
    reporter: &dyn Reporter,
    control: &TransferControl,
) -> Result<TransferSummary> {
    control.finish(send_inner(options, reporter, control))
}

fn send_inner(
    options: &SendOptions,
    reporter: &dyn Reporter,
    control: &TransferControl,
) -> Result<TransferSummary> {
    control.check()?;
    validate_secure_token(options.secure, options.token.as_deref())?;
    let plan = build_plan_with_gitignore(
        &options.input,
        &options.excludes,
        options.follow_links,
        options.gitignore,
    )?;
    if (options.sync || options.two_way) && plan.kind != crate::protocol::TransferKind::Directory {
        return Err(XferError::invalid_input("sync requires a directory"));
    }
    if options.two_way && options.follow_links {
        return Err(XferError::invalid_input(
            "two-way sync does not follow symlinks",
        ));
    }
    reporter.status(&format_plan(&plan));
    let (stream, deadline) =
        net::connect_with_deadline(&options.host, options.port, options.connect_timeout)?;
    control.attach(&stream)?;
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

    if options.two_way {
        return crate::reconcile::send(&mut session, &plan, options, reporter, control);
    }
    if options.sync {
        return crate::sync::send(&mut session, &plan, options, reporter, control, None);
    }

    let offer = Offer {
        root_name: plan.root_name.clone(),
        kind: plan.kind,
        total_bytes: plan.total_bytes,
        file_count: plan.file_count,
        entry_count: plan.entries.len() as u64,
        release_version: Some(crate::VERSION.into()),
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
        control.check()?;
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
        let mut sent_for_file = 0_u64;
        loop {
            let count = read_retry(&mut file, &mut buffer)?;
            if count == 0 {
                break;
            }
            if count as u64 > entry.size - sent_for_file {
                return Err(XferError::invalid_input(format!(
                    "{} grew during transfer",
                    entry.source.display()
                )));
            }
            sent_for_file += count as u64;
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
        if sent_for_file != entry.size {
            return Err(XferError::invalid_input(format!(
                "{} shrank during transfer",
                entry.source.display()
            )));
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
        sync_stats: None,
        preview: false,
        conflicts: Vec::new(),
        destination: PathBuf::from(complete.destination),
        file_count: complete.file_count,
        total_bytes: complete.total_bytes,
        peer,
        peer_version: complete.release_version,
    })
}

pub fn receive(options: &ReceiveOptions, reporter: &dyn Reporter) -> Result<TransferSummary> {
    receive_controlled(options, reporter, &TransferControl::default())
}

pub fn receive_controlled(
    options: &ReceiveOptions,
    reporter: &dyn Reporter,
    control: &TransferControl,
) -> Result<TransferSummary> {
    control.finish(receive_inner(options, reporter, control))
}

fn receive_inner(
    options: &ReceiveOptions,
    reporter: &dyn Reporter,
    control: &TransferControl,
) -> Result<TransferSummary> {
    control.check()?;
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
    let advertiser = if options.discoverable {
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
    receive_on_listener_inner(&listener, options, reporter, control, advertiser)
}

pub fn receive_on_listener(
    listener: &TcpListener,
    options: &ReceiveOptions,
    reporter: &dyn Reporter,
) -> Result<TransferSummary> {
    let control = TransferControl::default();
    control.finish(receive_on_listener_inner(
        listener, options, reporter, &control, None,
    ))
}

fn receive_on_listener_inner(
    listener: &TcpListener,
    options: &ReceiveOptions,
    reporter: &dyn Reporter,
    control: &TransferControl,
    advertiser: Option<Advertiser>,
) -> Result<TransferSummary> {
    validate_receive_options(options)?;
    let local = listener.local_addr()?;
    reporter.status(&format!("listening on {local}"));
    let (stream, peer) = control.accept(listener)?;
    drop(advertiser);
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

    match receive_transfer(&mut session, options, reporter, peer, control) {
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
    control: &TransferControl,
) -> Result<TransferSummary> {
    let (kind, payload) = session.receive_frame()?;
    net::restore_read_timeout(session.get_mut())?;
    let offer: Offer = serde_json::from_slice(&payload)
        .map_err(|error| XferError::Serialization(error.to_string()))?;
    if matches!(kind, FrameKind::SyncOffer | FrameKind::PreviewOffer) {
        return crate::sync::receive(
            session,
            offer,
            options,
            kind == FrameKind::PreviewOffer,
            reporter,
            control,
            None,
        );
    }
    if matches!(kind, FrameKind::TwoWayOffer | FrameKind::TwoWayPreview) {
        return crate::reconcile::receive(
            session,
            offer,
            options,
            kind == FrameKind::TwoWayPreview,
            reporter,
            control,
        );
    }
    if kind != FrameKind::Offer {
        return Err(XferError::protocol("expected a transfer offer"));
    }
    let mut receiver = Receiver::new(offer, &options.output, options.overwrite)?;
    session.send_message(FrameKind::Decision, &Decision::Accept)?;
    let mut payload = Vec::with_capacity(CHUNK_SIZE + 16);
    let mut last_progress = Instant::now();
    loop {
        control.check()?;
        let kind = session.receive_frame_into(&mut payload)?;
        let verified = receiver.accept(kind, &payload)?;
        if verified || kind == FrameKind::EntryEnd || last_progress.elapsed() >= PROGRESS_INTERVAL {
            reporter.progress(&receiver.progress());
            last_progress = Instant::now();
        }
        if verified {
            break;
        }
    }
    control.check()?;
    let (installed, file_count, total_bytes, peer_version) = receiver.into_verified()?.commit()?;
    if let Some(warning) = installed.warning {
        reporter.status(&warning);
    }
    reporter.status(&format!(
        "saved verified transfer to {}",
        installed.destination.display()
    ));
    // Publication has already succeeded. A lost acknowledgement cannot undo it.
    session
        .send_message(
            FrameKind::Complete,
            &Complete {
                sync_stats: None,
                preview: false,
                destination: installed.destination.display().to_string(),
                file_count,
                total_bytes,
                release_version: Some(crate::VERSION.into()),
            },
        )
        .map_err(|error| {
            XferError::protocol(format!(
                "transfer was saved to {}, but completion could not be acknowledged: {error}",
                installed.destination.display()
            ))
        })?;
    Ok(TransferSummary {
        sync_stats: None,
        preview: false,
        conflicts: Vec::new(),
        destination: installed.destination,
        file_count,
        total_bytes,
        peer,
        peer_version,
    })
}

const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

fn validate_receive_options(options: &ReceiveOptions) -> Result<()> {
    validate_secure_token(options.secure, options.token.as_deref())
}

pub(crate) fn validate_secure_token(secure: bool, token: Option<&str>) -> Result<()> {
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
    validate_peer_release_version(complete.release_version.as_deref())?;
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
    let previous_fingerprint = trust.get(&endpoint).map(|peer| peer.fingerprint.clone());
    TrustStore::update(paths, |trust| {
        let current = trust.get(&endpoint).map(|peer| peer.fingerprint.as_str());
        if current != previous_fingerprint.as_deref() && current != Some(fingerprint.as_str()) {
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

fn read_retry(reader: &mut impl Read, buffer: &mut [u8]) -> Result<usize> {
    loop {
        match reader.read(buffer) {
            Ok(count) => return Ok(count),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
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
mod tests;

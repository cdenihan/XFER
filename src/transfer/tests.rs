use std::{
    fs,
    net::{TcpListener, TcpStream},
    thread,
};

use tempfile::tempdir;

use crate::reporter::{Progress, SilentReporter, TrustPrompt};

use super::*;
use crate::{protocol::TransferKind, receiver::validate_offer};

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
                allow_sync: false,
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
            conflict_policy: crate::transfer::ConflictPolicy::Preserve,
            sync: false,
            preview: false,
            two_way: false,
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
    let payload = [0_u8, 1, 127, 255].repeat(CHUNK_SIZE / 2 + 137);
    fs::write(&source, &payload).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let receiver_output = output_dir.path().to_path_buf();
    let receiver_config = receiver_config.path().to_path_buf();
    let receiver = thread::spawn(move || {
        receive_on_listener(
            &listener,
            &ReceiveOptions {
                allow_sync: false,
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
            conflict_policy: crate::transfer::ConflictPolicy::Preserve,
            sync: false,
            preview: false,
            two_way: false,
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
    assert_eq!(fs::read(summary.destination).unwrap(), payload);
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
                allow_sync: false,
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
            conflict_policy: crate::transfer::ConflictPolicy::Preserve,
            sync: false,
            preview: false,
            two_way: false,
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
                allow_sync: false,
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
            conflict_policy: crate::transfer::ConflictPolicy::Preserve,
            sync: false,
            preview: false,
            two_way: false,
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
                allow_sync: false,
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
            conflict_policy: crate::transfer::ConflictPolicy::Preserve,
            sync: false,
            preview: false,
            two_way: false,
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
            release_version: None,
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
            release_version: None,
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
            release_version: None,
        })
        .is_err()
    );

    assert!(
        validate_receive_options(&ReceiveOptions {
            allow_sync: false,
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
    assert!(validate_peer_release_version(Some("2026.07.16.2")).is_ok());
    assert!(validate_peer_release_version(Some("bad\u{1b}[2J")).is_err());
}

#[test]
fn receiver_completion_must_match_sent_totals() {
    let complete = Complete {
        sync_stats: None,
        preview: false,
        destination: "payload".into(),
        file_count: 2,
        total_bytes: 10,
        release_version: None,
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
struct ConfirmChangedReporter {
    paths: Paths,
    replace_during_prompt: bool,
}

impl Reporter for ConfirmChangedReporter {
    fn status(&self, _: &str) {}
    fn progress(&self, _: &Progress) {}
    fn show_sas(&self, _: &str, _: &str) {}
    fn confirm_peer(&self, prompt: &TrustPrompt) -> Result<bool> {
        assert!(prompt.changed);
        if self.replace_during_prompt {
            TrustStore::update(&self.paths, |store| {
                store.remember(prompt.endpoint.clone(), "concurrent-identity".into());
                Ok(())
            })?;
        }
        Ok(true)
    }
}

#[test]
fn changed_identity_can_be_confirmed_but_cannot_overwrite_a_concurrent_change() {
    for replace_during_prompt in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_dir = tempdir().unwrap();
        let client_dir = tempdir().unwrap();
        let server_paths = Paths::discover(Some(server_dir.path().into())).unwrap();
        let client_paths = Paths::discover(Some(client_dir.path().into())).unwrap();
        let expected = fingerprint(
            Identity::load_or_create(&server_paths)
                .unwrap()
                .public()
                .as_bytes(),
        );
        TrustStore::update(&client_paths, |store| {
            store.remember(address.to_string(), "old-identity".into());
            Ok(())
        })
        .unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            net::configure_stream(&stream).unwrap();
            establish_server(stream, true, None, &server_paths, &SilentReporter)
        });
        let reporter = ConfirmChangedReporter {
            paths: client_paths.clone(),
            replace_during_prompt,
        };
        let result = establish_client(
            TcpStream::connect(address).unwrap(),
            true,
            None,
            &client_paths,
            &reporter,
            Instant::now() + Duration::from_secs(2),
        );
        assert_eq!(result.is_ok(), !replace_during_prompt);
        assert!(server.join().unwrap().is_ok());
        let stored = TrustStore::load(&client_paths).unwrap();
        assert_eq!(
            stored.get(&address.to_string()).unwrap().fingerprint,
            if replace_during_prompt {
                "concurrent-identity".into()
            } else {
                expected
            }
        );
    }
}

fn rejected_transfer(script: impl FnOnce(&mut RecordStream<TcpStream>)) -> String {
    let output = tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let options = ReceiveOptions {
        allow_sync: false,
        bind: "127.0.0.1".into(),
        port: 0,
        output: output.path().into(),
        overwrite: true,
        discoverable: false,
        secure: false,
        token: None,
        config_dir: None,
    };
    fs::write(output.path().join("payload"), b"original").unwrap();
    let receiver = thread::spawn(move || {
        let (stream, peer) = listener.accept().unwrap();
        net::configure_stream(&stream).unwrap();
        let mut session = RecordStream::new(stream, Role::Server, None, None);
        receive_transfer(
            &mut session,
            &options,
            &SilentReporter,
            peer,
            &TransferControl::default(),
        )
        .unwrap_err()
        .to_string()
    });
    let stream = TcpStream::connect(address).unwrap();
    net::configure_stream(&stream).unwrap();
    let mut session = RecordStream::new(stream, Role::Client, None, None);
    script(&mut session);
    drop(session);
    let error = receiver.join().unwrap();
    assert_eq!(
        fs::read(output.path().join("payload")).unwrap(),
        b"original"
    );
    assert_eq!(
        fs::read_dir(output.path()).unwrap().count(),
        1,
        "staging must be cleaned"
    );
    error
}

fn offer_directory(
    session: &mut RecordStream<TcpStream>,
    total_bytes: u64,
    file_count: u64,
    entry_count: u64,
) {
    session
        .send_message(
            FrameKind::Offer,
            &Offer {
                root_name: "payload".into(),
                kind: TransferKind::Directory,
                total_bytes,
                file_count,
                entry_count,
                release_version: None,
            },
        )
        .unwrap();
    assert!(matches!(
        session
            .receive_message::<Decision>(FrameKind::Decision)
            .unwrap(),
        Decision::Accept
    ));
}

#[test]
fn oversized_entry_is_rejected_before_reading_its_data() {
    let error = rejected_transfer(|session| {
        offer_directory(session, 1, 1, 1);
        session
            .send_message(
                FrameKind::EntryStart,
                &EntryStart {
                    path: "file".into(),
                    kind: EntryKind::File,
                    size: u64::MAX,
                },
            )
            .unwrap();
    });
    assert!(error.contains("offered transfer totals"), "{error}");
}

#[test]
fn excess_file_count_is_rejected_before_creating_file() {
    let error = rejected_transfer(|session| {
        offer_directory(session, 0, 0, 1);
        session
            .send_message(
                FrameKind::EntryStart,
                &EntryStart {
                    path: "file".into(),
                    kind: EntryKind::File,
                    size: 0,
                },
            )
            .unwrap();
    });
    assert!(error.contains("offered transfer totals"), "{error}");
}

#[test]
fn colliding_implicit_parent_names_are_rejected() {
    let error = rejected_transfer(|session| {
        offer_directory(session, 0, 0, 2);
        for path in ["Foo/a", "foo/b"] {
            session
                .send_message(
                    FrameKind::EntryStart,
                    &EntryStart {
                        path: path.into(),
                        kind: EntryKind::Directory,
                        size: 0,
                    },
                )
                .unwrap();
        }
    });
    assert!(error.contains("colliding directory names"), "{error}");
}

#[test]
fn interrupted_and_corrupt_transfers_preserve_destination_and_remove_staging() {
    for corrupt in [false, true] {
        let error = rejected_transfer(|session| {
            offer_directory(session, 4, 1, 1);
            session
                .send_message(
                    FrameKind::EntryStart,
                    &EntryStart {
                        path: "file".into(),
                        kind: EntryKind::File,
                        size: 4,
                    },
                )
                .unwrap();
            session.send_frame(FrameKind::Data, b"data").unwrap();
            if corrupt {
                session
                    .send_message(FrameKind::EntryEnd, &EntryEnd { sha256: [0; 32] })
                    .unwrap();
            }
        });
        if corrupt {
            assert!(error.contains("SHA-256 mismatch"), "{error}");
        }
    }
}

struct SyncPair {
    listener: TcpListener,
    source: tempfile::TempDir,
    output: tempfile::TempDir,
    client_config: tempfile::TempDir,
    server_config: tempfile::TempDir,
}
impl SyncPair {
    fn new() -> Self {
        Self {
            listener: TcpListener::bind("127.0.0.1:0").unwrap(),
            source: tempdir().unwrap(),
            output: tempdir().unwrap(),
            client_config: tempdir().unwrap(),
            server_config: tempdir().unwrap(),
        }
    }
    fn local(&self) -> PathBuf {
        self.source.path().join("data")
    }
    fn remote(&self) -> PathBuf {
        self.output.path().join("data")
    }
    fn run(&self, two_way: bool, preview: bool) -> Result<TransferSummary> {
        self.run_policy(two_way, preview, ConflictPolicy::Preserve)
    }
    fn run_policy(
        &self,
        two_way: bool,
        preview: bool,
        policy: ConflictPolicy,
    ) -> Result<TransferSummary> {
        let listener = self.listener.try_clone().unwrap();
        let options = ReceiveOptions {
            allow_sync: true,
            bind: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            output: self.output.path().into(),
            overwrite: false,
            discoverable: false,
            secure: true,
            token: Some("sync secret".into()),
            config_dir: Some(self.server_config.path().into()),
        };
        let receiver =
            thread::spawn(move || receive_on_listener(&listener, &options, &SilentReporter));
        let result = send(
            &SendOptions {
                conflict_policy: policy,
                sync: true,
                preview,
                two_way,
                host: "127.0.0.1".into(),
                port: self.listener.local_addr().unwrap().port(),
                input: self.local(),
                excludes: vec!["ignored".into()],
                follow_links: false,
                secure: true,
                token: Some("sync secret".into()),
                connect_timeout: Duration::from_secs(10),
                config_dir: Some(self.client_config.path().into()),
            },
            &AcceptNewReporter,
        );
        let server_result = receiver.join().unwrap();
        if result.is_ok() {
            assert!(server_result.is_ok(), "receiver failed: {server_result:?}");
        }
        result
    }
}

#[test]
fn incremental_sync_reuses_unchanged_files_and_shifted_blocks() {
    let pair = SyncPair::new();
    fs::create_dir_all(pair.local().join("empty")).unwrap();
    let basis = (0..crate::delta::MIN_BLOCK * 5)
        .map(|n| u8::try_from((n * 31 + n / 113) % 251).unwrap())
        .collect::<Vec<_>>();
    fs::write(pair.local().join("large.bin"), &basis).unwrap();
    fs::write(pair.local().join("small"), b"hello").unwrap();
    let first = pair.run(false, false).unwrap().sync_stats.unwrap();
    assert_eq!(first.sent_bytes, basis.len() as u64 + 5);
    assert_eq!(first.changed_files, 2);
    let modified = fs::metadata(pair.remote().join("large.bin"))
        .unwrap()
        .modified()
        .unwrap();
    fs::write(pair.remote().join("remote-only"), b"keep").unwrap();
    let second = pair.run(false, false).unwrap().sync_stats.unwrap();
    assert_eq!(second.sent_bytes, 0);
    assert_eq!(second.unchanged_files, 2);
    assert_eq!(
        fs::metadata(pair.remote().join("large.bin"))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    let mut updated = b"prefix".to_vec();
    updated.extend_from_slice(&basis);
    fs::write(pair.local().join("large.bin"), &updated).unwrap();
    let third = pair.run(false, false).unwrap().sync_stats.unwrap();
    assert_eq!(third.sent_bytes, 6);
    assert_eq!(third.reused_bytes, basis.len() as u64 + 5);
    assert_eq!(third.changed_files, 1);
    assert_eq!(fs::read(pair.remote().join("large.bin")).unwrap(), updated);
    assert_eq!(
        fs::read(pair.remote().join("remote-only")).unwrap(),
        b"keep"
    );
    assert!(pair.remote().join("empty").is_dir());
}

#[test]
fn sync_preview_does_not_create_or_replace_destination_data() {
    let pair = SyncPair::new();
    fs::create_dir_all(pair.local()).unwrap();
    fs::write(pair.local().join("file"), b"new").unwrap();
    let preview = pair.run(false, true).unwrap();
    assert!(preview.preview);
    assert_eq!(preview.sync_stats.unwrap().sent_bytes, 3);
    assert!(!pair.remote().exists());
    pair.run(false, false).unwrap();
    fs::write(pair.local().join("file"), b"changed").unwrap();
    let preview = pair.run(false, true).unwrap();
    assert_eq!(preview.sync_stats.unwrap().sent_bytes, 7);
    assert_eq!(fs::read(pair.remote().join("file")).unwrap(), b"new");
}

#[test]
fn two_way_sync_propagates_changes_and_preserves_both_conflicting_versions() {
    let pair = SyncPair::new();
    fs::create_dir_all(pair.local()).unwrap();
    fs::create_dir_all(pair.remote()).unwrap();
    for root in [pair.local(), pair.remote()] {
        fs::write(root.join("shared"), b"same").unwrap();
    }
    fs::write(pair.local().join("from-local"), b"local").unwrap();
    fs::write(pair.remote().join("from-remote"), b"remote").unwrap();
    fs::write(pair.local().join("conflict"), b"left").unwrap();
    fs::write(pair.remote().join("conflict"), b"right").unwrap();
    fs::write(pair.remote().join("ignored"), b"ignore").unwrap();
    let first = pair.run(true, false).unwrap();
    assert_eq!(first.conflicts, vec!["conflict"]);
    assert_eq!(
        fs::read(pair.local().join("from-remote")).unwrap(),
        b"remote"
    );
    assert_eq!(
        fs::read(pair.remote().join("from-local")).unwrap(),
        b"local"
    );
    assert!(!pair.local().join("ignored").exists());
    fs::write(pair.local().join("shared"), b"local edit").unwrap();
    fs::write(pair.remote().join("from-remote"), b"remote edit").unwrap();
    pair.run(true, false).unwrap();
    assert_eq!(
        fs::read(pair.remote().join("shared")).unwrap(),
        b"local edit"
    );
    assert_eq!(
        fs::read(pair.local().join("from-remote")).unwrap(),
        b"remote edit"
    );
    fs::write(pair.local().join("shared"), b"left edit").unwrap();
    fs::write(pair.remote().join("shared"), b"right edit").unwrap();
    let third = pair.run(true, false).unwrap();
    assert!(third.conflicts.contains(&"shared".into()));
    assert_eq!(fs::read(pair.local().join("shared")).unwrap(), b"left edit");
    assert_eq!(
        fs::read(pair.remote().join("shared")).unwrap(),
        b"right edit"
    );
    assert_eq!(fs::read(pair.local().join("conflict")).unwrap(), b"left");
    assert_eq!(fs::read(pair.remote().join("conflict")).unwrap(), b"right");
}

#[test]
fn two_way_preview_keeps_both_folders_and_baseline_unchanged() {
    let pair = SyncPair::new();
    fs::create_dir_all(pair.local()).unwrap();
    fs::create_dir_all(pair.remote()).unwrap();
    fs::write(pair.local().join("local"), b"local").unwrap();
    fs::write(pair.remote().join("remote"), b"remote").unwrap();
    let result = pair.run(true, true).unwrap();
    assert!(result.preview);
    assert_eq!(result.sync_stats.unwrap().changed_files, 2);
    assert!(!pair.local().join("remote").exists());
    assert!(!pair.remote().join("local").exists());
    assert!(
        !fs::read_dir(pair.client_config.path())
            .unwrap()
            .any(|entry| {
                let path = entry.unwrap().path();
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("sync-")
                    && path.extension().is_some_and(|ext| ext == "json")
            })
    );
}

#[test]
fn explicit_conflict_resolution_previews_then_applies_the_selected_side() {
    let pair = SyncPair::new();
    fs::create_dir_all(pair.local()).unwrap();
    fs::create_dir_all(pair.remote()).unwrap();
    fs::write(pair.local().join("file"), b"local").unwrap();
    fs::write(pair.remote().join("file"), b"remote").unwrap();
    let preview = pair
        .run_policy(true, true, ConflictPolicy::PreferRemote)
        .unwrap();
    assert!(preview.conflicts.is_empty());
    assert_eq!(preview.sync_stats.unwrap().changed_files, 1);
    assert_eq!(fs::read(pair.local().join("file")).unwrap(), b"local");
    pair.run_policy(true, false, ConflictPolicy::PreferRemote)
        .unwrap();
    assert_eq!(fs::read(pair.local().join("file")).unwrap(), b"remote");
    fs::write(pair.local().join("file"), b"left edit").unwrap();
    fs::write(pair.remote().join("file"), b"right edit").unwrap();
    pair.run_policy(true, false, ConflictPolicy::PreferLocal)
        .unwrap();
    assert_eq!(fs::read(pair.remote().join("file")).unwrap(), b"left edit");
}

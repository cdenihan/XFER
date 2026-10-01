#![cfg(feature = "cli")]

use std::{
    fs,
    net::{TcpListener, TcpStream},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use xfer::{
    error::XferError,
    transfer::ReceiveOptions,
    workflow::{Action, Job, Recent, WorkerEvent, send_options},
};

#[track_caller]
fn wait_for(job: &Job, mut predicate: impl FnMut(WorkerEvent) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(envelope) = job.try_recv() {
            assert_eq!(envelope.operation, job.operation);
            let terminal = if let WorkerEvent::Finished(result) = &envelope.event {
                Some(format!("{result:?}"))
            } else {
                None
            };
            if predicate(envelope.event) {
                return;
            }
            assert!(
                terminal.is_none(),
                "unexpected worker completion: {terminal:?}"
            );
        }
        assert!(Instant::now() < deadline, "worker event timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

fn listening(job: &Job) {
    wait_for(job, |event| match event {
        WorkerEvent::Status(message) => message.starts_with("listening on"),
        WorkerEvent::Finished(result) => panic!("receiver exited before listening: {result:?}"),
        _ => false,
    });
}

#[test]
fn desktop_worker_receives_cli_sessions_cancels_blocked_io_and_retries() {
    let directory = tempfile::tempdir().unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = Some(directory.path().join("desktop-config"));
    let recent = Recent {
        action: Action::Receive,
        path: directory.path().join("received"),
        host: "127.0.0.1".into(),
        port,
        gitignore: false,
    };
    let receiver = ReceiveOptions {
        allow_sync: true,
        sync_into: false,
        bind: "127.0.0.1".into(),
        port,
        output: recent.path.clone(),
        overwrite: true,
        discoverable: false,
        secure: false,
        token: None,
        config_dir: config.clone(),
    };
    let start = |operation| {
        Job::start(
            operation,
            recent.clone(),
            send_options(&recent, config.clone()),
            receiver.clone(),
        )
        .unwrap()
    };
    let job = start(1);
    listening(&job);
    // A disconnected or malformed unauthenticated client cannot take the receiver offline.
    drop(TcpStream::connect(("127.0.0.1", port)).unwrap());
    wait_for(
        &job,
        |event| matches!(event, WorkerEvent::Status(text) if text.starts_with("Session ended:")),
    );
    for name in ["résumé.txt", "second.txt"] {
        listening(&job);
        let source = directory.path().join(name);
        fs::write(&source, format!("payload for {name}")).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_xfer"))
            .arg("--config-dir")
            .arg(directory.path().join("cli-config"))
            .args(["send", "127.0.0.1"])
            .arg(&source)
            .args(["--port", &port.to_string(), "--insecure"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        wait_for(&job, |event| matches!(event, WorkerEvent::Received(_)));
        assert_eq!(
            fs::read(&source).unwrap(),
            fs::read(recent.path.join(name)).unwrap()
        );
    }
    listening(&job);
    // A peer connects but never sends its handshake. Cancellation must unblock reads.
    let stalled = TcpStream::connect(("127.0.0.1", port)).unwrap();
    thread::sleep(Duration::from_millis(100));
    job.control.cancel();
    wait_for(&job, |event| {
        matches!(event, WorkerEvent::Finished(Err(XferError::Cancelled)))
    });
    drop(stalled);
    drop(job);
    let retry = start(2);
    listening(&retry);
    retry.control.cancel();
    wait_for(&retry, |event| {
        matches!(event, WorkerEvent::Finished(Err(XferError::Cancelled)))
    });
}

struct ReceiverProcess(std::process::Child);
impl Drop for ReceiverProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn desktop_sender_compares_codes_rejects_changed_identity_and_can_retry() {
    use std::process::Stdio;
    let directory = tempfile::tempdir().unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let source = directory.path().join("payload.txt");
    fs::write(&source, b"encrypted desktop payload").unwrap();
    let recent = Recent {
        action: Action::Copy,
        path: source,
        host: "127.0.0.1".into(),
        port,
        gitignore: false,
    };
    let config = Some(directory.path().join("desktop-config"));
    for (operation, receiver_config, approve, changed) in [
        (1, "first", true, false),
        (2, "replacement", false, true),
        (3, "replacement", true, true),
    ] {
        let log = directory.path().join(format!("receiver-{operation}.log"));
        let output_dir = directory.path().join(format!("received-{operation}"));
        let mut process = ReceiverProcess(
            Command::new(env!("CARGO_BIN_EXE_xfer"))
                .arg("--config-dir")
                .arg(directory.path().join(receiver_config))
                .args([
                    "receive",
                    "--bind",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                    "--output",
                ])
                .arg(&output_dir)
                .arg("--no-discovery")
                .stdout(Stdio::null())
                .stderr(fs::File::create(&log).unwrap())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fs::read_to_string(&log).unwrap().contains("listening on") {
            assert!(Instant::now() < deadline, "CLI receiver did not start");
            thread::sleep(Duration::from_millis(10));
        }
        let receiver = ReceiveOptions {
            allow_sync: false,
            sync_into: false,
            bind: "127.0.0.1".into(),
            port,
            output: output_dir.clone(),
            overwrite: false,
            discoverable: false,
            secure: true,
            token: None,
            config_dir: config.clone(),
        };
        let job = Job::start(
            operation,
            recent.clone(),
            send_options(&recent, config.clone()),
            receiver,
        )
        .unwrap();
        let mut prompted = false;
        wait_for(&job, |event| match event {
            WorkerEvent::Trust(prompt, reply) => {
                assert_eq!(prompt.changed, changed);
                assert!(
                    fs::read_to_string(&log).unwrap().contains(&prompt.sas),
                    "both endpoints must display the same security code"
                );
                reply.send(approve).unwrap();
                prompted = true;
                false
            }
            WorkerEvent::Finished(result) => {
                assert_eq!(result.is_ok(), approve, "{result:?}");
                true
            }
            _ => false,
        });
        assert!(prompted);
        // Bounded child wait so a protocol regression cannot hang this test suite.
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = process.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "CLI receiver did not exit");
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(status.success(), approve);
        if approve {
            assert_eq!(
                fs::read(output_dir.join("payload.txt")).unwrap(),
                b"encrypted desktop payload"
            );
        } else {
            assert!(!output_dir.join("payload.txt").exists());
        }
    }
}

#[test]
fn reviewed_sync_rejects_changed_files_before_overwriting() {
    use xfer::transfer::ConflictPolicy;
    for action in [Action::Sync, Action::TwoWay] {
        for mutation in ["source", "new-source", "destination", "unchanged"] {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("source");
            let target = directory.path().join("target");
            fs::create_dir_all(&source).unwrap();
            fs::create_dir_all(&target).unwrap();
            fs::write(source.join("file.txt"), b"previewed source").unwrap();
            fs::write(target.join("file.txt"), b"previewed destination").unwrap();
            let port = TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let receive_recent = Recent {
                action: Action::Receive,
                path: target.clone(),
                host: "127.0.0.1".into(),
                port,
                gitignore: false,
            };
            let receive_options = ReceiveOptions {
                allow_sync: true,
                sync_into: true,
                bind: "127.0.0.1".into(),
                port,
                output: target.clone(),
                overwrite: false,
                discoverable: false,
                secure: false,
                token: None,
                config_dir: Some(directory.path().join("receiver-config")),
            };
            let receiver = Job::start(
                1,
                receive_recent.clone(),
                send_options(&receive_recent, receive_options.config_dir.clone()),
                receive_options.clone(),
            )
            .unwrap();
            listening(&receiver);
            let recent = Recent {
                action,
                path: source.clone(),
                host: "127.0.0.1".into(),
                port,
                gitignore: false,
            };
            let mut options = send_options(&recent, Some(directory.path().join("sender-config")));
            options.secure = false;
            options.conflict_policy = ConflictPolicy::PreferLocal;
            let preview =
                Job::start(2, recent.clone(), options.clone(), receive_options.clone()).unwrap();
            wait_for(
                &preview,
                |event| matches!(event, WorkerEvent::Finished(Ok(summary)) if summary.preview),
            );
            let snapshot = preview.control.review_snapshot().unwrap();
            drop(preview);
            match mutation {
                "source" => fs::write(source.join("file.txt"), b"unreviewed source").unwrap(),
                "new-source" => fs::write(source.join("new.txt"), b"unreviewed new file").unwrap(),
                "destination" => {
                    fs::write(target.join("file.txt"), b"intervening destination edit").unwrap();
                }
                _ => {}
            }
            let expected = fs::read(target.join("file.txt")).unwrap();
            options.preview = false;
            let apply =
                Job::start_reviewed(3, recent, options, receive_options, Some(snapshot)).unwrap();
            if mutation == "unchanged" {
                wait_for(
                    &apply,
                    |event| matches!(event, WorkerEvent::Finished(Ok(summary)) if !summary.preview),
                );
                assert_eq!(
                    fs::read(source.join("file.txt")).unwrap(),
                    fs::read(target.join("file.txt")).unwrap()
                );
            } else {
                wait_for(
                    &apply,
                    |event| matches!(event, WorkerEvent::Finished(Err(XferError::Rejected(message))) if message.contains("Folders changed")),
                );
                assert_eq!(
                    fs::read(target.join("file.txt")).unwrap(),
                    expected,
                    "{action:?}: {mutation}"
                );
                assert!(!target.join("new.txt").exists());
            }
            receiver.control.cancel();
            wait_for(&receiver, |event| {
                matches!(event, WorkerEvent::Finished(Err(XferError::Cancelled)))
            });
        }
    }
}

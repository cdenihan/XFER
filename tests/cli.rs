use std::{
    fs::{self, File},
    net::TcpListener,
    process::{Command as ProcessCommand, Stdio},
    thread,
    time::Duration,
};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

#[test]
fn help_lists_primary_workflows() {
    let mut command = Command::cargo_bin("xfer").unwrap();
    command
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("send"))
        .stdout(predicate::str::contains("receive"))
        .stdout(predicate::str::contains("discover"))
        .stdout(predicate::str::contains("update"))
        .stdout(predicate::str::contains("tui"));
}

#[test]
fn dry_run_reports_a_transfer_without_connecting() {
    let directory = tempdir().unwrap();
    let file = directory.path().join("payload.txt");
    std::fs::write(&file, b"payload").unwrap();

    let mut command = Command::cargo_bin("xfer").unwrap();
    command
        .args(["send", "example.invalid"])
        .arg(&file)
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains("payload.txt"))
        .stdout(predicate::str::contains("7 B"));
}

#[test]
fn version_matches_compiled_release_version() {
    let mut command = Command::cargo_bin("xfer").unwrap();
    command
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::eq(format!("xfer {}\n", xfer::VERSION)));
}

#[test]
fn dry_run_json_has_stable_machine_readable_fields() {
    let directory = tempdir().unwrap();
    let file = directory.path().join("payload.txt");
    fs::write(&file, b"payload").unwrap();

    let output = Command::cargo_bin("xfer")
        .unwrap()
        .args(["--json", "send", "example.invalid"])
        .arg(&file)
        .arg("--dry-run")
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["root_name"], "payload.txt");
    assert_eq!(value["kind"], "file");
    assert_eq!(value["total_bytes"], 7);
    assert_eq!(value["file_count"], 1);
    assert_eq!(value["entry_count"], 1);
}

#[test]
fn dry_run_rejects_invalid_exclude_glob() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("payload");
    fs::create_dir(&source).unwrap();

    Command::cargo_bin("xfer")
        .unwrap()
        .args(["send", "example.invalid"])
        .arg(&source)
        .args(["--dry-run", "--exclude", "["])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid exclude pattern"));
}

#[test]
fn receive_help_documents_discovery_opt_out() {
    let mut command = Command::cargo_bin("xfer").unwrap();
    command
        .args(["receive", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--no-discovery"))
        .stdout(predicate::str::contains("local network"));
}

#[test]
fn update_help_documents_release_pinning() {
    let mut command = Command::cargo_bin("xfer").unwrap();
    command
        .args(["update", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--version"))
        .stdout(predicate::str::contains("2026.07.16.2"));
}

#[test]
fn doctor_json_reports_identity_network_and_discovery() {
    let directory = tempdir().unwrap();
    let output = Command::cargo_bin("xfer")
        .unwrap()
        .args([
            "--json",
            "--config-dir",
            directory.path().to_str().unwrap(),
            "doctor",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "ok");
    assert_eq!(value["version"], xfer::VERSION);
    assert_eq!(value["default_port"], 9_000);
    assert!(
        value["identity_fingerprint"]
            .as_str()
            .unwrap()
            .contains(':')
    );
    assert_eq!(value["discovery_multicast"], "239.255.90.90:39090");
    assert!(directory.path().join("identity.key").exists());
}

#[test]
fn ip_json_is_a_valid_array() {
    let output = Command::cargo_bin("xfer")
        .unwrap()
        .args(["--json", "ip"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value.is_array());
}

#[test]
fn discover_timeout_is_bounded() {
    Command::cargo_bin("xfer")
        .unwrap()
        .args(["discover", "--timeout", "0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("1..=60"));

    Command::cargo_bin("xfer")
        .unwrap()
        .args(["discover", "--timeout", "61"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("1..=60"));
}

#[test]
fn peer_mutations_emit_json_in_json_mode() {
    let directory = tempdir().unwrap();
    std::fs::write(
        directory.path().join("known_peers.json"),
        br#"{
  "peers": {
    "receiver:9000": {
      "fingerprint": "abcd",
      "first_seen": 1,
      "last_seen": 1
    }
  }
}"#,
    )
    .unwrap();

    let forget = Command::cargo_bin("xfer")
        .unwrap()
        .args([
            "--json",
            "--config-dir",
            directory.path().to_str().unwrap(),
            "peers",
            "forget",
            "receiver:9000",
        ])
        .output()
        .unwrap();
    assert!(forget.status.success());
    let forget_json: serde_json::Value = serde_json::from_slice(&forget.stdout).unwrap();
    assert_eq!(forget_json["action"], "forgot");
    assert_eq!(forget_json["endpoint"], "receiver:9000");

    let clear = Command::cargo_bin("xfer")
        .unwrap()
        .args([
            "--json",
            "--config-dir",
            directory.path().to_str().unwrap(),
            "peers",
            "clear",
            "--yes",
        ])
        .output()
        .unwrap();
    assert!(clear.status.success());
    let clear_json: serde_json::Value = serde_json::from_slice(&clear.stdout).unwrap();
    assert_eq!(clear_json["action"], "cleared");
}

#[test]
fn peer_list_and_failure_paths_are_explicit() {
    let directory = tempdir().unwrap();
    fs::write(
        directory.path().join("known_peers.json"),
        br#"{
  "peers": {
    "receiver:9000": {
      "fingerprint": "abcd",
      "first_seen": 1,
      "last_seen": 2
    }
  }
}"#,
    )
    .unwrap();

    Command::cargo_bin("xfer")
        .unwrap()
        .args([
            "--config-dir",
            directory.path().to_str().unwrap(),
            "peers",
            "list",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("receiver:9000"))
        .stdout(predicate::str::contains("abcd"));

    Command::cargo_bin("xfer")
        .unwrap()
        .args([
            "--config-dir",
            directory.path().to_str().unwrap(),
            "peers",
            "forget",
            "missing:9000",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no remembered peer"));

    Command::cargo_bin("xfer")
        .unwrap()
        .args([
            "--config-dir",
            directory.path().to_str().unwrap(),
            "peers",
            "clear",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--yes"));
}

#[test]
fn insecure_mode_rejects_shared_tokens_before_network_use() {
    let directory = tempdir().unwrap();
    let file = directory.path().join("payload.txt");
    fs::write(&file, b"payload").unwrap();

    Command::cargo_bin("xfer")
        .unwrap()
        .args(["send", "127.0.0.1"])
        .arg(&file)
        .args(["--insecure", "--token", "secret"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--token can only be used with secure transfers",
        ));

    Command::cargo_bin("xfer")
        .unwrap()
        .args(["receive", "--insecure", "--token", "secret"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--token can only be used with secure transfers",
        ));
}

#[test]
fn completion_generation_produces_shell_source() {
    Command::cargo_bin("xfer")
        .unwrap()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_xfer"));
}

#[cfg(not(windows))]
#[test]
fn update_skips_replacement_when_latest_is_already_installed() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().unwrap();
    let release = directory.path().join("release");
    let download = release.join("latest/download");
    let install_directory = directory.path().join("bin");
    fs::create_dir_all(&download).unwrap();
    fs::create_dir(&install_directory).unwrap();
    fs::write(download.join("VERSION"), format!("{}\n", xfer::VERSION)).unwrap();

    let source_binary = std::path::PathBuf::from(Command::cargo_bin("xfer").unwrap().get_program());
    let installed_binary = install_directory.join("xfer");
    fs::copy(&source_binary, &installed_binary).unwrap();
    fs::set_permissions(&installed_binary, fs::Permissions::from_mode(0o755)).unwrap();

    let output = ProcessCommand::new(&installed_binary)
        .args(["--json", "update"])
        .env(
            "XFER_RELEASE_BASE_URL",
            format!("file://{}", release.display()),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "update failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary["status"], "current");
    assert_eq!(
        summary["executable"],
        installed_binary.display().to_string()
    );
    assert_eq!(summary["installed_version"], xfer::VERSION);
}

#[test]
fn cli_insecure_transfer_round_trips_between_processes() {
    let directory = tempdir().unwrap();
    let output_dir = directory.path().join("output");
    let sender_config = directory.path().join("sender-config");
    let receiver_config = directory.path().join("receiver-config");
    let source = directory.path().join("payload.bin");
    let receiver_log = directory.path().join("receiver.log");
    fs::create_dir(&output_dir).unwrap();
    fs::write(
        &source,
        (0_u8..=255).cycle().take(128 * 1024).collect::<Vec<_>>(),
    )
    .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let binary = Command::cargo_bin("xfer")
        .unwrap()
        .get_program()
        .to_os_string();
    let mut receiver = ProcessCommand::new(&binary)
        .args(["--config-dir"])
        .arg(&receiver_config)
        .args([
            "receive",
            "--bind",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--output",
        ])
        .arg(&output_dir)
        .args(["--insecure", "--no-discovery"])
        .stdout(Stdio::null())
        .stderr(Stdio::from(File::create(&receiver_log).unwrap()))
        .spawn()
        .unwrap();

    wait_for_log(&receiver_log, "listening on");

    let sender = ProcessCommand::new(&binary)
        .args(["--config-dir"])
        .arg(&sender_config)
        .args(["send", "127.0.0.1"])
        .arg(&source)
        .args([
            "--port",
            &port.to_string(),
            "--insecure",
            "--connect-timeout",
            "2",
        ])
        .output()
        .unwrap();
    assert!(
        sender.status.success(),
        "sender failed: {}",
        String::from_utf8_lossy(&sender.stderr)
    );

    let status = receiver.wait().unwrap();
    assert!(
        status.success(),
        "receiver failed: {}",
        fs::read_to_string(&receiver_log).unwrap()
    );
    assert_eq!(
        fs::read(output_dir.join("payload.bin")).unwrap(),
        fs::read(source).unwrap()
    );
}

fn wait_for_log(path: &std::path::Path, needle: &str) {
    for _ in 0..100 {
        if fs::read_to_string(path).is_ok_and(|contents| contents.contains(needle)) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "timed out waiting for {needle:?} in {}: {}",
        path.display(),
        fs::read_to_string(path).unwrap_or_default()
    );
}

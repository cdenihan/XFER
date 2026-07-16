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

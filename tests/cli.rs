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

//! Cross-process checks for the local configuration store.
use std::{
    fs::{self, File},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use xfer::{
    config::{Identity, Paths, TrustStore},
    secure_store::SecureDir,
};

#[test]
fn storage_child() {
    let Some(root) = std::env::var_os("XFER_TEST_STORE") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let paths = Paths::discover(Some(root.clone())).unwrap();
    match std::env::var("XFER_TEST_OPERATION").unwrap().as_str() {
        "identity" => {
            let identity = Identity::load_or_create(&paths).unwrap();
            fs::write(
                root.join(std::env::var("XFER_TEST_RESULT").unwrap()),
                identity.public().as_bytes(),
            )
            .unwrap();
        }
        "peer" => {
            let endpoint = std::env::var("XFER_TEST_RESULT").unwrap();
            TrustStore::update(&paths, |store| {
                store.remember(endpoint, "fingerprint".into());
                Ok(())
            })
            .unwrap();
        }
        "lock" => {
            let directory = SecureDir::discover("xfer", Some(root.clone())).unwrap();
            let _lock = directory.lock_exclusive("held.lock").unwrap();
            fs::write(root.join("ready"), b"ready").unwrap();
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }
        operation => panic!("unknown test operation {operation}"),
    }
}
fn child(root: &std::path::Path, operation: &str, result: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "storage_child", "--nocapture"])
        .env("XFER_TEST_STORE", root)
        .env("XFER_TEST_OPERATION", operation)
        .env("XFER_TEST_RESULT", result)
        .stdout(Stdio::null())
        .spawn()
        .unwrap()
}
#[test]
fn processes_agree_on_one_identity_and_preserve_all_peer_updates() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut children = (0..8)
        .map(|n| child(root, "identity", &format!("identity-{n}")))
        .collect::<Vec<_>>();
    for process in &mut children {
        assert!(process.wait().unwrap().success());
    }
    let first = fs::read(root.join("identity-0")).unwrap();
    for n in 1..8 {
        assert_eq!(fs::read(root.join(format!("identity-{n}"))).unwrap(), first);
    }
    let mut children = (0..8)
        .map(|n| child(root, "peer", &format!("peer-{n}")))
        .collect::<Vec<_>>();
    for process in &mut children {
        assert!(process.wait().unwrap().success());
    }
    let paths = Paths::discover(Some(root.to_path_buf())).unwrap();
    assert_eq!(TrustStore::load(&paths).unwrap().iter().count(), 8);
}
#[test]
fn process_exit_releases_a_file_lock() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut process = child(root, "lock", "");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("ready").exists() {
        if Instant::now() > deadline {
            process.kill().unwrap();
            process.wait().unwrap();
            panic!("lock worker did not become ready");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let file = File::options()
        .read(true)
        .write(true)
        .open(root.join("held.lock"))
        .unwrap();
    assert!(matches!(
        file.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    process.kill().unwrap();
    process.wait().unwrap();
    file.try_lock().unwrap();
    file.unlock().unwrap();
}
#[test]
fn failed_peer_update_preserves_previous_data() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths::discover(Some(directory.path().to_path_buf())).unwrap();
    TrustStore::update(&paths, |store| {
        store.remember("original".into(), "key".into());
        Ok(())
    })
    .unwrap();
    let before = fs::read(paths.peers()).unwrap();
    let result = TrustStore::update::<()>(&paths, |store| {
        store.clear();
        Err(xfer::error::XferError::invalid_input("abort"))
    });
    assert!(result.is_err());
    assert_eq!(fs::read(paths.peers()).unwrap(), before);
}

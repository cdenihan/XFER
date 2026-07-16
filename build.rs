use std::fs;

fn main() {
    println!("cargo:rerun-if-changed=VERSION");
    let version = fs::read_to_string("VERSION").expect("could not read VERSION");
    let version = version.trim();
    assert!(
        is_release_version(version),
        "VERSION must use YYYY.MM.DD.N format"
    );
    println!("cargo:rustc-env=XFER_SOURCE_VERSION={version}");
}

fn is_release_version(version: &str) -> bool {
    let parts = version.split('.').collect::<Vec<_>>();
    parts.len() == 4
        && parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && parts[3].bytes().any(|byte| byte != b'0')
}

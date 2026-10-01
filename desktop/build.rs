use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/xfer.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("assets/xfer.ico");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let source = output.join("xfer-icon.rc");
    let resource = output.join("xfer-icon.res");
    fs::write(
        &source,
        format!(
            "1 ICON \"{}\"\n",
            icon.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    let compiler =
        find_resource_compiler().expect("Windows SDK resource compiler rc.exe is required");
    let status = Command::new(compiler)
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource)
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success(), "Windows icon resource compilation failed");
    println!("cargo:rustc-link-arg-bins={}", resource.display());
}

fn find_resource_compiler() -> Option<PathBuf> {
    if let Ok(output) = Command::new("where.exe").arg("rc.exe").output()
        && output.status.success()
        && let Some(path) = String::from_utf8_lossy(&output.stdout).lines().next()
    {
        return Some(PathBuf::from(path.trim()));
    }
    let root = PathBuf::from(env::var_os("ProgramFiles(x86)")?).join("Windows Kits/10/bin");
    let mut versions: Vec<_> = fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    versions.sort();
    let host = env::var("HOST").ok()?;
    let architecture = if host.starts_with("aarch64") {
        "arm64"
    } else {
        "x64"
    };
    versions
        .into_iter()
        .rev()
        .map(|version| version.join(architecture).join("rc.exe"))
        .find(|path| path.is_file())
}

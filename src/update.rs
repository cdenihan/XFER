use std::{
    cmp::Ordering,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(windows)]
use std::process::Stdio;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{Result, XferError};

const DEFAULT_REPOSITORY: &str = "cdenihan/XFER";

#[derive(Clone, Debug, Serialize)]
pub struct UpdateSummary {
    pub previous_version: String,
    pub installed_version: Option<String>,
    pub executable: PathBuf,
    pub status: &'static str,
}

pub fn update_current(requested_version: &str, quiet_background: bool) -> Result<UpdateSummary> {
    validate_requested_version(requested_version)?;
    let executable = std::env::current_exe()?;
    let repository = std::env::var("XFER_REPOSITORY").unwrap_or_else(|_| DEFAULT_REPOSITORY.into());
    let release_base = std::env::var("XFER_RELEASE_BASE_URL")
        .unwrap_or_else(|_| format!("https://github.com/{repository}/releases"));
    update_executable(
        &executable,
        crate::VERSION,
        &repository,
        release_base.trim_end_matches('/'),
        requested_version,
        quiet_background,
    )
}

fn update_executable(
    executable: &Path,
    previous_version: &str,
    repository: &str,
    release_base: &str,
    requested_version: &str,
    quiet_background: bool,
) -> Result<UpdateSummary> {
    #[cfg(not(windows))]
    let _ = quiet_background;

    validate_executable_name(executable)?;
    validate_release_base(release_base)?;
    let install_directory = executable.parent().ok_or_else(|| {
        XferError::Configuration("executable path has no parent directory".into())
    })?;
    let temporary = tempfile::Builder::new().prefix("xfer-update-").tempdir()?;
    let installer_name = installer_name();
    let installer = temporary.path().join(installer_name);
    let checksum = temporary.path().join(format!("{installer_name}.sha256"));
    let download_base = format!("{release_base}/latest/download");

    download_file(&format!("{download_base}/{installer_name}"), &installer)?;
    download_file(
        &format!("{download_base}/{installer_name}.sha256"),
        &checksum,
    )?;
    verify_checksum(&installer, &checksum)?;

    #[cfg(windows)]
    {
        let temporary = temporary.keep();
        schedule_windows_update(
            &temporary,
            &installer,
            install_directory,
            repository,
            release_base,
            requested_version,
            quiet_background,
        )?;
        Ok(UpdateSummary {
            previous_version: previous_version.into(),
            installed_version: None,
            executable: executable.to_path_buf(),
            status: "scheduled",
        })
    }

    #[cfg(not(windows))]
    {
        let output = Command::new("sh")
            .arg(&installer)
            .arg("--install-dir")
            .arg(install_directory)
            .env("XFER_VERSION", requested_version)
            .env("XFER_REPOSITORY", repository)
            .env("XFER_RELEASE_BASE_URL", release_base)
            .output()
            .map_err(|error| {
                XferError::Configuration(format!("could not launch the XFER installer: {error}"))
            })?;
        if !output.status.success() {
            return Err(XferError::Configuration(format!(
                "the XFER installer failed: {}",
                command_failure_text(&output.stdout, &output.stderr)
            )));
        }
        let installed_version = read_installed_version(executable)?;
        Ok(UpdateSummary {
            previous_version: previous_version.into(),
            installed_version: Some(installed_version),
            executable: executable.to_path_buf(),
            status: "updated",
        })
    }
}

fn validate_executable_name(executable: &Path) -> Result<()> {
    let expected = if cfg!(windows) { "xfer.exe" } else { "xfer" };
    let actual = executable.file_name().and_then(|name| name.to_str());
    if actual != Some(expected) {
        return Err(XferError::Configuration(format!(
            "the running executable is named {:?}, not {expected}; reinstall XFER with the official installer before using `xfer update`",
            actual.unwrap_or("<non-UTF-8>")
        )));
    }
    Ok(())
}

fn validate_release_base(release_base: &str) -> Result<()> {
    if release_base.starts_with("https://") || release_base.starts_with("file://") {
        return Ok(());
    }
    Err(XferError::Configuration(format!(
        "refusing non-HTTPS release URL: {release_base}"
    )))
}

fn validate_requested_version(version: &str) -> Result<()> {
    if version == "latest" {
        return Ok(());
    }
    let version = version.strip_prefix('v').unwrap_or(version);
    if version.is_empty()
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(XferError::invalid_input(format!(
            "invalid release version {version:?}"
        )));
    }
    Ok(())
}

pub fn compare_versions(local: &str, peer: &str) -> Option<Ordering> {
    let local = numeric_version_parts(local)?;
    let peer = numeric_version_parts(peer)?;
    Some(local.cmp(&peer))
}

fn numeric_version_parts(version: &str) -> Option<Vec<u64>> {
    version
        .strip_prefix('v')
        .unwrap_or(version)
        .split('.')
        .map(str::parse)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()
}

fn installer_name() -> &'static str {
    if cfg!(windows) {
        "install.ps1"
    } else {
        "install.sh"
    }
}

fn download_file(url: &str, destination: &Path) -> Result<()> {
    if let Some(path) = url.strip_prefix("file://") {
        fs::copy(path, destination)?;
        return Ok(());
    }
    validate_release_base(url)?;

    #[cfg(windows)]
    {
        let status = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -Uri $env:XFER_UPDATE_URL -OutFile $env:XFER_UPDATE_DEST -UseBasicParsing",
            ])
            .env("XFER_UPDATE_URL", url)
            .env("XFER_UPDATE_DEST", destination)
            .status()
            .map_err(|error| {
                XferError::Configuration(format!(
                    "could not launch PowerShell to download the updater: {error}"
                ))
            })?;
        if !status.success() {
            return Err(XferError::Configuration(format!(
                "could not download {url}"
            )));
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        match Command::new("curl")
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--retry",
                "3",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--tlsv1.2",
                "--output",
            ])
            .arg(destination)
            .arg(url)
            .status()
        {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                return Err(XferError::Configuration(format!(
                    "curl could not download {url} (exit status {status})"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let status = Command::new("wget")
            .arg("-q")
            .arg("-O")
            .arg(destination)
            .arg(url)
            .status()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    XferError::Configuration("curl or wget is required to update XFER".into())
                } else {
                    XferError::Io(error)
                }
            })?;
        if !status.success() {
            return Err(XferError::Configuration(format!(
                "wget could not download {url} (exit status {status})"
            )));
        }
        Ok(())
    }
}

fn verify_checksum(artifact: &Path, checksum_file: &Path) -> Result<()> {
    let contents = fs::read_to_string(checksum_file)?;
    let expected = contents
        .split_whitespace()
        .next()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| XferError::security("release checksum file is malformed"))?;

    let mut file = File::open(artifact)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(XferError::security(
            "updater installer SHA-256 verification failed",
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn read_installed_version(executable: &Path) -> Result<String> {
    let output = Command::new(executable)
        .arg("--version")
        .output()
        .map_err(|error| {
            XferError::Configuration(format!("updated executable could not be launched: {error}"))
        })?;
    if !output.status.success() {
        return Err(XferError::Configuration(
            "updated executable did not report its version".into(),
        ));
    }
    let reported = String::from_utf8_lossy(&output.stdout).trim().to_string();
    reported
        .strip_prefix("xfer ")
        .map(str::to_string)
        .ok_or_else(|| {
            XferError::Configuration("updated executable did not identify itself as XFER".into())
        })
}

#[cfg(not(windows))]
fn command_failure_text(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stdout = String::from_utf8_lossy(stdout);
    let message = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if message.is_empty() {
        "no error output was produced".into()
    } else {
        message
            .chars()
            .flat_map(char::escape_default)
            .collect::<String>()
    }
}

#[cfg(windows)]
fn schedule_windows_update(
    temporary: &Path,
    installer: &Path,
    install_directory: &Path,
    repository: &str,
    release_base: &str,
    requested_version: &str,
    quiet_background: bool,
) -> Result<()> {
    let wrapper = temporary.join("complete-update.ps1");
    fs::write(
        &wrapper,
        r#"[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][int]$ParentProcessId,
    [Parameter(Mandatory = $true)][string]$Installer,
    [Parameter(Mandatory = $true)][string]$InstallDirectory,
    [Parameter(Mandatory = $true)][string]$Repository,
    [Parameter(Mandatory = $true)][string]$ReleaseBaseUrl,
    [Parameter(Mandatory = $true)][string]$RequestedVersion,
    [Parameter(Mandatory = $true)][string]$TemporaryDirectory
)
$ErrorActionPreference = "Stop"
try {
    Wait-Process -Id $ParentProcessId -ErrorAction SilentlyContinue
    & $Installer -Version $RequestedVersion -InstallDir $InstallDirectory -Repository $Repository -ReleaseBaseUrl $ReleaseBaseUrl -NoModifyPath
}
finally {
    Remove-Item -LiteralPath $TemporaryDirectory -Recurse -Force -ErrorAction SilentlyContinue
}
"#,
    )?;

    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&wrapper)
        .arg("-ParentProcessId")
        .arg(std::process::id().to_string())
        .arg("-Installer")
        .arg(installer)
        .arg("-InstallDirectory")
        .arg(install_directory)
        .arg("-Repository")
        .arg(repository)
        .arg("-ReleaseBaseUrl")
        .arg(release_base)
        .arg("-RequestedVersion")
        .arg(requested_version)
        .arg("-TemporaryDirectory")
        .arg(temporary)
        .stdin(Stdio::null());
    if quiet_background {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    } else {
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    }
    command.spawn().map_err(|error| {
        XferError::Configuration(format!(
            "could not launch the Windows update helper: {error}"
        ))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_base_requires_https_or_local_fixture() {
        assert!(validate_release_base("https://github.com/cdenihan/XFER/releases").is_ok());
        assert!(validate_release_base("file:///tmp/releases").is_ok());
        assert!(validate_release_base("http://example.com/releases").is_err());
    }

    #[test]
    fn release_versions_are_validated_and_compared() {
        assert!(validate_requested_version("latest").is_ok());
        assert!(validate_requested_version("v2026.07.16.2").is_ok());
        assert!(validate_requested_version("../release").is_err());
        assert_eq!(
            compare_versions("2026.07.16.2", "2026.07.16.10"),
            Some(Ordering::Less)
        );
        assert_eq!(
            compare_versions("2026.07.17.1", "2026.07.16.10"),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn checksum_verification_rejects_modified_content() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("artifact");
        let checksum = directory.path().join("artifact.sha256");
        fs::write(&artifact, b"original").unwrap();
        let digest = hex::encode(Sha256::digest(b"original"));
        fs::write(&checksum, format!("{digest}  artifact\n")).unwrap();
        verify_checksum(&artifact, &checksum).unwrap();

        fs::write(&artifact, b"modified").unwrap();
        assert!(verify_checksum(&artifact, &checksum).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_update_replaces_the_selected_installation() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let release = directory.path().join("release");
        let download = release.join("latest/download");
        let install_directory = directory.path().join("installed");
        fs::create_dir_all(&download).unwrap();
        fs::create_dir(&install_directory).unwrap();

        let installer = download.join("install.sh");
        fs::write(&installer, include_bytes!("../scripts/install.sh")).unwrap();
        write_checksum(&installer);

        let artifact_name = current_unix_artifact();
        let artifact = download.join(artifact_name);
        fs::write(&artifact, b"#!/bin/sh\nprintf 'xfer 9.9.9\\n'\n").unwrap();
        fs::set_permissions(&artifact, fs::Permissions::from_mode(0o755)).unwrap();
        write_checksum(&artifact);

        let executable = install_directory.join("xfer");
        fs::write(&executable, b"#!/bin/sh\nprintf 'xfer 1.0.0\\n'\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();

        let summary = update_executable(
            &executable,
            "1.0.0",
            DEFAULT_REPOSITORY,
            &format!("file://{}", release.display()),
            "latest",
            false,
        )
        .unwrap();

        assert_eq!(summary.status, "updated");
        assert_eq!(summary.installed_version.as_deref(), Some("9.9.9"));
        assert_eq!(
            Command::new(&executable)
                .arg("--version")
                .output()
                .unwrap()
                .stdout,
            b"xfer 9.9.9\n"
        );
    }

    #[cfg(not(windows))]
    fn current_unix_artifact() -> &'static str {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "x86_64") => "xfer-macos-x86_64",
            ("macos", "aarch64") => "xfer-macos-aarch64",
            ("linux", "x86_64") => "xfer-linux-x86_64",
            ("linux", "aarch64") => "xfer-linux-aarch64",
            combination => panic!("unsupported test platform: {combination:?}"),
        }
    }

    fn write_checksum(path: &Path) {
        let contents = fs::read(path).unwrap();
        let digest = hex::encode(Sha256::digest(contents));
        let checksum = path.with_file_name(format!(
            "{}.sha256",
            path.file_name().unwrap().to_string_lossy()
        ));
        fs::write(
            checksum,
            format!(
                "{digest}  {}\n",
                path.file_name().unwrap().to_string_lossy()
            ),
        )
        .unwrap();
    }
}

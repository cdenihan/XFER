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

use crate::error::{Result, XferError as Error};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleaseSpec {
    pub binary_name: &'static str,
    pub display_name: &'static str,
    pub repository: &'static str,
    pub environment_prefix: &'static str,
    pub current_version: &'static str,
}

impl ReleaseSpec {
    pub const fn new(
        binary_name: &'static str,
        display_name: &'static str,
        repository: &'static str,
        environment_prefix: &'static str,
        current_version: &'static str,
    ) -> Self {
        Self {
            binary_name,
            display_name,
            repository,
            environment_prefix,
            current_version,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateSummary {
    pub previous_version: String,
    pub installed_version: Option<String>,
    pub executable: PathBuf,
    pub status: &'static str,
}

impl UpdateSummary {
    /// One line a person can read, for the common case where a CLI prints the
    /// outcome rather than piping it somewhere. The serialized form stays
    /// available for `--json`.
    #[must_use]
    pub fn describe(&self, display_name: &str) -> String {
        let executable = self.executable.display();
        match (self.status, self.installed_version.as_deref()) {
            ("current", _) => format!(
                "{display_name} {} is already up to date at {executable}",
                self.previous_version
            ),
            // Windows cannot replace a running executable, so the swap is
            // handed to a helper that waits for this process to exit.
            ("scheduled", _) | (_, None) => format!(
                "Update staged for {executable}; {display_name} is replaced once this process exits."
            ),
            (_, Some(installed)) => format!(
                "Updated {display_name} {} -> {installed} at {executable}",
                self.previous_version
            ),
        }
    }
}

fn update_for_spec(
    spec: &ReleaseSpec,
    requested_version: &str,
    quiet_background: bool,
) -> Result<UpdateSummary> {
    validate_spec(spec)?;
    validate_requested_version(requested_version)?;
    let executable = std::env::current_exe()?;
    let repository_environment = product_environment(spec, "REPOSITORY")?;
    let repository =
        std::env::var(repository_environment).unwrap_or_else(|_| spec.repository.into());
    let release_base_environment = product_environment(spec, "RELEASE_BASE_URL")?;
    let release_base = std::env::var(release_base_environment)
        .unwrap_or_else(|_| format!("https://github.com/{repository}/releases"));
    update_executable(
        spec,
        &executable,
        &repository,
        release_base.trim_end_matches('/'),
        requested_version,
        quiet_background,
    )
}

fn update_executable(
    spec: &ReleaseSpec,
    executable: &Path,
    repository: &str,
    release_base: &str,
    requested_version: &str,
    quiet_background: bool,
) -> Result<UpdateSummary> {
    #[cfg(not(windows))]
    let _ = quiet_background;

    validate_executable_name(spec, executable)?;
    validate_release_base(release_base)?;
    if requested_version != "latest" && versions_equal(requested_version, spec.current_version) {
        return Ok(current_summary(spec, executable));
    }
    let install_directory = executable
        .parent()
        .ok_or_else(|| Error::Configuration("executable path has no parent directory".into()))?;
    let temporary = tempfile::Builder::new()
        .prefix(&format!("{}-update-", spec.binary_name))
        .tempdir()?;
    let installer_name = installer_name();
    let installer = temporary.path().join(installer_name);
    let checksum = temporary.path().join(format!("{installer_name}.sha256"));
    let download_base = format!("{release_base}/latest/download");

    if requested_version == "latest" {
        let latest_version_file = temporary.path().join("VERSION");
        if download_file(&format!("{download_base}/VERSION"), &latest_version_file).is_ok() {
            let latest_version = fs::read_to_string(&latest_version_file)?;
            let latest_version = latest_version.trim();
            validate_requested_version(latest_version)?;
            if versions_equal(latest_version, spec.current_version) {
                return Ok(current_summary(spec, executable));
            }
        }
    }

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
            spec,
            &temporary,
            &installer,
            install_directory,
            repository,
            release_base,
            requested_version,
            quiet_background,
        )?;
        Ok(UpdateSummary {
            previous_version: spec.current_version.into(),
            installed_version: None,
            executable: executable.to_path_buf(),
            status: "scheduled",
        })
    }

    #[cfg(not(windows))]
    {
        let version_environment = product_environment(spec, "VERSION")?;
        let repository_environment = product_environment(spec, "REPOSITORY")?;
        let release_base_environment = product_environment(spec, "RELEASE_BASE_URL")?;
        let output = Command::new("sh")
            .arg(&installer)
            .arg("--install-dir")
            .arg(install_directory)
            .env(version_environment, requested_version)
            .env(repository_environment, repository)
            .env(release_base_environment, release_base)
            .output()
            .map_err(|error| {
                Error::Configuration(format!(
                    "could not launch the {} installer: {error}",
                    spec.display_name
                ))
            })?;
        if !output.status.success() {
            return Err(Error::Configuration(format!(
                "the {} installer failed: {}",
                spec.display_name,
                command_failure_text(&output.stdout, &output.stderr)
            )));
        }
        let installed_version = read_installed_version(spec, executable)?;
        Ok(UpdateSummary {
            previous_version: spec.current_version.into(),
            installed_version: Some(installed_version),
            executable: executable.to_path_buf(),
            status: "updated",
        })
    }
}

fn versions_equal(left: &str, right: &str) -> bool {
    left.strip_prefix('v').unwrap_or(left) == right.strip_prefix('v').unwrap_or(right)
}

fn current_summary(spec: &ReleaseSpec, executable: &Path) -> UpdateSummary {
    UpdateSummary {
        previous_version: spec.current_version.into(),
        installed_version: Some(spec.current_version.into()),
        executable: executable.to_path_buf(),
        status: "current",
    }
}

fn validate_spec(spec: &ReleaseSpec) -> Result<()> {
    if spec.binary_name.is_empty()
        || !spec
            .binary_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(Error::Configuration("invalid binary name".into()));
    }
    if spec.display_name.trim().is_empty()
        || spec.repository.split('/').count() != 2
        || spec.repository.contains(char::is_whitespace)
    {
        return Err(Error::Configuration(
            "display name and owner/repository must be configured".into(),
        ));
    }
    product_environment(spec, "VERSION")?;
    Ok(())
}

fn product_environment(spec: &ReleaseSpec, suffix: &str) -> Result<String> {
    if spec.environment_prefix.is_empty()
        || !spec
            .environment_prefix
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(Error::Configuration(
            "environment prefix must use uppercase ASCII, digits, and underscores".into(),
        ));
    }
    Ok(format!("{}_{suffix}", spec.environment_prefix))
}

fn validate_executable_name(spec: &ReleaseSpec, executable: &Path) -> Result<()> {
    let expected = if cfg!(windows) {
        format!("{}.exe", spec.binary_name)
    } else {
        spec.binary_name.into()
    };
    let actual = executable.file_name().and_then(|name| name.to_str());
    if actual != Some(expected.as_str()) {
        return Err(Error::Configuration(format!(
            "the running executable is named {:?}, not {expected}; reinstall {} with the official installer before using `{} update`",
            actual.unwrap_or("<non-UTF-8>"),
            spec.display_name,
            spec.binary_name
        )));
    }
    Ok(())
}

fn validate_release_base(release_base: &str) -> Result<()> {
    if release_base.starts_with("https://") || release_base.starts_with("file://") {
        return Ok(());
    }
    Err(Error::Configuration(format!(
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
        return Err(Error::InvalidInput(format!(
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

const fn installer_name() -> &'static str {
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
                Error::Configuration(format!(
                    "could not launch PowerShell to download the updater: {error}"
                ))
            })?;
        if !status.success() {
            return Err(Error::Configuration(format!("could not download {url}")));
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
                return Err(Error::Configuration(format!(
                    "curl could not download {url} (exit status {status})"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        download_with_wget(url, destination)
    }
}

#[cfg(not(windows))]
fn download_with_wget(url: &str, destination: &Path) -> Result<()> {
    let mut url = url.to_string();
    for redirects in 0..=20 {
        let output = Command::new("wget")
            .args([
                "--server-response",
                "--max-redirect=0",
                "--tries=1",
                "--timeout=30",
                "-O",
            ])
            .arg(destination)
            .arg(&url)
            .output()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Error::Configuration("curl or GNU wget is required to update this CLI".into())
                } else {
                    Error::Io(error)
                }
            })?;
        if output.status.success() {
            return Ok(());
        }
        let response = String::from_utf8_lossy(&output.stderr);
        let next = wget_redirect(&url, &response)?;
        match next {
            Some(next) if redirects < 20 => url = next,
            Some(_) => {
                return Err(Error::Configuration(
                    "too many HTTPS download redirects".into(),
                ));
            }
            None => {
                return Err(Error::Configuration(format!(
                    "wget could not download {url}: {}",
                    command_failure_text(&output.stdout, &output.stderr)
                )));
            }
        }
    }
    unreachable!("redirect loop returns on its last iteration")
}

#[cfg(not(windows))]
fn wget_redirect(base: &str, response: &str) -> Result<Option<String>> {
    let mut status = None;
    let mut location = None;
    for line in response.lines().filter_map(|line| line.strip_prefix("  ")) {
        if line.starts_with("HTTP/") {
            status = line
                .split_whitespace()
                .nth(1)
                .and_then(|value| value.parse::<u16>().ok());
            location = None;
        } else if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("location")
        {
            location = Some(value.trim());
        }
    }
    if !matches!(status, Some(301 | 302 | 303 | 307 | 308)) {
        return Ok(None);
    }
    let location = location
        .ok_or_else(|| Error::Configuration("HTTPS redirect has no Location header".into()))?;
    if location.is_empty() || location.chars().any(char::is_control) || location.contains('\\') {
        return Err(Error::Security("invalid HTTPS redirect location".into()));
    }
    if location.starts_with("https://") {
        return Ok(Some(location.to_string()));
    }
    if location.starts_with("//") {
        return Ok(Some(format!("https:{location}")));
    }
    // A colon in the first path segment identifies a URI scheme, not a relative path.
    if location
        .split('/')
        .next()
        .is_some_and(|segment| segment.contains(':'))
    {
        return Err(Error::Security(
            "refusing a non-HTTPS download redirect".into(),
        ));
    }
    let rest = base
        .strip_prefix("https://")
        .ok_or_else(|| Error::Security("HTTPS redirect base is invalid".into()))?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let origin = format!("https://{authority}");
    if location.starts_with('/') {
        return Ok(Some(format!("{origin}{location}")));
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    if location.starts_with('?') || location.starts_with('#') {
        return Ok(Some(format!("{origin}/{path}{location}")));
    }
    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    Ok(Some(if parent.is_empty() {
        format!("{origin}/{location}")
    } else {
        format!("{origin}/{parent}/{location}")
    }))
}

fn verify_checksum(artifact: &Path, checksum_file: &Path) -> Result<()> {
    let contents = fs::read_to_string(checksum_file)?;
    let expected = contents
        .split_whitespace()
        .next()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| Error::Security("release checksum file is malformed".into()))?;

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
    let actual = crate::encoding::hex(hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(Error::Security(
            "updater installer SHA-256 verification failed".into(),
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn read_installed_version(spec: &ReleaseSpec, executable: &Path) -> Result<String> {
    let output = Command::new(executable)
        .arg("--version")
        .output()
        .map_err(|error| {
            Error::Configuration(format!("updated executable could not be launched: {error}"))
        })?;
    if !output.status.success() {
        return Err(Error::Configuration(
            "updated executable did not report its version".into(),
        ));
    }
    let reported = String::from_utf8_lossy(&output.stdout).trim().to_string();
    reported
        .strip_prefix(&format!("{} ", spec.binary_name))
        .map(str::to_string)
        .ok_or_else(|| {
            Error::Configuration(format!(
                "updated executable did not identify itself as {}",
                spec.display_name
            ))
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
#[allow(clippy::too_many_arguments)]
fn schedule_windows_update(
    spec: &ReleaseSpec,
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
        Error::Configuration(format!(
            "could not launch the {} Windows update helper: {error}",
            spec.display_name
        ))
    })?;
    Ok(())
}

const RELEASE: ReleaseSpec =
    ReleaseSpec::new("xfer", "XFER", "cdenihan/XFER", "XFER", crate::VERSION);

pub fn update_current(requested_version: &str, quiet_background: bool) -> Result<UpdateSummary> {
    update_for_spec(&RELEASE, requested_version, quiet_background)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: ReleaseSpec =
        ReleaseSpec::new("fixture", "Fixture", "owner/fixture", "FIXTURE", "1.0.0");

    #[test]
    fn every_update_status_describes_itself() {
        let summary = |status, installed: Option<&str>| UpdateSummary {
            previous_version: "1.0.0".into(),
            installed_version: installed.map(Into::into),
            executable: PathBuf::from("/bin/fixture"),
            status,
        };
        assert_eq!(
            summary("current", Some("1.0.0")).describe("Fixture"),
            "Fixture 1.0.0 is already up to date at /bin/fixture"
        );
        assert_eq!(
            summary("updated", Some("1.1.0")).describe("Fixture"),
            "Updated Fixture 1.0.0 -> 1.1.0 at /bin/fixture"
        );
        assert_eq!(
            summary("scheduled", None).describe("Fixture"),
            "Update staged for /bin/fixture; Fixture is replaced once this process exits."
        );
    }

    #[test]
    fn release_base_requires_https_or_local_fixture() {
        assert!(validate_release_base("https://github.com/owner/repo/releases").is_ok());
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
    }

    #[test]
    fn exact_current_version_is_a_no_op() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join(if cfg!(windows) {
            "fixture.exe"
        } else {
            "fixture"
        });
        fs::write(&executable, b"not executed").unwrap();
        let summary = update_executable(
            &SPEC,
            &executable,
            SPEC.repository,
            "https://example.invalid/releases",
            "v1.0.0",
            true,
        )
        .unwrap();
        assert_eq!(summary.status, "current");
        assert_eq!(summary.installed_version.as_deref(), Some("1.0.0"));
    }

    #[cfg(not(windows))]
    #[test]
    fn latest_marker_skips_installer_when_current() {
        let directory = tempfile::tempdir().unwrap();
        let release = directory.path().join("release/latest/download");
        fs::create_dir_all(&release).unwrap();
        fs::write(release.join("VERSION"), "1.0.0\n").unwrap();
        let executable = directory.path().join("fixture");
        fs::write(&executable, b"not executed").unwrap();
        let summary = update_executable(
            &SPEC,
            &executable,
            SPEC.repository,
            &format!("file://{}", directory.path().join("release").display()),
            "latest",
            true,
        )
        .unwrap();
        assert_eq!(summary.status, "current");
    }

    #[test]
    fn release_spec_validates_product_identity() {
        validate_spec(&SPEC).unwrap();
        let invalid = ReleaseSpec::new("bad name", "Fixture", "owner/repo", "FIXTURE", "1");
        assert!(validate_spec(&invalid).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn wget_rejects_redirect_downgrades_and_resolves_https_locations() {
        let redirect = |location: &str| format!("  HTTP/1.1 302 Found\n  Location: {location}\n");
        let base = "https://example.test/releases/installer";
        for location in [
            "http://example.test/installer",
            "ftp://example.test/installer",
            "file:///tmp/installer",
        ] {
            assert!(matches!(
                wget_redirect(base, &redirect(location)),
                Err(Error::Security(_))
            ));
        }
        for (location, expected) in [
            ("https://cdn.test/file", "https://cdn.test/file"),
            ("//cdn.test/file", "https://cdn.test/file"),
            ("/file", "https://example.test/file"),
            ("next", "https://example.test/releases/next"),
            (
                "?download=1",
                "https://example.test/releases/installer?download=1",
            ),
        ] {
            assert_eq!(
                wget_redirect(base, &redirect(location)).unwrap().as_deref(),
                Some(expected)
            );
        }
        assert!(
            wget_redirect(base, "  HTTP/1.1 404 Not Found\n  Location: /file\n")
                .unwrap()
                .is_none()
        );
        assert!(wget_redirect(base, "  HTTP/1.1 302 Found\n").is_err());
    }
    #[test]
    fn checksum_verification_rejects_modified_content() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("artifact");
        let checksum = directory.path().join("artifact.sha256");
        fs::write(&artifact, b"original").unwrap();
        let digest = crate::encoding::hex(Sha256::digest(b"original"));
        fs::write(&checksum, format!("{digest}  artifact\n")).unwrap();
        verify_checksum(&artifact, &checksum).unwrap();

        fs::write(&artifact, b"modified").unwrap();
        assert!(verify_checksum(&artifact, &checksum).is_err());
    }
}

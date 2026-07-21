# Installing XFER

Official releases provide binaries and checksum files for:

| Operating system | Architectures | Runtime |
| --- | --- | --- |
| Linux | x86_64, ARM64 | GNU libc and musl |
| macOS | Intel, Apple Silicon | native |
| Windows | x86_64, ARM64 | MSVC |

The installers download only from HTTPS by default, verify the release
SHA-256 file before replacing anything, run the downloaded binary as a
compatibility check, and install through a temporary file so a failed upgrade
does not destroy the existing executable.

## Updating an existing installation

After installing an official release, update the executable currently being
used by your shell:

```console
xfer update
xfer update --version 2026.07.16.2
```

The command resolves the running executable and reads the latest release's
`VERSION` asset. If that version is already installed, XFER reports that it is
current and leaves the executable untouched. Otherwise it downloads the latest
platform installer and checksum, verifies SHA-256, and asks that installer to
replace XFER in the same directory. On Windows, the command starts a helper that
waits for the running `xfer.exe` process to exit before completing the
replacement. Use `--version` to pin the installation to a specific published
release; requesting the installed version is also a no-op.

During transfers, current XFER releases exchange their release versions. When
they differ, the older interactive CLI offers to update to the newer peer's
exact release after the transfer completes. Non-interactive sessions print the
command to run, while `--json` emits a `version_mismatch` event.

`XFER_REPOSITORY` and `XFER_RELEASE_BASE_URL` apply to updates as well as initial
installation. The target directory must be writable by the current user.

## Linux and macOS

Install the latest release:

```console
curl -fsSL https://github.com/cdenihan/XFER/releases/latest/download/install.sh | sh
```

The default destination is `~/.local/bin/xfer`. If that directory is not in
`PATH`, the installer prints the directory to add.

On Linux, the installer selects the musl binary by default. This avoids a
dependency on the host system's glibc version while retaining the GNU builds
for users who need them. Select the GNU binary explicitly with `--libc gnu`:

```console
curl -fsSL https://github.com/cdenihan/XFER/releases/latest/download/install.sh \
  | sh -s -- --libc gnu
```

Pin a release or choose another destination:

```console
curl -fsSL https://github.com/cdenihan/XFER/releases/latest/download/install.sh \
  | sh -s -- --version v2026.07.16.2 --install-dir "$HOME/bin"
```

Equivalent environment variables are `XFER_VERSION`, `XFER_INSTALL_DIR`, and
`XFER_LIBC`. Set `XFER_LIBC=gnu` for the GNU override. The script requires
`curl` or `wget`, plus one of `sha256sum`, `shasum`, or `openssl`.

## Windows

Install the latest release from PowerShell:

```powershell
irm https://github.com/cdenihan/XFER/releases/latest/download/install.ps1 | iex
```

The default destination is
`%LOCALAPPDATA%\Programs\XFER\bin\xfer.exe`. The installer adds that directory
to the current user's `PATH`; open a new terminal afterward.

To pin a version or suppress the PATH update, save and invoke the script:

```powershell
$installer = Join-Path $env:TEMP "install-xfer.ps1"
irm https://github.com/cdenihan/XFER/releases/latest/download/install.ps1 -OutFile $installer
& $installer -Version v2026.07.16.42 -InstallDir "$HOME\bin" -NoModifyPath
```

Equivalent environment variables are `XFER_VERSION` and `XFER_INSTALL_DIR`.
The script supports Windows PowerShell 5.1 and newer PowerShell releases.

## Mirrors and private release proxies

Set `XFER_RELEASE_BASE_URL` to a release root with the same layout as GitHub
Releases:

```text
<base>/latest/download/<artifact>
<base>/download/<tag>/<artifact>
```

Set `XFER_REPOSITORY` to change the GitHub owner/repository while retaining the
standard GitHub release URL. Non-HTTPS network downloads are rejected; local
`file://` URLs are supported for offline testing.

## Manual installation

Download the binary and matching `.sha256` file from the GitHub release page.
Verify SHA-256, rename the binary to `xfer` (`xfer.exe` on Windows), make it
executable on Unix, and place it in a directory on `PATH`.

Run these checks afterward:

```console
xfer --version
xfer doctor
```

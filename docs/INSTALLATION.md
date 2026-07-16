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

## Linux and macOS

Install the latest release:

```console
curl -fsSL https://github.com/cdenihan/XFER/releases/latest/download/install.sh | sh
```

The default destination is `~/.local/bin/xfer`. If that directory is not in
`PATH`, the installer prints the directory to add.

Pin a release or choose another destination:

```console
curl -fsSL https://github.com/cdenihan/XFER/releases/latest/download/install.sh \
  | sh -s -- --version v4.0.0 --install-dir "$HOME/bin"
```

Equivalent environment variables are `XFER_VERSION` and `XFER_INSTALL_DIR`.
The script requires `curl` or `wget`, plus one of `sha256sum`, `shasum`, or
`openssl`.

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
& $installer -Version v4.0.0 -InstallDir "$HOME\bin" -NoModifyPath
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

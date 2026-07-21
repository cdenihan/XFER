# XFER

XFER is a secure, direct file-transfer tool for Windows, macOS, and Linux. It
sends a file or directory over a single TCP connection, with no account, cloud
service, or server deployment.

The v4 rewrite is a library-first Rust application with a
[clap](https://github.com/clap-rs/clap) CLI and a
[Ratatui](https://github.com/ratatui/ratatui) terminal interface.

## Highlights

- Secure by default: X25519 key agreement, HKDF-SHA-256, and
  ChaCha20-Poly1305 authenticated encryption
- Trust on first use (TOFU) with a human-verifiable 10-digit security code
- Streaming file and directory transfer with bounded memory use
- Per-file SHA-256 and aggregate manifest verification
- Atomic receive staging: unverified data never appears at the final path
- IPv4 and IPv6 support over one configurable port
- Passive same-LAN receiver discovery with no subnet or port scanning
- Collision-safe destination naming and explicit `--overwrite`
- Exclusion globs, safe symlink handling, and a no-network `--dry-run`
- Optional shared token mixed into key derivation
- Human progress, newline-delimited JSON events, and a live TUI
- Peer-management, diagnostics, and shell-completion commands
- A checksum-verified `xfer update` command that replaces the active installation
- No-op update checks when the installed release is already current
- Native CI on Linux, macOS, and Windows plus cross-target checks

## Install

Linux or macOS:

```console
curl -fsSL https://github.com/cdenihan/XFER/releases/latest/download/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://github.com/cdenihan/XFER/releases/latest/download/install.ps1 | iex
```

The installers detect the operating system, CPU architecture, and Linux libc;
download the matching release binary; verify its SHA-256 file; and replace an
existing installation atomically. See [docs/INSTALLATION.md](docs/INSTALLATION.md)
for version pinning, install locations, PATH behavior, mirrors, and manual
installation.

After the first install, update that same executable in place:

```console
xfer update
```

## Quick start

Install the same XFER version on both machines.

On the receiving machine:

```console
xfer receive --output ~/Downloads
```

On the sending machine, open the TUI and choose the discovered receiver:

```console
xfer tui
```

Or send directly to an address:

```console
xfer send 192.168.1.42 ./photos
```

The first secure connection displays the same security code on both machines.
Compare the codes before approving the receiver on the sending machine. The
receiver identity is remembered for future transfers. An identity change always
requires manual confirmation.

Receivers advertise only while `xfer receive` is waiting. Discovery uses one
small, link-local multicast announcement rather than probing machines or ports.
If multicast is unavailable, use the receiver address shown in the TUI or by
`xfer ip`.

## Terminal interface

Launch the Ratatui interface with:

```console
xfer tui
```

The TUI provides send and receive forms, security and overwrite controls, live
progress, activity logs, and an in-app peer confirmation dialog. Send mode
automatically lists every active XFER receiver detected on the same LAN; use the
arrow keys and Enter to select one. Receive mode shows the local IP addresses
and ports another machine can use.

## CLI

### Send

```console
xfer send <HOST> <PATH>
```

Common options:

```console
# Different port
xfer send 192.168.1.42 ./payload --port 9100

# Exclude directory content
xfer send 192.168.1.42 ./project \
  --exclude '.git' \
  --exclude 'target/**'

# Inspect the plan without connecting
xfer send example.invalid ./project --dry-run

# Non-interactively trust a previously unseen identity
xfer send 192.168.1.42 ./payload --accept-new

# Add a shared secret without exposing it in shell history
XFER_TOKEN='correct horse battery staple' \
  xfer send 192.168.1.42 ./payload
```

Symlinks are skipped by default. `--follow-links` follows only links whose
resolved targets remain inside the transfer root.

### Receive

```console
xfer receive --output ./downloads
```

### Update

```console
xfer update
xfer update --version 2026.07.16.2
```

The updater reads the latest release's small `VERSION` marker first. If the
installed release is current it exits without replacing the executable;
otherwise it verifies the installer and uses it to replace the currently
running XFER installation.
On Windows, replacement finishes immediately after the current process exits.
Installations in protected system directories may require reinstalling to a
user-writable directory first.

XFER also exchanges release versions during a transfer. If the versions differ,
the older interactive CLI warns and offers to update to the peer's exact release.
Non-interactive and JSON sessions report the mismatch without prompting.

The receiver accepts one transfer, verifies it, writes it to the destination,
and exits. If the destination name already exists, XFER chooses a numbered name
such as `photo (1).jpg`. Use `--overwrite` to replace the exact destination.
The receiver is discoverable on the local network by default. Use
`--no-discovery` when you want to require manual address entry.

To bind a specific interface:

```console
xfer receive --bind 0.0.0.0 --port 9100
```

The default bind address is `::`, configured as a dual-stack socket where the
operating system supports it.

### Automation

`--json` emits newline-delimited JSON status, progress, SAS, and completion
events:

```console
xfer --json send 192.168.1.42 ./artifact --accept-new
```

Secure automation must either use an already remembered peer or opt into
`--accept-new`. A changed identity is never accepted automatically.

### Utilities

```console
xfer ip
xfer discover
xfer doctor
xfer peers list
xfer peers forget 192.168.1.42:9000
xfer peers clear --yes
xfer completions zsh
```

Global configuration options:

- `--config-dir <PATH>` or `XFER_CONFIG_DIR`: override `~/.xfer`
- `--json`: emit machine-readable events

Set `XFER_NAME` on a receiver to override the machine label shown during LAN
discovery.

### Insecure mode

`--insecure` (alias `--no-secure`) disables encryption and peer authentication.
It must be set on both sender and receiver.

```console
xfer receive --insecure
xfer send 192.168.1.42 ./payload --insecure
```

SHA-256 integrity checks still run, but they do not protect against an active
attacker because the hashes travel over the same unauthenticated connection.
Use insecure mode only for controlled compatibility or debugging.

## Security model

The receiver stores a persistent X25519 identity in
`~/.xfer/identity.key`. The sender pins the receiver public-key fingerprint in
`~/.xfer/known_peers.json`.

Each connection uses:

1. protocol and security-mode negotiation;
2. the receiver static public key and fresh random nonce;
3. a sender ephemeral X25519 key and fresh random nonce;
4. HKDF-SHA-256 directional keys and nonce prefixes;
5. an encrypted readiness exchange before a new identity can be persisted;
6. sequence-numbered ChaCha20-Poly1305 records;
7. per-file and aggregate SHA-256 verification.

The security code authenticates the first connection when users compare it on
both machines. See [SECURITY.md](SECURITY.md) for the threat model and
[docs/PROTOCOL.md](docs/PROTOCOL.md) for the wire format.

## Build and test

The repository tracks the current stable Rust toolchain. The crate metadata
records Rust 1.88 as the minimum version accepted by the latest dependency set.

```console
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo build --release --locked
cargo audit
```

The resulting executable is `target/release/xfer` (or `xfer.exe` on Windows).
Install the optional audit command with `cargo install cargo-audit --locked`.

For contributor architecture, test strategy, and release details, see
[docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Current limitations

- Each `receive` invocation accepts one transfer and exits.
- Interrupted transfers restart; resumable chunks are not implemented.
- File metadata such as ownership, ACLs, and extended attributes is not copied.
- Entry names must be valid UTF-8 and portable across Windows, macOS, and Linux;
  case-only collisions and Windows-reserved names are rejected.
- Automatic discovery currently uses IPv4 multicast; direct transfers continue
  to support both IPv4 and IPv6.

These constraints keep the protocol small, deterministic, and auditable.

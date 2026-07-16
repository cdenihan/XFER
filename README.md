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
- Collision-safe destination naming and explicit `--overwrite`
- Exclusion globs, safe symlink handling, and a no-network `--dry-run`
- Optional shared token mixed into key derivation
- Human progress, newline-delimited JSON events, and a live TUI
- Peer-management, diagnostics, and shell-completion commands
- Native CI on Linux, macOS, and Windows plus cross-target checks

## Quick start

Install the same XFER version on both machines.

On the receiving machine:

```console
xfer receive --output ~/Downloads
```

On the sending machine:

```console
xfer send 192.168.1.42 ./photos
```

The first secure connection displays the same security code on both machines.
Compare the codes before approving the receiver on the sending machine. The
receiver identity is remembered for future transfers. An identity change always
requires manual confirmation.

Use `xfer ip` on the receiver if you need its local address.

## Terminal interface

Launch the Ratatui interface with:

```console
xfer tui
```

The TUI provides send and receive forms, security and overwrite controls, live
progress, activity logs, and an in-app peer confirmation dialog.

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

The receiver accepts one transfer, verifies it, writes it to the destination,
and exits. If the destination name already exists, XFER chooses a numbered name
such as `photo (1).jpg`. Use `--overwrite` to replace the exact destination.

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
xfer doctor
xfer peers list
xfer peers forget 192.168.1.42:9000
xfer peers clear --yes
xfer completions zsh
```

Global configuration options:

- `--config-dir <PATH>` or `XFER_CONFIG_DIR`: override `~/.xfer`
- `--json`: emit machine-readable events

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
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release
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
- Discovery is intentionally explicit; XFER does not broadcast presence on the
  network.

These constraints keep the protocol small, deterministic, and auditable.

# XFER (Rust)

XFER is a LAN file and directory transfer tool focused on **effortless setup** with **advanced controls**.

## What you get

- Single executable on each machine
- Secure-by-default transfers (TOFU + SAS + E2E encryption)
- File and directory transfer with integrity verification (SHA-256)
- Multi-channel transport for throughput and observability
- Interactive TUI mode with setup stats and live stream updates
- Sender and receiver progress bars with speed + file/overall percentages
- Advanced configuration via CLI flags (especially ports/security)

---

## Architecture

The app is split into functional sections:

- `src/main.rs` — protocol/security/transfer engine and CLI entrypoint
- `src/client.rs` — sender orchestration
- `src/server.rs` — receiver orchestration
- `src/tui.rs` — interactive terminal UI workflow

### Data flow

1. **Control channel** performs secure pairing and key setup.
2. **Data channel** sends file bytes or streaming tar data.
3. **Meta channel** sends checksum/manifest for verification.
4. **Status channel** emits progress/stats updates.
5. **Heartbeat channel** emits liveness signals.

---

## Port model (important)

You configure one **data port** (`--port`, default `9000`).
All other channels are derived from it.

| Channel | Port |
|---|---|
| Control (SAS) | `port - 1` |
| Data | `port` |
| Meta | `port + 1` |
| Status | `port + 2` |
| Heartbeat | `port + 3` |

### Example

If `--port 9100`, channels are:
- control `9099`
- data `9100`
- meta `9101`
- status `9102`
- heartbeat `9103`

---

## Quick start (2 machines)

### Receiver

```bash
xfer receive --out ./downloads
```

### Sender

```bash
xfer send <RECEIVER_IP> ./payload
```

That’s it. First secure run prompts both sides with the same SAS code.

---

## TUI mode

Run:

```bash
xfer tui
```

TUI features:

- guided send/receive setup
- per-transfer setup stats (file count/bytes for directories)
- explicit channel port display before transfer
- secure mode + default ports automatically applied
- optional advanced overrides for port/security only when requested
- optional excludes
- live status and heartbeat output during transfer
- sender + receiver single-line progress with throughput, file %, overall %, and ETA

---

## CLI reference

### Show local IPs

```bash
xfer ip
```

`xfer ip` only lists non-loopback IPv4 addresses to show endpoints useful for peer-to-peer transfers.

### Receive (auto file/dir)

```bash
xfer receive --out ./downloads --port 9000
```

### Receive file only

```bash
xfer recv-file ./out.bin --port 9000 --force
```

### Receive dir only

```bash
xfer recv-dir ./out-dir --port 9000
```

### Send file/dir

```bash
xfer send 192.168.1.42 ./payload --port 9000 --exclude ".git/*"
```

### Optional flags

- `--port <N>` — base data port
- `--force` — overwrite file destination where applicable
- `--exclude <PATTERN>` — repeatable fnmatch-style filter for dir send
- `--insecure` / `--no-secure` — disables TOFU/SAS and E2E encryption

---

## Security model

### Default secure mode

- TOFU peer trust store in `~/.xfer/known_peers`
- persistent receiver identity at `~/.xfer/identity.key`
- SAS confirmation on first trust (or key changes)
- X25519 key agreement + ChaCha20-Poly1305 encrypted streams

### Integrity

- Files: checksum verification
- Directories: manifest verification of extracted files

---

## Dependency trust / supply-chain notes

Dependencies are intentionally limited to widely used Rust ecosystem crates:

- `clap` (CLI)
- `sha2`, `hmac`, `hex` (hash/auth primitives)
- `x25519-dalek`, `chacha20poly1305`, `rand` (crypto/keying)
- `tar`, `walkdir`, `glob` (file/dir transfer operations)

These are mainstream crates with broad community adoption.

---

## Build and test

```bash
cargo check
cargo test
cargo build --release
```

---

## Release automation

Tag pushes (`v*`) trigger GitHub Actions to:

1. build release binaries for Linux (GNU + musl, x86_64 + ARM64), macOS (Intel + Apple Silicon), and Windows (x86_64 + ARM64)
2. copy artifacts and verify SHA-256 copy integrity
3. retry copy if hash mismatch
4. fail release if mismatch persists
5. publish GitHub Release assets

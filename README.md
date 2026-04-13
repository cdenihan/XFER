# XFER (Rust)

XFER is a simple LAN file/directory transfer tool with:

- file + directory transfer
- transfer integrity verification (SHA-256)
- TOFU + SAS peer trust prompts
- end-to-end encryption (ChaCha20-Poly1305)
- minimal, single-binary CLI

## Install

```bash
cargo build --release
./target/release/xfer --help
```

## Usage

### Show local IP

```bash
xfer ip
```

### Receive (auto file/dir)

```bash
xfer receive --out ./downloads
```

### Send file/dir

```bash
xfer send 192.168.1.42 ./payload
```

### Enforced modes

```bash
xfer recv-file ./out.bin
xfer recv-dir ./out-dir
```

### Optional flags

- `--port <N>` (default `9000`)
- `--force` (file receive overwrite)
- `--exclude <PATTERN>` (repeatable for `send` dir mode)
- `--insecure` / `--no-secure` (disable TOFU/SAS + encryption; legacy mode)

## Trust setup

- First secure connection shows a shared SAS code on both peers.
- Accept once to store trust at `~/.xfer/known_peers` keyed by `<ip>:<port>`.
- Receiver identity key is kept at `~/.xfer/identity.key`.

## Protocol summary

- Control channel: `port-1` for TOFU + SAS + key agreement
- Data channel: `port` for file or tar stream
- Meta channel: `port+1` for checksum/manifest verification

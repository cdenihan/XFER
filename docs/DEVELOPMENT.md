# Development guide

## Project shape

XFER is one package with a library and a thin binary:

| Module | Responsibility |
| --- | --- |
| `cli` | clap command model and command dispatch |
| `config` | identity, permissions, and TOFU peer persistence |
| `crypto` | key derivation, fingerprints, SAS, and AEAD helpers |
| `filesystem` | source planning, exclusions, safe paths, destination naming |
| `net` | dual-stack listeners, address discovery, and connection setup |
| `protocol` | negotiation, typed messages, and framed record transport |
| `reporter` | presentation-neutral status, progress, and trust prompts |
| `transfer` | sender/receiver orchestration and verification |
| `tui` | Ratatui forms, worker events, progress, and peer confirmation |

The CLI and TUI call the same `transfer` APIs. Network and filesystem behavior
must not be reimplemented in a presentation layer.

## Toolchain

`rust-toolchain.toml` tracks the current stable Rust toolchain. The crate metadata
records Rust 1.88 as the minimum accepted by the current dependency set.
Dependencies are locked in `Cargo.lock`, including for release builds.

Run the full local gate:

```console
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
cargo build --release --locked
cargo audit
```

Install the audit command with `cargo install cargo-audit --locked`.

Loopback tests need permission to bind local sockets. Sandboxed environments may
need to grant that capability.

## Test strategy

Unit tests cover:

- stable identity and peer-store persistence;
- key agreement, record encryption, and tamper detection;
- protocol record bounds and ordering;
- exclusion planning, path traversal rejection, and collision naming;
- clap command validity.

End-to-end tests bind an ephemeral loopback port and cover:

- plaintext compatibility transfer;
- secure transfer with a shared token;
- directory trees and empty directories;
- wrong-token failure before TOFU persistence.

CLI integration tests verify help and dry-run behavior through the compiled
binary.

When changing the wire format, add a focused protocol test and update
`docs/PROTOCOL.md`.

## Error handling

Library functions return `XferError`; the binary converts errors at its outer
boundary. Protocol errors should identify the violated invariant. Sensitive
values such as tokens and private keys must never appear in errors or logs.

The receiver sends a best-effort encrypted error frame after a session exists.
Partially received content remains in the staging directory and is removed by
the temporary-directory guard.

## Adding protocol features

Prefer a new typed frame or a versioned structured field. Keep these invariants:

- one monotonically ordered stream;
- bounded record allocation;
- authenticated headers and sequence numbers;
- no final-path visibility before verification;
- no path interpretation before validation;
- no automatic trust after an identity change.

A breaking wire change increments `protocol::VERSION` and the record version.

## CI and releases

Pull requests run format, Clippy, tests on all three desktop operating systems,
and cross-target `cargo check` using current stable Rust.

Tags matching `v*` build raw release binaries and SHA-256 files for:

- Linux x86_64 and ARM64, GNU and musl;
- macOS x86_64 and Apple Silicon;
- Windows x86_64 and ARM64.

Release builds use `--locked`.

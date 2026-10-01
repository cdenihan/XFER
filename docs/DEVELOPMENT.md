# Development guide

## Project shape

XFER is a workspace with a core library, a feature-gated CLI, and a separate GPUI desktop package:

| Module | Responsibility |
| --- | --- |
| `cli` | clap command model and command dispatch |
| `control` | cooperative cancellation and shutdown of blocked transfer sockets |
| `config` | identity, permissions, and TOFU peer persistence |
| `crypto` | key derivation, fingerprints, SAS, and AEAD helpers |
| `discovery` | TTL-1 multicast receiver announcements and passive browsing |
| `filesystem` | source planning, exclusions, safe paths, destination naming |
| `net` | dual-stack listeners, address discovery, and connection setup |
| `protocol` | negotiation, typed messages, and framed record transport |
| `reporter` | presentation-neutral status, progress, and trust prompts |
| `transfer` | connection, trust handshake, sending, and receive orchestration |
| `receiver` | frame state machine, path registry, totals, and verification |
| `storage` | private staging, collision naming, publication, and overwrite rollback |
| `delta` | Bounded rolling block matching and transfer statistics |
| `sync` | Incremental file reconstruction, previews, and per-file publication |
| `reconcile` | Two-way inventories, baseline history, and conflict decisions |
| `workflow` | Frontend-neutral actions, recent preferences, rate calculations, and bounded transfer jobs |
| `secure_store` | Local private directories, atomic writes, and standard file locks |
| `desktop` package | GPUI views, native path selection, Unicode text input, discovery, and peer settings |

The receive state machine has four states: between entries, receiving a file,
verified, and failed. An invalid frame poisons it; only a verified machine can
produce the value that publishes the storage transaction. Staging owns incoming
files until publication, and drop order closes active files before deleting
staging, including on Windows. Tests can drive the state machine directly
without a socket. Network integration tests live in `src/transfer/tests.rs`.

The CLI and desktop call the same `transfer` APIs. Network and filesystem behavior
must not be reimplemented in a presentation layer.

## Toolchain

`rust-toolchain.toml` tracks the current stable Rust toolchain. The crate metadata
records Rust 1.89 as the minimum accepted by the current dependency set.
Dependencies are locked in `Cargo.lock`, including for release builds.

Run the full local gate:

```console
cargo fmt --all -- --check
cargo clippy --locked -p xfer --all-targets --all-features -- -D warnings
cargo clippy --locked -p xfer-desktop --all-targets -- -D warnings
cargo test --locked -p xfer --all-targets
cargo test --locked -p xfer-desktop --features test-support
cargo build --release --locked -p xfer
cargo build --release --locked -p xfer-desktop
cargo audit
```

Install the audit command with `cargo install cargo-audit --locked`.

Loopback tests need permission to bind local sockets. Sandboxed environments may
need to grant that capability.

## Test strategy

Unit tests cover:

- stable identity and peer-store persistence;
- key agreement, record encryption, token separation, and tamper detection;
- protocol record bounds, flags, sequence ordering, and negotiation rejection;
- discovery validation, version filtering, address selection, and name limits;
- exclusions, path traversal, portability, symlink escape, and collision naming;
- GPUI workflow views, preview invalidation, Unicode/IME input, and constrained layouts;
- rolling block reuse after insertions/deletions and literal boundaries;
- clap command validity and value bounds.

End-to-end tests bind an ephemeral loopback port and cover:

- plaintext compatibility transfer;
- secure transfer with a shared token;
- zero-byte files and collision-safe destinations;
- directory trees and empty directories;
- wrong-token failure before TOFU persistence;
- changed pinned-identity rejection and manual approval with concurrent store updates;
- cancellation of waiting listeners and blocked reads;
- malformed totals, path aliases, corrupt data, and interrupted staging cleanup;
- a complete transfer between two compiled CLI processes;
- unchanged sync, shifted block reuse, read-only previews, and two-way conflicts.

CLI integration tests verify human and JSON output, diagnostics, peer
management, completion generation, validation failures, and a real subprocess
transfer. Installer tests build local release fixtures, exercise platform
selection and checksum verification, and prove that failed upgrades preserve an
existing installation. CI runs the POSIX installer tests on Linux and macOS and
the PowerShell tests on Windows.

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

## Dependencies and desktop build setup

The default workspace member is the CLI package. `cargo build -p xfer` never
compiles GPUI; `xfer-desktop` depends on the core with default features disabled,
so it does not pull in Clap. GPUI is pinned to 0.2.2 with font support, Linux
X11/Wayland, and Windows manifest features enabled explicitly. Its large
transitive dependency tree is isolated from CLI artifacts. Crypto, JSON, Unicode
normalization, path traversal, globbing, and safe platform support remain proven
library dependencies. Application code forbids unsafe Rust.

The desktop records Rust 1.98 as its supported minimum and is verified on 1.98.1.
On macOS, install Xcode and its optional Metal Toolchain. If the Metal compiler
is unavailable, local builds can use `--features runtime-shaders` on the desktop
package; shaders then compile through Metal at app startup. Release CI uses
precompiled shaders. On Linux, build with Clang, CMake, pkg-config and development
packages for ALSA, Fontconfig, FreeType, Wayland, xkbcommon (including xkbcommon-x11), OpenSSL, and XCB.
On Windows, use the MSVC toolchain, Visual Studio C++ build tools, and Windows SDK.

All application storage, updater, installer, and release behavior is local to
XFER. `THIRD_PARTY_NOTICES.md` records the MIT source of adapted infrastructure
and the Apache license of the GPUI input example. No shared toolkit Actions,
Cargo Git dependency, or private dependency token is required.

`build_plan_controlled` is the cancellable planning API. Existing planning
functions remain wrappers for callers without a cancellation handle. GUI jobs
carry operation identifiers, bound reliable events to 64 entries, and coalesce
progress outside the queue. Dropping a job cancels its socket and disconnects
pending events; trust waits observe cancellation. Source traversal checks the
control between entries, while DNS resolution and Git subprocesses remain
blocking only on worker threads. Discovery updates are bounded; desktop peer
lists are capped at 256 and logs at 256 entries. Virtual lists avoid rendering
whole inventories. Progress redraws run at most 10 Hz; idle polls do not redraw.

Measure release performance with `python3 scripts/benchmark.py target/release/xfer
--output /tmp/xfer-performance.json`. The harness uses isolated insecure loopback
sessions to compare payload transport and filesystem work, not authenticated
crypto throughput. It reports wall time, child CPU time, process startup, binary
size, and the cumulative peak child RSS (bytes on macOS, KiB on Linux). Resource
accounting requires Unix. Baseline results and validation limitations are in
`docs/VALIDATION.md`.

## CI and releases

Pull requests run format, Clippy, tests on all three desktop operating systems,
and cross-target `cargo check` using current stable Rust. Branch pushes do not
duplicate those runs; pushes to `main` validate the merged result. Superseded
runs for the same pull request or ref are cancelled.

Every push to `main` creates a release. The workflow generates a UTC version in
the form `YYYY.MM.DD.<daily-release-number>` and a matching
`vYYYY.MM.DD.<daily-release-number>` Git tag. It inspects the existing tags for
that UTC date and increments the highest suffix, so releases made on the same
day are numbered `.1`, `.2`, `.3`, and so on. The workflow reserves the tag
atomically before building to avoid duplicate numbers from concurrent pushes,
and removes its unused reservation if the release fails.

The local prepare workflow updates three version sources and creates a
`github-actions[bot]` commit on `main` before building:

- `VERSION` keeps the exact public form, such as `2026.07.16.7`;
- `Cargo.toml` uses the SemVer-compatible equivalent `2026.7.16-7`;
- `Cargo.lock` records the same Cargo package version.

Cargo requires exactly three non-zero-padded numeric core components, so the
public date form cannot be used literally in the package `version` field. The
CLI reads `VERSION` through `build.rs`, preserving the exact public form for
`xfer --version`, `xfer doctor`, transfer version checks, tags, and release
titles. The push-triggered workflow stops after creating the bot commit and
reserved tag. It then dispatches a separate `Release` workflow at that tag, so
the build run itself, every platform checkout, and the GitHub release are all
attached to the bot-authored commit rather than the triggering user commit.
Pushes made with the workflow's `GITHUB_TOKEN` do not recursively start the
push-triggered workflow.

Each release builds raw binaries and SHA-256 files for:

- Linux x86_64 and ARM64, GNU and musl;
- macOS x86_64 and Apple Silicon;
- Windows x86_64 and ARM64.

Release builds use `--locked`. The local publish workflow renders XFER-branded
`install.sh` and `install.ps1`, publishes checksums for both scripts, and adds a
`VERSION` asset used to make already-current update checks a no-op.

Desktop CI compiles and tests natively on Linux, macOS, and Windows. Linux musl
and other cross-target CLI checks remain separate. Desktop release builds cover
x86_64 and ARM64 on each platform, with Linux GNU runners. `scripts/package-desktop.py`
creates app archives and checksums using the Python standard library. The CLI
installer tests run against disposable local release fixtures and preserve
rollback behavior. Desktop signing, notarization, and automatic updates are deferred.

## Desktop visual assets

The original exchange-arrow icon lives in `desktop/assets`: SVG source, PNG,
macOS ICNS, and Windows ICO. Regenerate these with `python3 scripts/generate-icons.py`;
no graphics or Cargo dependency is required. The UI embeds the PNG. The Windows
build script embeds the ICO using the Windows SDK resource compiler. Linux installs
the PNG into the user icon theme and registers an app-id-matching desktop launcher.
macOS packages seal the complete bundle with an ad hoc signature for resource
integrity; Developer ID signing and notarization remain deferred.

A desktop receiver keeps one listener open across sessions. Cancellation drains the
retiring worker and waits for its exit before allowing a retry, avoiding port races.
Preview approval uses the job’s captured input revision, so later edits cannot
authorize an unreviewed destination.

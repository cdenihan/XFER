# Development

## Toolchain

Use Zig **0.17.0**. The migration began with the installed `zig init` command;
its package fingerprint is retained. There are no fetched application
packages, C bindings, Rust components, or external networking/crypto libraries.
Use the [language reference](https://ziglang.org/documentation/0.17.0/) and the
standard-library source shipped with that exact compiler version.

```sh
zig fmt --check build.zig src
zig build test
zig build -Doptimize=ReleaseSafe
python3 tests/integration.py zig-out/bin/xfer
python3 tests/desktop.py zig-out/bin/xfer
```

If a sandbox disallows the default compiler cache, point
`ZIG_GLOBAL_CACHE_DIR` at a writable directory. Network integration requires
permission to bind local TCP/UDP ports and uses isolated temporary directories.

## Architecture

| Module | Responsibility |
| --- | --- |
| `main.zig` | Process entry, errors and exit status |
| `cli.zig` | Strict arguments, launch routing, terminal menu, listener lifecycle |
| `desktop.zig` | Loopback capability API, selection staging, jobs and browser consent |
| `ui/` | Embedded browser interface, no bundler or runtime packages |
| `reporter.zig` | JSON/human events and bounded terminal input |
| `discovery.zig` | Nonce-bound UDP queries and source-address replies |
| `manifest.zig` | Snapshot planning, streaming hashes, inventory validation |
| `paths.zig` | Portable names and handle-relative no-follow traversal |
| `net_io.zig` | Cancellable Windows network deadlines; native batching elsewhere |
| `wire.zig` | Committed handshake, key derivation, encrypted records |
| `transfer.zig` | Consent, streaming, staging and publication |
| `root.zig` | Reusable library surface and test discovery |

All I/O receives an explicit `std.Io` instance. Application entry uses
`std.process.Init`; tests use `std.testing.io`. The long-running receiver
allocates a fresh arena for each session and releases it after success or
failure. Discovery is a cancellable `Io.Group` task whose socket stays alive
until the task finishes. File content memory is bounded independently of item
size; metadata has finite caps and is released with the session arena.

Preparation opens the user-selected root's canonical parent, skips symlinks,
walks ordinary files/directories and hashes each file. The offer is sorted in
byte order and checked again by the receiver. Every non-root entry has an
already declared directory parent. File access opens each path component
separately without following links. A source that changes while streaming
cannot match the preapproved digest and is never published.

The connection API in Zig 0.17's `Io.Threaded` backend panics when
`ConnectOptions.timeout` is set. `transfer.connect` instead races a normal
cancellable connection against a 10-second `Io.Select` timer. It cancels the
loser and closes any stream returned by a losing connection. Windows also rejects network operations in `Io.Batch.awaitConcurrent` (used by
`Io.operateTimeout`). `net_io.zig` races a normal cancellable network operation
against its absolute deadline on Windows and joins canceled tasks before their
buffers expire. macOS/Linux retain the native `Io.operateTimeout` path. UDP
discovery uses the same adapter. These paths are covered by native network CI.

The browser control listener uses a random loopback port and a 256-bit launch
capability carried initially in the URL fragment, then in same-origin
sessionStorage. API requests send the capability as a bearer header. Exact
Host and Origin checks prevent DNS rebinding and cross-origin control; no CORS
access is enabled. Embedded static assets need no capability. A restrictive
CSP allows only same-origin scripts/styles and rejects framing.

Browser-selected files stream to a private temporary tree. The UI never sends
an arbitrary source path to the server. One outgoing or incoming job runs at a
time; the HTTP API remains responsive while transfer/approval/upload I/O waits.
Application state is protected by `Io.Mutex`, and no response writes hold that
mutex. `Io.Group` owns cancellable listener, discovery, HTTP, and transfer jobs.
Shutdown cancels jobs before closing their sockets or freeing state. Ordinary
cancellation removes both selected-file storage and receive staging. Discovery
uses a fresh instance identifier so a window does not offer itself as a peer.

## Verification

Unit tests exercise portable-path checks, manifest invariants, AEAD tamper and
sequence binding, discovery correlation, CLI validation, and collision names.
Integration tests use two real XFER processes and TCP proxies to check:

- Encrypted file/folder transfers, Unicode, empty entries and skipped symlinks.
- Final acknowledgement and preserving an existing destination.
- Tampered ciphertext, dropped connections, wrong secrets and size limits.
- No published item or abandoned staging after ordinary session failures.
- UDP discovery and persistent-listener recovery.
- Offline preparation and rejecting automatic approval without a secret.
- Real pseudo-terminal code comparison, rejection and changing-source checks.
- Browser API upload/send/receive, UTF-8 and empty directories, approval and rejection.
- Browser capability/Host/Origin enforcement, traversal/size rejection and stalled-upload cancellation.

CI executes native tests on all three operating systems and cross-builds both
x86-64 and ARM64 for each. Cross-compilation alone does not validate operating
system behavior. The migration includes a real Linux ARM64 to macOS ARM64 Wi-Fi benchmark
with independent hash verification; see [the report](../benchmarks/README.md).
Platform firewall prompts, unusual destination filesystems, and native Windows
execution still require the corresponding environments. Browser appearance and
actual file-picker/code-confirmation controls were also checked locally.

## Release

`VERSION` supplies the executable version through generated build options.
Keep the package's semantic `.version` in `build.zig.zon` in sync.

```sh
python3 scripts/package.py
```

This builds six `ReleaseSafe` binaries and writes archives and SHA-256 sidecars
to `dist/`. Linux uses musl. Archives include the executable, README, security
notes and VERSION. Windows archives are ZIP; macOS/Linux archives are tar.gz.
The script requires only Python's standard library and Zig.

Pushing a tag `v<VERSION>` starts native tests and cross-builds before publishing
release archives. Release jobs reject a tag that disagrees with VERSION.
The rewritten release flow uses no private Rust toolkit or dependency token.

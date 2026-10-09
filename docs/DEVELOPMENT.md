# Development

## Toolchain

Use Zig **0.17.0**. The migration began with the installed `zig init` command;
its package fingerprint is retained. The native engine has no C bindings, Rust components, or external
networking/crypto libraries. The frontend uses Bun **1.4.2** and Vite+
**1.1.0**, with React, TypeScript, CSS, TanStack Router and Query pinned in `web/bun.lock`.
Use the [language reference](https://ziglang.org/documentation/0.17.0/) and the
standard-library source shipped with that exact compiler version.

```sh
zig fmt --check build.zig src
(cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build)
zig build test
python3 tests/build_cache.py
zig build -Doptimize=ReleaseSafe
python3 tests/integration.py zig-out/bin/xfer
python3 tests/desktop.py zig-out/bin/xfer
python3 tests/standalone.py zig-out/bin/xfer
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
| `web/src/` | React views, TanStack Router navigation, Query selectors/actions, CSS tokens and responsive layouts |
| `web/scripts/embed.ts` | Packs deterministic gzip assets into a binary blob and a small Zig index |
| `ui_http.zig` | Encoding negotiation, conditional GET, immutable asset caching and bounded identity fallback |
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

Files smaller than 1 MiB retain synchronous streaming with a 64 KiB buffer.
Larger files use two 1 MiB buffers per endpoint: the sender overlaps the next
read/hash with encryption/transmission, and the receiver overlaps the previous
hash/write with reception/decryption. Only one task touches a file hash at a
time. Futures are canceled and joined before their borrowed buffers, hash state
or file handles expire; `Io.async` can execute synchronously when concurrency
is unavailable. All hashes, consent checks, syncs and publication rules remain.
The 64 KiB wire limit is unchanged, so previous Zig peers still interoperate.
Record encryption and decryption use exact-overlap channel buffers. The
standard-library crypto aliasing behavior is covered by boundary tests and the
buffers are wiped on channel teardown. Planning reuses its already validated
parent directory handle while retaining no-follow and opened-file kind checks.
See [performance measurements](../benchmarks/PERFORMANCE.md).

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
Shutdown cancels jobs before closing their sockets or freeing state. Explicit task cancellation wakes stalled reads on every platform; Windows
socket shutdown alone does not reliably interrupt pending reads. Cleanup runs
with cancellation blocked, removing selected-file storage and receive staging. Discovery
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
Native CI also verifies Windows CLI transfers and the browser control API.
Platform firewall prompts, default-browser launch behavior on Windows/Linux,
and unusual destination filesystems still need manual checks on those hosts.
Browser appearance and actual file-picker/code-confirmation controls were
checked locally on macOS.

## Release

`VERSION` supplies the executable version through generated build options.
Keep the package's semantic `.version` in `build.zig.zon` in sync.

```sh
python3 scripts/package.py
```

This builds six `ReleaseSafe` binaries and writes archives and SHA-256 sidecars
to `dist/`. Linux uses musl. Archives include the executable, README, security
notes and VERSION. Windows archives are ZIP; macOS/Linux archives are tar.gz.
The script requires Python's standard library, Zig and Bun. `zig build`
automatically installs the frozen frontend lockfile, runs Vite+ using Bun
(Vite+'s build API), and embeds its output. The browser assets live in
the executable; neither assets nor executable helpers are extracted at launch.

Pushing a tag `v<VERSION>` starts native tests and cross-builds before publishing
release archives. Release jobs reject a tag that disagrees with VERSION.
The rewritten release flow uses no private Rust toolkit or dependency token.

## UI iteration

Start the complete development app with one command:

```sh
cd web
bun install --frozen-lockfile
bun run dev
```

Bun builds and starts Zig, starts Vite+ with the authenticated loopback proxy,
and prints the frontend URL with the launch capability. Ctrl+C stops both
processes. Vite+ hot reloads the React UI; changes to Zig require a restart.
Bun owns development tooling; the Zig process owns the API and native transfer
engine. Production embeds the Vite+ build in the executable and needs no Bun
installation on the recipient's machine.

The Share (`/`) and Receive (`/receive`) views use TanStack Router. Selection,
destination, upload actions and TanStack Query's live state belong to a shared
session, so navigation preserves the queue and active work. Reload restores
backend state and authentication; browser file selections must be chosen again.
The root shell owns consent dialogs, progress and shutdown on both routes.
Mutations are never retried automatically. New requests reset code confirmation;
rejected staging creation never cancels an incoming transfer.

For an independently launched backend:

Build `zig build -Doptimize=ReleaseSafe` and launch `zig-out/bin/xfer --no-open`.
The URL contains a fresh capability in its fragment. In another terminal:

```sh
cd web
XFER_DEV_URL=http://127.0.0.1:PORT bun run dev:web
```

Open `http://127.0.0.1:5173/#CAPABILITY` using the port/capability printed by
XFER. The development-only proxy rewrites Host/Origin to that exact loopback
host while preserving bearer authorization. It never enables CORS on the
production host. Restarting XFER creates a new capability; update the dev proxy
port and fragment. On Windows set `$env:XFER_DEV_URL` before `bun run dev`.

`bun run build` inside `web/` produces `web/dist/` and a compressed asset pack
in `web/.generated/ui/` (`assets.bin`, `assets.zig`, `stats.json`). The native
build produces an isolated pack in the Zig cache. It explicitly tracks every
frontend source/public file, package lockfile and build script; directory
creation/deletion invalidates the configuration too. The same target-independent
pack is reused for all six native targets. Unchanged builds skip Bun, dependency
installation and Vite+ completely. Only generated metadata is Zig source;
`@embedFile("assets.bin")` embeds the compressed payload as read-only bytes.

Browsers accepting gzip get those bytes directly, with no runtime compression,
asset extraction or helper processes. Identity-only clients use a bounded,
request-local decompression fallback. Hashed assets get immutable caching;
HTML/public assets revalidate with content ETags. Host/Origin checks and CSP
still apply; control API responses remain uncached. See
[frontend architecture and measurements](FRONTEND.md) for the research and decisions.

A single poll owner runs every 2 seconds idle, 300 ms during transfers, and
1 second after connection failures, including when the tab is in the background.
Other components subscribe to selected state fields without adding timers.
File totals are computed only when the selection changes. Queue rendering is
paginated in batches of 100; filtering scans names only when its input or the
selection changes. Browser directory traversal uses one accumulator, avoiding
repeated copies and large argument spreads.

Vite+ tests cover staging ownership, upload failures, path encoding and complete
directory enumeration. The Chromium suite exercises the actual React controls,
route navigation and reload, reconnects, stale approvals, explicit consent,
encrypted delivery, mobile overflow and quit:

```sh
cd web
bunx --no-install playwright install chromium
bun scripts/browser.ts ../zig-out/bin/xfer
```

Set `XFER_CHROMIUM` to an existing Chromium executable to use it instead. The
standalone test copies only `xfer` into an isolated directory, removes runtimes
from PATH and verifies that UI operation creates no asset/helper files. Native
CI runs transfer/API/isolation tests on Linux, macOS and Windows; Linux also
runs the real browser test. Release packages are built once in CI and published
from those verified artifacts.

## Optional remote transport

See [TAILCAT.md](TAILCAT.md) for helper installation, source provenance, real
helper integration tests and paired backend benchmarks. `src/tailcat.zig` owns
bounded startup parsing and subprocess arguments; the desktop engine owns
helper cancellation and exposes only the native transfer port.

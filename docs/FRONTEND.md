# Frontend and single-executable architecture

XFER ships one native executable **per OS and architecture**, plus optional
README/security documentation in its release archive. The executable contains
the Zig engine, local HTTP server and every production HTML/JavaScript/CSS/icon
asset. A modern browser is required. Bun and Vite+ are development/build tools;
neither is required on the receiving machine.

## Research and decision

Zig's official [build-system guide](https://ziglang.org/learn/build-system/)
describes generated assets consumed by `@embedFile`. Vite's
[production build](https://vite.dev/guide/build) outputs a static application
bundle. This is a natural fit: build the application once on the build host,
then embed the same target-independent assets into each native binary.

| Approach | Result for XFER |
| --- | --- |
| Raw assets embedded with `@embedFile` | Simple and native, but includes the full uncompressed UI payload. |
| Precompressed asset pack embedded with `@embedFile` | Selected: smaller payload, direct gzip serving, native executable, no runtime extraction. |
| Generating a Zig byte literal for every asset byte | Previous implementation: produces a large parser/compiler input with no runtime advantage. |
| Bun `--compile` | Bundles the Bun runtime with the program, according to [Bun's documentation](https://bun.sh/docs/bundler/executables). Useful for JavaScript servers; unnecessary for XFER's Zig-owned API and transfer engine. |
| External frontend directory or extracted assets | Adds runtime files and installation/cleanup requirements; does not satisfy the single-executable requirement. |

This is the best fit for XFER's constraints, rather than a universal claim about
all desktop applications. The browser runs the frontend JavaScript; Zig runs
native discovery, encryption, hashing, staging and file streaming. No Bun
runtime, native addon, child web server or asset extraction is used in production.

Remote sharing additionally uses an optional installed Tailcat process; see
[remote transport and packaging](TAILCAT.md). This does not change how the UI
is embedded or require Bun in production.

## Asset pack

`web/scripts/build.ts` installs the frozen lockfile, builds with Vite+ into an
isolated Zig-cache output directory, then calls `web/scripts/embed.ts`.
The packer sorts paths and emits:

- `assets.bin`: concatenated gzip payloads (or identity for tiny/incompressible assets).
- `assets.zig`: a small typed index with paths, offsets, MIME types, sizes and weak content ETags.
- `stats.json`: raw/stored byte counts per asset for measurement.

Gzip runs once at build time with deterministic output and maximum compression.
An asset uses gzip only when it saves more than 32 bytes. Raw and compressed
copies are **not** both stored. `@embedFile("assets.bin")` compiles the pack into
the executable's read-only data. Requests only borrow slices from it.

`ui_http.zig` follows [Accept-Encoding negotiation](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Accept-Encoding),
including quality values, explicit gzip exclusion and identity fallback.
Browsers get precompressed bytes without allocating/decompressing a copy.
Clients requesting identity get request-local, size-bounded decompression.
If neither supported encoding is acceptable, the server returns 406.

Following [HTTP cache controls](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Cache-Control),
hashed `/assets/` files use a one-year immutable cache policy; HTML/public files
use `no-cache` and ETags so clients revalidate. `Vary: Accept-Encoding` separates
representations. Conditional GET and HEAD omit the body. The loopback Host/Origin
checks, content types, no-sniff, referrer policy, CSP and frame restrictions remain.
Capability-bearing control API responses remain `no-store`.

## Build and render costs

The Zig build graph hashes frontend inputs and tracks directory additions/removals.
Its generated directory is a declared build output. Unchanged frontend builds
are cached, including across cross-compilation targets; changes automatically
produce a fresh index/blob pair. No timestamp-only cache or manually refreshed
artifact is required.

The interface uses TanStack Router and Query. One observer owns polling; views
subscribe to stable selectors using TanStack's documented
[render optimizations](https://tanstack.com/query/latest/docs/framework/react/guides/render-optimizations).
Idle polling runs every 2 seconds (previously 600 ms), while active transfers
poll every 300 ms. File totals and filtered lists are memoized. The queue starts
with 100 rendered rows and can incrementally show more. Directory traversal
appends to one array instead of copying/spreading subtrees. No file contents are
read into frontend memory just to enumerate the queue.

The frontend uses local SVG components and ordinary CSS. Removing unused
Tailwind and icon-library dependencies reduced Vite's transformed module count
from 2,039 to 158. Mutations are never retried automatically; an aborted upload
joins through its owner, then cancels only staging it successfully created.

## Earlier local measurements (2026-10-09)

These precede the subsequent UI redesign and Tailcat integration. They are a macOS ARM64 `ReleaseSafe` development snapshot, not a cross-platform
benchmark or a promise about LAN transfer throughput. The UI changed as well as
the embedding format, so binary size is a comparison of complete builds.

| Measurement | Previous implementation | Optimized implementation |
| --- | ---: | ---: |
| Executable bytes | 1,636,152 | 1,426,456 |
| New UI payload before/after packing | 377,236 raw bytes | 117,101 stored bytes |
| Frontend modules transformed | 2,039 | 158 |
| Idle polling interval | 600 ms | 2,000 ms |
| Unchanged native build | Frontend rebuilt each invocation | Cached; 0.12 seconds in this snapshot |

The packed UI is about 69% smaller than its raw representation; the complete
native executable in this snapshot is about 13% smaller. Idle polling frequency
is reduced by 70%. Use the packer's current `stats.json`, `zig build --summary all`,
and binary file size to refresh measurements after changes.

Verification includes frontend types/lint/unit tests, deterministic pack
roundtrips, Zig negotiation tests, HTTP representation/cache/HEAD tests,
real browser consent and encrypted delivery, native network integration and
operation of a copied standalone executable with Bun/Node removed from PATH.

The subsequent redesigned UI with the optional Tailcat adapter built to
1,445,656 executable bytes on macOS ARM64. Its 385,953-byte frontend packs to
119,267 bytes (159 transformed modules). The optional source-built Tailcat
helper is separately installed and measures 19,232,258 bytes in this local
snapshot; keeping it optional avoids adding its Go networking implementation
to every Nearby installation.

# Transfer performance investigation

Measured on 2026-10-05 with Zig 0.17.0, ReleaseSafe, on an Apple ARM64 Mac.
Baseline is commit `52223e4e84189bebd74b663dafa42e6218f7368f`; the refreshed
after binaries include review fixes at `16eaf55683972d64c3461f309e220a0831f838fd`.

## Cause and change

The previous real LAN benchmark showed a 5.9% longer large-file median and
higher sender CPU time on a Raspberry Pi. That measurement also had uncontrolled
Wi-Fi variation; it does not isolate a single cause. Code inspection found
serialized disk read → SHA-256 → encryption → socket write on the sender, and
socket read → decryption → SHA-256 → disk write on the receiver. Sources are
also fully hashed before approval, so the sender reads/hashes them twice.
The generic Linux ARM64 target enables NEON but not SHA2, so Zig's standard
library selects software SHA-256. This contributes CPU cost; the source
verification pass is retained to reject content changes after approval.

Larger disk buffers alone did not materially improve local timings. The final
change pipelines both endpoints for files at least 1 MiB. Two 1 MiB buffers
allow disk/hash work to overlap encryption/network work. Each file still uses
64 KiB encrypted records, keeping the protocol compatible with existing Zig
peers. Small files keep synchronous streaming and allocate no bulk buffers.
Additional content memory is bounded at 2 MiB per endpoint, with at most one
worker outstanding per endpoint. Buffers are wiped after workers are joined.

A second pass removes the separate outgoing plaintext buffer and its full
64 KiB wipe on every record. ChaCha20-Poly1305 now encrypts/decrypts in place
in the existing channel buffers, which are wiped on scope exit. This also
removes a 64 KiB buffer from each channel. Tests compare in-place and disjoint
crypto at cipher-block and maximum-record boundaries.

Planning reuses the safely opened parent directory instead of retraversing
ancestors and repeating handle stat operations for every file. It retains
no-follow opens, opened-handle type checks, preapproval hashing and final size
checks. No third-party dependencies are introduced.

Both endpoints still verify every file hash, require consent, enforce deadlines,
sync completed files, and publish only the fully verified selection. A single
CPU or an I/O backend without available asynchronous capacity can execute the
work synchronously and may see little improvement. Slow Wi-Fi can still dominate.

## Results

Median of five alternating measured trials after one warm-up for each binary
and dataset. Lower wall time is better; the last column is time saved.

| Build | Dataset | Before | After | Time saved |
| --- | --- | ---: | ---: | ---: |
| Native Apple ARM64 (SHA instructions) | 128 MiB random file | 0.3243 s | 0.2764 s | 14.8% |
| Native Apple ARM64 (SHA instructions) | 1,000 × 4 KiB files | 0.1533 s | 0.1373 s | 10.4% |
| ARM64 with SHA instructions disabled | 128 MiB random file | 0.7935 s | 0.6170 s | 22.3% |
| ARM64 with SHA instructions disabled | 1,000 × 4 KiB files | 0.1684 s | 0.1522 s | 9.6% |

The final measurements show about 15–22% lower large-file time and 10%
lower small-file time against the original baseline.
These are loopback measurements with warm filesystem caches, including planning,
handshake, encryption, file sync and delivery acknowledgement. Independent
Python SHA-256 checks passed after every transfer outside the timed window.
Raw results include executable hashes, platform and all trials:
[Native](local-performance.json), [software SHA-256](local-software-sha256.json).

The software-SHA build uses `-Dtarget=aarch64-macos -Dcpu=generic-sha2` on the
same physical Mac to exercise the standard library's portable SHA-256 path.
It is a portable-hashing experiment on macOS. The
[Rust-versus-Zig LAN comparison](README.md) remains historical: the new
Pi-to-Mac run failed to establish a usable session, and tiny-file probes
failed for both Rust and Zig. Both existing Mac interfaces were checked.
[Attempt metadata](lan-attempt-2026-10-05.json) records the limitation. No
network settings were changed. Browser selection still requires a local
staging copy before transfer.

## Earlier second-pass comparison

[second-pass.json](second-pass.json) records the earlier comparison of the pipeline with the
in-place crypto/parent-handle changes on the native Mac. The 1,000-file median
fell from 0.1628 s to 0.1441 s (11.5% less time); the large-file median was
0.2736 s versus 0.2764 s, a 1.1% difference that does not establish a meaningful
large-file change. Measurements are workload- and host-specific, not a speed
guarantee. The main large-file improvement remains the pipeline.

## Reproduce

Keep a baseline checkout and the changed checkout; build both with identical
Zig/optimization/CPU settings into separate prefixes, then run:

```sh
python3 scripts/benchmark_local.py /path/to/before/xfer /path/to/after/xfer \
  --output /tmp/xfer-performance.json
```

Use `zig build -Doptimize=ReleaseSafe --prefix /tmp/xfer-before` in the baseline
checkout and a different prefix for the changed build. For the portable-hashing
experiment, add `-Dtarget=aarch64-macos -Dcpu=generic-sha2` to both builds.
The harness generates fixtures in a temporary directory, installs nothing,
uses only Python's standard library, and needs loopback TCP permission.
Do not run heavy builds concurrently with the timing loop.

## Verification

Core tests, real TCP/UDP transfer tests and browser workflow tests pass on the
Mac. Added integration fixtures cover 64 KiB and 1 MiB boundaries, multiple
bulk buffers and partial final blocks. A same-size modification in a large
source file after approval planning is rejected without publishing it.
Both old-to-new and new-to-old Zig transfers pass independent hash checks.
The same real transfer and browser API checks also pass natively on the
Raspberry Pi (Linux ARM64), including broadcast discovery when the receiver
uses a specific IPv4 bind address. Six release targets cross-compile. Native Windows
validation runs in CI; cross-compilation does not replace it. The LAN harness
does not establish whether the route uses Wi-Fi or Ethernet.

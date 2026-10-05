# Transfer performance investigation

Measured on 2026-10-05 with Zig 0.17.0, ReleaseSafe, on an Apple ARM64 Mac.
Baseline is commit `52223e4e84189bebd74b663dafa42e6218f7368f`.

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
| Native Apple ARM64 (SHA instructions) | 128 MiB random file | 0.3301 s | 0.2790 s | 15.5% |
| Native Apple ARM64 (SHA instructions) | 1,000 × 4 KiB files | 0.1528 s | 0.1372 s | 10.2% |
| ARM64 with SHA instructions disabled | 128 MiB random file | 0.7817 s | 0.6158 s | 21.2% |
| ARM64 with SHA instructions disabled | 1,000 × 4 KiB files | 0.1665 s | 0.1520 s | 8.7% |

The final measurements show about 15–21% lower large-file time and 9–10%
lower small-file time against the original baseline.
These are loopback measurements with warm filesystem caches, including planning,
handshake, encryption, file sync and delivery acknowledgement. Independent
Python SHA-256 checks passed after every transfer outside the timed window.
Raw results include executable hashes, platform and all trials:
[Native](local-performance.json), [software SHA-256](local-software-sha256.json).

The software-SHA build uses `-Dtarget=aarch64-macos -Dcpu=generic-sha2` on the
same physical Mac to exercise the standard library's portable SHA-256 path.
It is not a Linux or Raspberry Pi measurement. No new Rust comparison or physical
LAN benchmark was run; these numbers cannot be substituted for the older LAN
results. Browser selection still requires a local staging copy before transfer.

## Second-pass comparison

[second-pass.json](second-pass.json) compares the prior pipeline with the final
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
Native Linux/Windows behavior and real Wi-Fi performance require their own
runs; cross-compilation does not replace those checks.

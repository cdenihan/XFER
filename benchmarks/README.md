# Rust versus Zig LAN benchmark

Measured against **`cdenihan@192.168.86.150`** on 2026-10-04.
The sender was **Raspberry Pi 4 Model B Rev 1.5**, running `Linux-7.0.0-1020-raspi-aarch64-with-glibc2.43`.
The receiver was `macOS-27.0.1-arm64-arm-64bit`.
The Linux ARM64 host sent directly over Wi-Fi/TCP to the ARM64 Mac.
SSH controlled the run and copied fixtures beforehand; file-transfer
traffic did **not** pass through SSH.

The host firewall permits SSH but blocks incoming transfer ports. The run
therefore used Linux-to-macOS transfers without changing firewall rules.
Binaries and fixtures stayed in an isolated `/tmp/xfer-benchmark-*` directory,
which was removed after validation. No tools were installed on the host.

## Results

Median of **3 measured trials** per implementation/dataset after one
excluded warm-up. The order alternated between Rust-first and Zig-first.

| Dataset | Rust wall time | Zig wall time | Rust MiB/s | Zig MiB/s |
| --- | ---: | ---: | ---: | ---: |
| One 128 MiB random file | 21.036 s | 22.286 s | 6.085 | 5.743 |
| 1,000 files × 4 KiB | 5.091 s | 0.918 s | 0.767 | 4.255 |

Zig completed the small-file selection **5.55× faster**. Its large-file
median took **5.9% longer** in this run.
These are end-to-end LAN results, not an isolated compiler/language
comparison or a claim about every network. Wi-Fi and host activity were
not controlled. Large-file measured times ranged from 20.747
to 22.693 seconds; small-file times from 0.910
to 5.297 seconds across both implementations.

| Dataset | Rust sender CPU | Zig sender CPU | Rust receiver CPU | Zig receiver CPU |
| --- | ---: | ---: | ---: | ---: |
| One 128 MiB random file | 1.912 s | 5.633 s | 0.944 s | 1.538 s |
| 1,000 files × 4 KiB | 0.176 s | 0.337 s | 0.909 s | 0.260 s |

Zig hashes the source during planning and again while sending, encrypts every
record, and verifies files before publication. No checks were disabled for
performance. Encrypted record headers and bodies share one write to avoid
extra small network writes. OS-reported sender peak RSS is retained in the
raw results, including process startup; it is not an allocator benchmark.

## Versions and verification

- Rust: published release **`v2026.09.06.1`**, commit
  `6214e213f392d6281455a3d3f24988012c6ee596` (pre-migration main).
  The official macOS ARM64/Linux ARM64 musl release binaries were checked
  against their published SHA-256 sidecars. The local offline Rust rebuild
  was unavailable because its dependency cache was incomplete.
- Zig: **0.17.0**, `ReleaseSafe`, generic ARM64 targets, with the complete
  embedded browser interface and platform cancellation fixes present.
- Both implementations received into a fresh empty directory, used the
  same random shared secret, and ran with supported unattended approvals.
- Timing starts immediately before the remote sender process starts and
  ends after its successful delivery acknowledgement. Preparation, connection
  establishment, authentication, encryption, and file I/O are included.
  Receiver/SSH startup and fixture preparation are excluded.
- After **all 16 transfers**, including warm-ups, an independent
  Python SHA-256 inventory of published files matched the original generated
  selection. Independent verification is outside the timed window.
- This measures the CLI path. Browser selection adds a local temporary copy
  and human approval time; those are not included.

[results.json](results.json) records all trials, warm-ups, operating systems,
source/binary hashes, binary sizes, throughput, CPU usage, and verification.
The browser and CLI integration suites also passed natively on this Mac
and the physical Linux ARM64 host.

## Reproduce

Use Python 3 with only its standard library and prebuilt Rust/Zig binaries.
Create an isolated `/tmp/xfer-benchmark-*` directory on the Linux host and
copy its two executable binaries there as `rust` and `zig`. Then run:

```sh
python3 scripts/benchmark.py \
  --remote cdenihan@192.168.86.150 \
  --root /tmp/xfer-benchmark-<your-isolated-directory> \
  --rust /path/to/local/rust-xfer \
  --zig /path/to/local/zig-xfer \
  --output benchmarks/results.json
python3 scripts/benchmark_report.py
```

The sender must be able to reach the receiver’s LAN address. The harness
uses existing SSH/SCP and remote Python, installs no tools, and accepts
only isolated remote directories under `/tmp` named `xfer-benchmark-*`.
Remove that specific directory after the run; do not put user files in it.

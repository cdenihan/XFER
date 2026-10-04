# Rust versus Zig LAN benchmark

Measured on 2026-10-04 against **`cdenihan@192.168.86.150`**.
The remote Raspberry Pi 4 (ARM64, Cortex-A72, Ubuntu 26.04) sent directly over
Wi-Fi/TCP to this ARM64 macOS computer. SSH controlled the run and copied
fixtures beforehand; file-transfer traffic did **not** pass through SSH.

The host firewall permits SSH but blocks incoming transfer ports. The run
therefore used Linux-to-macOS transfers; no firewall rules, services, or user
files were changed. Both app binaries and fixtures stayed in an isolated
`/tmp/xfer-benchmark-*` directory, which was removed after validation.

## Results

Median of three measured trials per implementation/dataset, after one excluded
warm-up. The order alternated between Rust-first and Zig-first.

| Dataset | Rust wall time | Zig wall time | Rust MiB/s | Zig MiB/s |
| --- | ---: | ---: | ---: | ---: |
| One 128 MiB random file | 20.729 s | 22.281 s | 6.175 | 5.745 |
| 1,000 files × 4 KiB (3.906 MiB) | 5.172 s | 0.879 s | 0.755 | 4.442 |

Zig completed the small-file selection **5.88× faster**. Its large-file
median took **7.5% longer** in this run. These are end-to-end LAN results,
not an isolated compiler/language comparison or a claim about every network.
The large-file times varied from 20–24 seconds; Wi-Fi and concurrent host
activity were not controlled. The small-file Zig trials ranged from 0.868 to
1.310 seconds, with every run faster than the corresponding Rust runs.

| Dataset | Rust sender CPU | Zig sender CPU | Rust receiver CPU | Zig receiver CPU |
| --- | ---: | ---: | ---: | ---: |
| 128 MiB | 1.959 s | 5.231 s | 0.702 s | 1.261 s |
| 1,000 small files | 0.169 s | 0.323 s | 0.837 s | 0.251 s |

Zig used more sender CPU in both workloads. On the 128 MiB workload, the Zig
sender used about 2.7× the CPU time. The rewrite hashes the source during
planning and again while sending, encrypts every record, and verifies files
before publication; none of those checks were disabled for performance.
Encrypted record headers and bodies share one write to avoid extra small
network writes. OS-reported sender peak RSS is retained in the raw results,
including process startup; it is not a standalone allocator benchmark.

## Versions and verification

- Rust: published release **`v2026.09.06.1`**, commit
  `6214e213f392d6281455a3d3f24988012c6ee596` (the pre-migration main branch).
  The official macOS ARM64/Linux ARM64 musl release binaries were checked
  against their published SHA-256 sidecars. An offline rebuild was unavailable
  because the local Rust dependency cache was incomplete.
- Zig: **0.17.0**, `ReleaseSafe`, generic ARM64 platform targets, with the
  complete embedded browser interface present in both binaries.
- Both implementations received into a fresh empty directory, used an
  environment-provided random shared secret, and ran with their supported
  unattended approval options.
- Timing starts immediately before the remote sender process starts and ends
  when it exits successfully after delivery. It includes source preparation,
  connection establishment, authentication, encryption, file I/O, and the
  recipient acknowledgement. Receiver/SSH startup and fixture preparation are
  excluded.
- After **all 16 transfers**, including warm-ups, an independent Python SHA-256
  inventory of the published files matched the original generated selection.
  Hash verification is outside the timed window.
- This benchmarks the CLI transfer path. Browser selection adds a local
  temporary copy and human approval time; those are not part of these timings.

[results.json](results.json) records every trial, warm-ups, operating systems,
source and binary hashes, binary sizes, throughput, CPU and verification.
The UI assets received whitespace formatting after the measured build, and a
Windows-only cancellable deadline adapter was added after native Windows CI
exposed unsupported network batching. The measured macOS/Linux network path
still uses the original native timed operations. Measured binary/source hashes
and final source hashes are retained separately.
The browser/CLI integration suites also passed natively on both macOS ARM64
and this Linux ARM64 host.

## Reproduce

Use Python 3 with only its standard library and prebuilt Rust/Zig binaries.
The current harness controls an ARM64 Linux sender and a macOS/Linux receiver.
Use an isolated temporary directory on the remote host, copy its two executable
binaries there as `rust` and `zig`, and run:

```sh
python3 scripts/benchmark.py \
  --remote cdenihan@192.168.86.150 \
  --root /tmp/xfer-benchmark-<your-isolated-directory> \
  --rust /path/to/local/rust-xfer \
  --zig /path/to/local/zig-xfer \
  --output benchmarks/results.json
```

The remote sender must be able to connect to the receiver’s LAN address. The
harness requires existing SSH/SCP access and remote Python, installs no tools,
and accepts only remote directories under `/tmp` named `xfer-benchmark-*`.
Remove that specific directory after the run. Do not place user files in it.

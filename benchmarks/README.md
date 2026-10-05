# Rust versus Zig LAN benchmark

Latest update (2026-10-05): [current Zig loopback measurements](PERFORMANCE.md)
include the review fixes. The LAN numbers below remain the historical
2026-10-04 comparison; the latest Pi-to-Mac attempt could not establish a
usable session for either implementation on either existing Mac interface.
[Attempt metadata](lan-attempt-2026-10-05.json) records that limitation; no failed
transfer is reported as a performance result.

Measured against `cdenihan@192.168.86.150` at an unrecorded time (historical result).
Sender: **Raspberry Pi 4 Model B Rev 1.5**, `Linux-7.0.0-1020-raspi-aarch64-with-glibc2.43`.
Receiver: `macOS-27.0.1-arm64-arm-64bit`.
Direction: Linux ARM64 sender to macOS ARM64 receiver over direct LAN TCP.

SSH controls the run; file traffic uses direct TCP rather than SSH.
The harness does not identify whether the route is Wi-Fi or Ethernet.
Fixtures and binaries use an isolated remote `/tmp/xfer-benchmark-*` directory.
No software is installed by the harness.

## Results

Medians exclude one warm-up per binary and dataset. Execution order alternates.

| Dataset | Rust time | Zig time | Rust MiB/s | Zig MiB/s | Measured trials per binary |
| --- | ---: | ---: | ---: | ---: | ---: |
| One 128 MiB random file | 21.036 s | 22.286 s | 6.085 | 5.743 | 3 |
| 1,000 files × 4 KiB | 5.091 s | 0.918 s | 0.767 | 4.255 | 3 |

These end-to-end measurements include planning, connection, authentication,
encryption, file I/O and delivery acknowledgement. Receiver/SSH startup,
fixture creation and independent SHA-256 verification are excluded.
LAN conditions and host activity are uncontrolled; this is not a general
language comparison. Browser staging and human approval are not timed.

| Dataset | Rust sender CPU | Zig sender CPU | Rust receiver CPU | Zig receiver CPU |
| --- | ---: | ---: | ---: | ---: |
| One 128 MiB random file | 1.912 s | 5.633 s | 0.944 s | 1.538 s |
| 1,000 files × 4 KiB | 0.176 s | 0.337 s | 0.909 s | 0.260 s |

## Binary provenance

Versions below are queried from the actual binaries. SHA-256 hashes and sizes
are recorded for each endpoint in the raw JSON. Source revisions and build
settings are caller-supplied declarations, not inferred from a binary.

- Rust sender: `xfer 2026.09.06.1`; SHA-256 `2b8c81794e96cee1d37b7e29c4d3f08274c2708a6a9b6fcde11952e266025bfe`.
- Rust receiver: `version not recorded`; SHA-256 `2867d829032943b234a2942f34b619e8eee524b1eef96be3dd955655bd32259c`.
- Declared rust revision: `6214e213f392d6281455a3d3f24988012c6ee596`.
- Zig sender: `xfer 1.0.0 (Zig 0.17.0, protocol XFERZ017)`; SHA-256 `18f6360de05abdaedb48a8f712ceb739c7513dec2b0e37d4b94dd80ac2f47e56`.
- Zig receiver: `version not recorded`; SHA-256 `f85ef1dd0f55019493acccdc984abff5c442d9a5a6c73dd86c60ed9e3be8513c`.
- Declared zig revision: `not supplied`.
- Declared Zig build: `ReleaseSafe`.
- Zig workspace source fingerprint: `db8a66ac3f76a3effdffb888dc57c78252ea33b2e163d0f2a203f6597eb868a1` (the workspace, not proof of the prebuilt binary revision).

All **16 transfers**, including warm-ups, passed independent
SHA-256 inventory comparison before the result was recorded.

[results.json](results.json) contains the full measurements and metadata.

## Reproduce

Provide prebuilt binaries for the local receiver and remote sender. Create
an isolated `/tmp/xfer-benchmark-*` directory remotely and copy the remote
executables there as `rust` and `zig`. Use Python 3 and existing SSH/SCP:

```sh
python3 scripts/benchmark.py \
  --remote user@host \
  --root /tmp/xfer-benchmark-<isolated-run> \
  --rust /path/to/local/rust-xfer --zig /path/to/local/zig-xfer \
  --output benchmarks/results.json
python3 scripts/benchmark_report.py
```

Supply `--rust-commit`, `--zig-commit` and `--zig-build` when known. The
sender must reach the receiver LAN address. Remove the specific isolated
remote directory after verification; never use a directory of user files.

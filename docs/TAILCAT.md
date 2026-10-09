# Optional remote transport

The browser UI offers **Nearby** and **Across a distance**. Nearby uses XFER's
native TCP transport. Across a distance uses an installed
[Tailcat](https://github.com/tailscale/tailcat) 0.7+ helper to forward that same
protocol through an encrypted peer connection. Zig still owns file planning,
consent, encryption, streaming, hashes and atomic publication. Tailcat's SFTP
`cp` command is not used, because it would bypass XFER's transfer workflow.

XFER remains one native executable with its Vite+/React/TanStack UI embedded.
The remote option additionally requires Tailcat on both computers. Bun and Go
are build tools, not production runtime requirements. No helper is extracted
from XFER or installed automatically.

## macOS installation

```sh
brew install tailcat
python3 scripts/tailcat_setup.py
```

The official [Homebrew formula](https://formulae.brew.sh/formula/tailcat)
provides Apple Silicon macOS bottles even though Tailcat's v0.7.0 GitHub release
has no macOS download. The installed ARM64 bottle was verified locally at
v0.7.0, with no runtime dependencies. Bottle availability depends on macOS and
architecture; use the source fallback when a compatible bottle is unavailable.

With an existing Go 1.27.1+ toolchain:

```sh
python3 scripts/tailcat_setup.py --source
XFER_TAILCAT_BIN="$PWD/.tools/tailcat" zig-out/bin/xfer
```

The script downloads v0.7.0, verifies the source SHA-256 pinned to Homebrew's
[upstream formula](https://raw.githubusercontent.com/Homebrew/homebrew-core/master/Formula/t/tailcat.rb),
and compiles a trimmed, CGO-free helper. It writes a license and provenance
record next to the executable and refuses to overwrite an existing output.
This fallback was compiled and tested on macOS ARM64. On Windows the default
output is `.tools/tailcat.exe`. Otherwise XFER finds `tailcat` through PATH.
The version probe is bounded; an absent/incompatible helper disables remote
controls while Nearby remains available.

## Sharing remotely

1. On the receiver, open **Receive**, enable remote receiving and copy its invitation.
2. Send that private invitation to the intended sender through a trusted channel.
3. On the sender, choose **Across a distance**, paste the invitation and select files.
4. Compare the verification code on both computers and approve on both.
5. Stop remote receiving when finished. Re-enabling creates a fresh invitation.

Tailcat uses ephemeral keys, user-space WireGuard, NAT traversal and DERP
fallback; it does not require a Tailscale account, tailnet membership or root.
Normal operation can contact public infrastructure described by upstream.
Connectivity and performance depend on the actual network and relay path.

The invitation includes a secret capability and preserves case. XFER only
allows the helper to expose its native transfer port. The authenticated browser
control API stays on loopback. Sender forwarding binds to `127.0.0.1` with an
allocated port. Helper arguments are structured, never shell interpolated.
Invitations are returned only through the authenticated, no-store control API;
XFER does not print them to its logs. Quit, stop, cancellation and transfer
completion tear down the owned helper processes. Remote receiving currently
requires the native listener to bind to `0.0.0.0` or `127.0.0.1`.

## Reproducible measurements

```sh
python3 scripts/benchmark_backends.py zig-out/bin/xfer \
  --trials 5 --output benchmarks/backends-local.json
XFER_TAILCAT_BIN="$PWD/.tools/tailcat" \
  python3 tests/tailcat.py zig-out/bin/xfer
```

The benchmark alternates transports, discards one warmup per workload/transport,
and records five samples by default. Both use the same Zig protocol and consent
flow. Workloads are a random 32 MiB file and 500 random 4 KiB files plus an empty
directory. Every delivery gets an independent SHA-256 inventory check outside
the timed interval. Staging, connection/consent, transfer and complete workflow
times are recorded separately. Automated approval and polling are included;
human response time is not modeled.

Tests and benchmarks use Tailcat's private loopback DERP mode with public DERP
discovery disabled. Results describe same-host overhead, not distant-network
throughput. The actual data tunnel path is unclassified; do not label these
samples direct-WAN or public-relay measurements. A WAN comparison needs two
real endpoints, their network conditions and path evidence. Tailcat v0.7.0 does
not include the `perf` command described by its newer main-branch README.

## Recorded local results (2026-10-09)

Five measured samples per transport/workload, following warmup. All 24 total
deliveries (including warmups) passed independent hash verification. Median
values are rounded to milliseconds.

| Workload | Nearby transfer | Tailcat transfer | Nearby complete workflow | Tailcat complete workflow |
| --- | ---: | ---: | ---: | ---: |
| 32 MiB file | 108 ms | 366 ms | 175 ms | 433 ms |
| 500 × 4 KiB files | 111 ms | 110 ms | 295 ms | 296 ms |

Full samples, executable hash, timing scope and helper version are in
[`benchmarks/backends-local.json`](../benchmarks/backends-local.json). The small
workload approaches the API polling granularity, so its close timings do not
establish a meaningful transport winner. These results support using Nearby
for local transfers and offering Tailcat for reachability; they do not predict
WAN speeds.

# XFER

Nearby file sharing for Windows, macOS, and Linux, written in **Zig 0.17.0**.
Open XFER on two computers, choose a nearby receiver, compare the code, and send
a file or folder. No accounts, cloud, server deployment, or runtime packages.

XFER uses the same local network (Wi-Fi or Ethernet). Launching it opens a
browser sharing window; the same executable also provides a scriptable CLI. It does not speak Apple's AirDrop
protocol or establish Bluetooth/AWDL/Wi-Fi Direct connections.

## Build

Install [Zig 0.17.0](https://ziglang.org/download/) and run:

```sh
zig build -Doptimize=ReleaseSafe
```

The executable is `zig-out/bin/xfer` (`xfer.exe` on Windows). The application
uses only Zig's standard library. Language and I/O APIs follow the
[0.17.0 documentation](https://ziglang.org/documentation/0.17.0/).

## Share

Launch `xfer` (or double-click `xfer.exe` on Windows) on both computers.
Your default browser opens a local sharing window and XFER is ready to receive.

1. Drag files or a folder into the window, or use **Choose files / Choose folder**.
2. Select a nearby computer and click **Share files**.
3. Compare the full code on both windows, check that it matches, and approve.

Received items appear in Downloads/XFER. Progress, cancellation, incoming
requests, and the next transfer use the same window. Multiple selected files
arrive together in a `Shared items` folder. Dropped folders preserve empty
subdirectories; folder pickers may omit empty subdirectories depending on the
browser. A modern browser is the only UI requirement; everything is embedded
in the Zig executable. Closing its tab leaves XFER receiving until you use
**Quit XFER** or stop the process.

If discovery is filtered, expand **Connect using an address** and enter the
other computer's IP/hostname (optionally with a port). Configure the window:

```sh
xfer --name "Office PC" --output ./Received
xfer --no-open                 # Print its local URL for manual opening
```

The browser sends selected files to private local temporary storage before the
LAN transfer. This requires temporary disk space equal to the selection size,
and adds a local copy; temporary files are removed after sending or canceling.
The browser never grants XFER arbitrary read access to local paths. Use the
CLI to stream a selected source directly without this temporary copy:

```sh
# Receiving computer
xfer receive
# Sending computer
xfer send ./photos
```

Compare the entire displayed code (for example `81b2-08fa-c903`) through a
trusted channel and approve on both computers. Run `xfer menu` for the terminal
menu. File contents are sent only after both approvals. Receivers remain
available for the next transfer until you quit.

Default destination: `~/Downloads/XFER` (`%USERPROFILE%\Downloads\XFER` on
Windows). Choose another directory with `--output`:

```sh
xfer receive --output ./Received --name "Office PC"
xfer send ./report.pdf --to 192.168.1.42
```

Allow XFER through the private-network firewall on the receiving computer.
Both TCP and UDP use port **9000** by default. Guest Wi-Fi, client isolation,
VPNs, or broadcast filtering can prevent discovery; use `--to HOST` when the
computers are otherwise reachable. Nearby discovery sends one small IPv4
broadcast query and receivers reply directly. It never scans a subnet.

## Delivery guarantees

- X25519 ephemeral key exchange, HKDF-SHA-256, and ChaCha20-Poly1305 records.
- Key commitments before disclosure prevent choosing keys after seeing a peer's
  handshake to manipulate the comparison code.
- Explicit approval on both devices, or a strong shared secret for automation.
- Streaming file data in 64 KiB chunks; finite metadata and transfer-size limits.
- SHA-256 verification of every file and of the exact offered manifest.
- The entire item stays in a private staging directory until verified.
- Atomic publication preserves existing items: `photos`, then `photos (1)`, etc.
- A sender reports success only after the receiver acknowledges publication.
- UTF-8 filenames, nested and empty directories, and zero-byte files work.
- Symlinks and special files are skipped. Paths are checked on both computers.

The source is hashed during preparation and again while streaming. Changes to
its contents or size abort delivery. Keep the source stable during a transfer.

## CLI

```sh
xfer [--name NAME] [--output DIR] [--bind ADDRESS] [--no-open]
xfer menu
xfer send PATH [--to HOST] [--port PORT] [--dry-run]
xfer receive [--output DIR] [--name NAME] [--bind ADDRESS]
             [--port PORT] [--once] [--no-discovery] [--max-bytes BYTES]
xfer discover [--port PORT]
xfer doctor [--port PORT]
xfer --help
xfer --version
```

`--dry-run` hashes and checks a local item without connecting. `--once` exits
after one incoming session, with a nonzero exit code on rejection or failure.
The default receiver limit is **16 GiB** per item; use `--max-bytes` to change it.
A folder is one item. Send several items as a containing folder or in separate
transfers.

The default listener is IPv4. Direct IPv6 works with an explicit listener:

```sh
xfer receive --bind :: --no-discovery
xfer send ./photo.jpg --to '[fe80::1234%en0]:9000'
```

Use your local interface name/index for scoped link-local addresses. IPv6-only
listeners do not advertise through IPv4 discovery. The receiver's bind address
restricts both its TCP listener and its discovery socket.

## Automation

Set the **same strong random secret** on both computers via `XFER_TOKEN`, then
use `--yes`. Minimum 16 bytes; a randomly generated 32-byte value encoded as hex
is recommended. A password's length alone does not make it strong. Secrets are
never saved in configuration or included in events.

```sh
# Set XFER_TOKEN privately in the environment on both computers.
xfer --json receive --yes --once --output ./Received
xfer --json send ./artifact --to 192.168.1.42 --yes
```

PowerShell environment syntax is `$env:XFER_TOKEN = '<your secret>'`.
`--json` emits newline-delimited JSON events on stdout. Diagnostics and prompts
use stderr. JSON CLI transfers require `--yes`; there is no implicit trust flag or
unencrypted mode. Exit status is 0 on success and 1 on failure.

## Tests

```sh
zig fmt --check build.zig src
zig build test
zig build -Doptimize=ReleaseSafe
python3 tests/integration.py zig-out/bin/xfer
python3 tests/desktop.py zig-out/bin/xfer
```

On Windows use `python tests/integration.py zig-out/bin/xfer.exe`.
Python 3 is used only for process/network integration tests and release
packaging. CI runs the suite natively on Windows, macOS, and Linux, and builds
x86-64 and ARM64 for each platform. See [development](docs/DEVELOPMENT.md),
[installation](docs/INSTALLATION.md), and the [wire protocol](docs/PROTOCOL.md).

For the large-file pipeline and measured results, see
[performance](benchmarks/PERFORMANCE.md).

## Boundaries

This is a deliberate breaking redesign around nearby sharing. The old Rust
protocol, folder synchronization, TOFU peer database, self-updater, and terminal
framework are replaced by the Zig implementation. Old peers cannot connect.

The window is browser-based. There is no tray, OS sharing extension, background
launch service, or wireless link setup. The control server binds only to loopback
on a random port. Its APIs require a random launch capability and validate
Host/Origin; the LAN listener exposes only the encrypted transfer protocol.
File permissions, ownership, ACLs, timestamps, extended attributes, resource
forks, and executable bits are not copied. On Unix received files are private
(0600; directories 0700). On Windows they inherit destination ACLs.

Names must be representable on all three operating systems. Traversal, Windows
reserved names, control characters, trailing dots/spaces, and ASCII case aliases
are rejected. Unicode case/normalization aliases that the destination cannot
represent abort safely during exclusive creation. Names are limited to 255
UTF-8 bytes, paths to 4096 bytes and 64 components; destination filesystem limits
may be lower. Metadata is limited to 16 MiB and inventories to 100,000 entries.

Network failures remove staging. Force-quitting or power loss can leave a
hidden `.xfer-*.part` directory; it can be removed after stopping the receiver.
Interrupted items restart. If the final acknowledgement is lost, the item may
already be delivered; check the destination before retrying. Publication is
atomic visibility, not a guarantee against every filesystem/power-loss scenario.

See [SECURITY.md](SECURITY.md) for trust assumptions and reporting.
The measured Rust/Zig LAN comparison is in [benchmarks](benchmarks/README.md).

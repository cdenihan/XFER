# Installation

## From source

Install [Zig 0.17.0](https://ziglang.org/download/) for your computer.
Clone the repository, then build:

```sh
zig build -Doptimize=ReleaseSafe
```

macOS/Linux executable: `zig-out/bin/xfer`. Windows: `zig-out/bin/xfer.exe`.
The resulting executable needs no Zig installation on the recipient's computer.

For a local user installation on macOS/Linux:

```sh
mkdir -p "$HOME/.local/bin"
cp zig-out/bin/xfer "$HOME/.local/bin/xfer"
```

Add `$HOME/.local/bin` to your PATH if needed.

On Windows copy `zig-out\bin\xfer.exe` to a user-owned directory such as
`$env:LOCALAPPDATA\XFER` and add that directory to your user PATH. Close a running
receiver before replacing its executable.

## Release archives

The new release pipeline produces these targets:

| Platform | Targets |
| --- | --- |
| Linux | `x86_64-linux-musl`, `aarch64-linux-musl` |
| macOS | `x86_64-macos`, `aarch64-macos` |
| Windows | `x86_64-windows`, `aarch64-windows` |

After a Zig release is published on the repository's
[releases page](https://github.com/cdenihan/XFER/releases), download the archive
matching your OS and CPU and its `.sha256` sidecar. Verify before extracting:

```sh
# Linux
sha256sum -c xfer-2.0.0-x86_64-linux-musl.tar.gz.sha256
# macOS
shasum -a 256 -c xfer-2.0.0-aarch64-macos.tar.gz.sha256
```

PowerShell: `Get-FileHash .\xfer-2.0.0-x86_64-windows.zip -Algorithm SHA256`
and compare its hash with the sidecar. Extract and place the executable on PATH.
Old Rust release archives are incompatible with the Zig wire protocol.
This checkout has not itself published a new release.

## Open the sharing window

Launch `xfer` with no arguments, or double-click `xfer.exe` on Windows. It opens
the default browser and starts receiving. No Node.js, Python, web server package,
or downloaded frontend is needed to run XFER. On desktop Linux the standard
`xdg-open` launcher opens your browser; if the launcher is unavailable, use
`xfer --no-open` and open the printed URL yourself. Terminal-only/headless hosts
can use `xfer receive` and `xfer send` instead.

Use `--name "Office PC"` for a friendly discovery label and `--output DIR` to
choose a destination. Otherwise the computer hostname and Downloads/XFER are
used. The printed browser URL contains a private launch capability; keep it
local. Closing the browser tab does not stop the receiver. Use **Quit XFER**
or stop its process to exit.

## Network access

Run XFER on two computers on the same reachable IP network. Allow its receiving
TCP port through the firewall. Allow UDP on that port for nearby discovery.
Both default to 9000; use the same `--port` on both computers to change it.

The default receiver binds IPv4 interfaces. Restrict it with `--bind ADDRESS`.
IPv6 direct transfers use `receive --bind :: --no-discovery`. Nearby discovery
uses IPv4 broadcast and cannot cross routed subnets. For multicast/broadcast
filtered networks, manually enter the receiving computer's address with `--to`.

The default output is the user's `Downloads/XFER` directory. The receiver creates
it if needed. `--output DIR` chooses another writable destination. Each item
requires enough free disk space to stage the complete transfer. Browser senders
also need temporary disk space for the selected files. Existing items
are preserved; there is no implicit overwrite or merge.

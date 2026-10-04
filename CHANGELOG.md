# Changelog

## 1.0.0 — Zig redesign

- Initialized a new project with `zig init` using Zig 0.17.0.
- Replaced the Rust application and all runtime dependencies with Zig.
- Launch a browser sharing window with drag-and-drop, file/folder pickers, nearby
  computers, progress, cancellation, and incoming code-confirmation prompts.
- Keep a terminal menu and scriptable CLI using the same encrypted transfer core.
- Isolate browser controls on a capability-protected loopback listener; embed all
  UI assets without additional runtime packages.
- Added nonce-bound IPv4 broadcast discovery and direct IPv4/IPv6 connections.
- Introduced a committed ephemeral X25519 handshake, HKDF-SHA-256 directional
  keys, human comparison codes, and ChaCha20-Poly1305 authenticated records.
- Require explicit consent on both devices; unattended approval requires a
  strong environment-provided shared secret.
- Verify each file and the complete offered manifest before atomic publication.
- Preserve existing destinations and clean up staging after network failures.
- Added unit and real process/network integration tests, native CI on Windows,
  macOS and Linux, six cross-build targets, and checksumed release archives.
- Added reproducible Rust/Zig LAN benchmarks against the requested Linux host.
- Removed old protocol compatibility, sync/delta/reconciliation commands,
  remembered-peer configuration, self-update, Rust toolkit release integration,
  and the previous terminal framework.

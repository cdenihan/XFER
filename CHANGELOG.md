# Changelog

All notable changes to XFER are documented here.

## Unreleased

### Changed

- Rewrote the application as a maintainable library-first Rust project.
- Replaced the multi-port protocol with one typed, ordered TCP record stream.
- Replaced the placeholder terminal workflow with a Ratatui interface.
- Redesigned the clap CLI and intentionally removed v3 wire compatibility.
- Reused encrypted record buffers and throttled progress updates on the bulk
  data path.

### Security

- Added X25519, HKDF-SHA-256, and ChaCha20-Poly1305 secure sessions.
- Added an encrypted readiness check before TOFU identity persistence.
- Added sequence-authenticated records, safe path validation, duplicate-entry
  rejection, atomic staging, and per-file plus manifest verification.
- Added optional shared-token key hardening.

### Added

- IPv4/IPv6 support, exclusion globs, safe symlink following, dry-run planning,
  JSON events, peer management, diagnostics, shell completions, collision-safe
  receive names, and explicit overwrite behavior.
- TTL-1 same-LAN receiver discovery, multi-machine selection, and receiver IP
  display in the TUI without subnet or port scanning.
- Checksum-verifying, atomic installers for Linux GNU/musl, macOS, and Windows
  across every supported x86_64 and ARM64 release target.
- Extensive unit, CLI subprocess, installer, secure loopback, protocol
  rejection, filesystem-safety, and directory-transfer tests.
- Native and cross-platform CI without duplicate push/PR runs.
- Date-based release automation for every push to `main`, with a sequential
  per-day suffix and the release version compiled into the CLI.
- A checksum-verifying `xfer update` command that replaces the currently
  installed executable with the latest official release or a specifically
  pinned release.
- Compatible release-version exchange during transfers, with mismatch warnings
  and an interactive update offer on the older CLI.

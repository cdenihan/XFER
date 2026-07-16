# Changelog

All notable changes to XFER are documented here.

## 4.0.0 - Unreleased

### Changed

- Rewrote the application as a maintainable library-first Rust project.
- Replaced the multi-port protocol with one typed, ordered TCP record stream.
- Replaced the placeholder terminal workflow with a Ratatui interface.
- Redesigned the clap CLI and intentionally removed v3 wire compatibility.

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
- Unit, CLI, secure loopback, and directory-transfer tests.
- Native and cross-platform CI plus tag-driven release automation.

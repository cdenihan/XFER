# Security policy

## Supported version

Security fixes are made in the latest date-based release.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting feature for this repository.
Do not open a public issue for a suspected vulnerability that could place users
or transferred data at risk.

Include the affected version, platform, reproduction steps, expected impact, and
any proof-of-concept material that is safe to share.

## Threat model

XFER's secure mode is designed to protect transfer confidentiality and integrity
against passive observers and active network attackers after the receiver
identity has been authenticated.

On the first connection, users must compare the displayed short authentication
string (SAS) on both machines. If they approve a mismatched code, TOFU cannot
detect that first-connection interception. A previously pinned identity change
is displayed as a high-severity warning and is never accepted automatically.

The optional shared token is mixed into HKDF input. It adds a possession factor
and causes the encrypted readiness exchange to fail when tokens differ. It does
not replace SAS comparison or identity pinning.

## Cryptography

- Receiver identity: static X25519 key, generated with the operating system CSPRNG
- Sender key: ephemeral X25519 key per connection
- Key derivation: HKDF-SHA-256
- Record encryption: ChaCha20-Poly1305
- Nonces: independent random session material plus directional monotonic sequence numbers
- File verification: SHA-256 per file
- Transfer verification: SHA-256 manifest over ordered paths and file digests

Protocol headers are authenticated as AEAD associated data. Record sequence
numbers must be exactly monotonic, preventing deletion, reordering, or replay
inside a session.

The identity file and peer store use private Unix permissions when applicable.
XFER cannot protect secrets after the local account or either endpoint is
compromised.

## LAN discovery

While a discoverable receiver is waiting, it sends a small XFER presence
announcement every two seconds to the administratively scoped IPv4 multicast
address `239.255.90.90:39090` with TTL 1. Senders listen to that group and do
not enumerate subnets, probe hosts, scan ports, or attempt connections until the
user starts a transfer. `xfer receive --no-discovery` disables announcements.

Discovery packets are intentionally unauthenticated and contain only the
machine label, transfer port, protocol version, and security-mode flag. Treat
the discovered name and address as advisory: the secure transfer handshake, SAS
comparison, and pinned receiver identity remain authoritative. A malicious LAN
peer can spoof or suppress discovery but cannot bypass those checks.

## Release installers

Official installers select only a named supported release artifact, download
the adjacent SHA-256 file, verify it before execution, and stage replacement in
the destination directory. Checksum, download, compatibility, or write failures
leave an existing installation unchanged. Network release URLs must use HTTPS;
`file://` is accepted only to support offline mirrors and installer tests.

## Filesystem safety

Incoming names are constrained to relative normal path components. Absolute
paths, parent traversal, duplicate or case-colliding entries, and non-portable
platform names are rejected. Data is written under a fresh staging directory
inside the requested output directory, verified, synced, and then renamed into
place.

Symlinks are not created by the receiver. Sender symlinks are skipped unless
`--follow-links` is set, and followed targets must remain inside the source root.

## Insecure mode

`--insecure` disables confidentiality, record authentication, SAS, and identity
pinning. SHA-256 still catches accidental corruption, but it is not meaningful
against an active attacker who can replace both content and hashes.

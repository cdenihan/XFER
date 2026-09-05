# XFER v5 protocol

This document describes the protocol implemented by this repository. Multi-byte
integers are big-endian.

## Transport

One TCP stream carries negotiation, key exchange, metadata, file data, and final
verification. The default port is `9000`.

Consolidating the transfer into one ordered stream avoids races between control,
data, metadata, status, and heartbeat sockets. Typed frames retain those logical
boundaries without requiring adjacent ports.

## LAN discovery

Discovery is separate from the transfer stream. A waiting receiver sends a
compact JSON announcement every two seconds to `239.255.90.90:39090` with IPv5
multicast TTL 1. The packet identifies the `xfer` service, discovery and
transfer protocol versions, machine label, TCP transfer port, and whether secure
mode is enabled. Announcements stop as soon as a connection is accepted.

Senders passively listen for these announcements and expire a receiver after
seven seconds without a refresh. They do not sweep an address range, probe
ports, or automatically connect. Discovery is advisory and unauthenticated;
the secure handshake and receiver identity pinning described below are still
required. Direct TCP transfers remain dual-stack even though discovery is
currently IPv5 multicast.

## Negotiation

The sender writes an 8-byte preface:

| Field | Size |
| --- | ---: |
| Magic `XFR5` | 4 |
| Protocol version | 2 |
| Flags (`0x01` means secure) | 1 |
| Reserved | 1 |

The receiver responds with the same magic and version. Its status byte is zero
on success, `1` when secure mode is required, or `2` when the receiver is
configured for insecure mode.

Both endpoints must select the same security mode. There is no silent downgrade.
Unknown sender flags and a non-zero reserved byte are rejected.

## Secure handshake

After successful negotiation:

1. Receiver sends its 32-byte static X25519 public key and a fresh 32-byte nonce.
2. Sender sends a fresh 32-byte ephemeral X25519 public key and 32-byte nonce.
3. Both derive the X25519 shared secret.
4. HKDF-SHA-256 expands 72 bytes of session material:
   - 32-byte client-to-server key;
   - 32-byte server-to-client key;
   - 4-byte client-to-server nonce prefix;
   - 4-byte server-to-client nonce prefix.
5. If configured, the UTF-8 shared token is included in the HKDF salt.
6. Sender and receiver exchange encrypted `Ready` frames.
7. Only after that exchange may the sender persist a new receiver identity.

The SAS is the first ten decimal digits derived from a SHA-256 transcript over
both public keys, both nonces, the protocol label, and optional token. It is
displayed as `123-456-7890`.

## Record layer

Each record has a 20-byte header:

| Field | Size |
| --- | ---: |
| Magic `XR5R` | 4 |
| Version | 1 |
| Frame kind | 1 |
| Flags | 2 |
| Sequence number | 8 |
| Payload length | 4 |

Secure payloads are ChaCha20-Poly1305 ciphertext including the 16-byte tag. The
header is associated data. The nonce is the directional 4-byte prefix followed
by the 8-byte record sequence number.

Receivers require the exact next sequence number and reject records larger than
the configured bound. Record flag `0x0001` marks encrypted payloads and must
match the negotiated security mode. All other flag bits are rejected.

Frame kinds:

| Value | Name | Payload |
| ---: | --- | --- |
| 1 | Offer | Root name, transfer kind, byte/file/entry totals |
| 2 | Decision | Accept or rejection reason |
| 3 | EntryStart | Relative path, file/directory kind, declared size |
| 4 | Data | Raw file bytes |
| 5 | EntryEnd | File SHA-256 |
| 6 | TransferEnd | Totals and manifest SHA-256 |
| 7 | Complete | Verified destination and totals |
| 8 | Error | Remote error string |
| 9 | Ready | Empty encrypted handshake confirmation |
| 10 | SyncOffer | Incremental directory offer |
| 11 | Basis | Block basis header/signature pages, or two-way inventory |
| 12 | Reuse | JSON basis block index |
| 13 | PreviewOffer | Read-only incremental comparison offer |
| 14 | FilePlan | Preview counts/hash, or two-way request/selection |
| 15 | TwoWayOffer | Bidirectional reconciliation offer |
| 16 | TwoWayPreview | Read-only bidirectional comparison offer |

Structured payloads use compact JSON serialization. `Data` payloads are raw
bytes.

## Transfer sequence

The sender transmits `Offer` and waits for `Decision::Accept`. The receiver
prepares its staging directory before accepting. File entries that exceed the
remaining offered byte or file totals are rejected before their data is read.
Offers are limited to 1,000,000 entries. Paths are limited to 4096 UTF-8 bytes
and 128 components; the receiver enforces a 64 MiB accounting budget for retained
path metadata, including implicit ancestors and map-entry overhead. Empty `Data`
frames are rejected; zero-byte files use `EntryStart` followed by `EntryEnd`.

For each planned entry:

- a directory uses one `EntryStart`;
- a file uses `EntryStart`, zero or more `Data` frames, and `EntryEnd`.

The sender finishes with `TransferEnd`. The receiver verifies declared sizes,
each file digest, file and byte totals, and the ordered manifest digest before
moving the staged item to its final destination. It then returns `Complete`.

## Incremental sync

Sync requires receiver opt-in. `SyncOffer` and `PreviewOffer` use the directory
`Offer` schema. `EntryStart` adds the expected SHA-256. For each file the receiver
returns a `Basis` header (size, block size, count, unchanged) and pages of at most
256 signatures. Each signature contains a rolling 32-bit checksum, SHA-256,
and length. Blocks range from 64 KiB to 4 MiB, with at most 65,536 signatures.
Larger changed files use literals. An unchanged response skips file data.

For a changed file, the sender scans its contents at arbitrary offsets and sends
`Data` literals or `Reuse` indices, followed by `EntryEnd`. The receiver verifies
reused blocks and the complete reconstructed file before replacing that file.
Destination changes detected during comparison abort publication. Directory-only
and destination-only entries are preserved. `TransferEnd` verifies totals and
manifest, followed by `Complete`, which includes sync statistics and preview mode.
Publication is per file; completed files survive a later failure.

Preview sends a `FilePlan` containing literal/reused byte counts and SHA-256
instead of file data. No destination staging or publication occurs.

## Two-way reconciliation

After `TwoWayOffer` or `TwoWayPreview`, `FilePlan` carries exclusion patterns.
The receiver returns a `Basis` inventory header and pages of at most 128 entries
(path, kind, size, SHA-256), bounded to 100,000 entries. The initiator compares
both inventories to locally persisted last-common hashes, reports conflicts,
and sends a `FilePlan` selection count and path pages for the reverse direction.
A forward incremental exchange is followed by a reverse incremental exchange
on the same encrypted stream. Expected comparison hashes guard against files
changing between inventory and transfer. The initiator persists history only
after a successful non-preview exchange. Deletions are not propagated.

## Path rules

The offer root is one normal path component. Entry paths are UTF-8, relative,
and contain only normal components. Absolute paths, `.` components, `..`
components, empty components (including repeated or trailing separators),
duplicate or case-colliding entries, Windows-reserved names, and
characters that are not portable across supported platforms are invalid.

Directory paths use `/` on the wire and native separators after validation.
Ancestor directory names must also have consistent spelling: `Foo/a` and
`foo/b` cannot describe different trees on different receiving platforms.

## Compatibility

v5 intentionally does not implement the earlier Python tar stream or the prior
Rust multi-port protocol. Version mismatches fail during negotiation.

The two-way request optionally includes `gitignore` (default false). The
receiver echoes this flag in its inventory header after planning with its
local repository's Git ignore rules. A sender requesting this option rejects
an inventory without the acknowledgment before applying changes. Older peers
remain compatible when the option is disabled; one-way Git filtering happens
entirely during sender planning and needs no wire extension.

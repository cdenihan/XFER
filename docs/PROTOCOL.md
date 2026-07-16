# XFER v4 protocol

This document describes the protocol implemented by this repository. Multi-byte
integers are big-endian.

## Transport

One TCP stream carries negotiation, key exchange, metadata, file data, and final
verification. The default port is `9000`.

Consolidating the transfer into one ordered stream avoids races between control,
data, metadata, status, and heartbeat sockets. Typed frames retain those logical
boundaries without requiring adjacent ports.

## Negotiation

The sender writes an 8-byte preface:

| Field | Size |
| --- | ---: |
| Magic `XFR4` | 4 |
| Protocol version | 2 |
| Flags (`0x01` means secure) | 1 |
| Reserved | 1 |

The receiver responds with the same magic and version. Its status byte is zero
on success, `1` when secure mode is required, or `2` when the receiver is
configured for insecure mode.

Both endpoints must select the same security mode. There is no silent downgrade.

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
| Magic `XR4R` | 4 |
| Version | 1 |
| Frame kind | 1 |
| Flags | 2 |
| Sequence number | 8 |
| Payload length | 4 |

Secure payloads are ChaCha20-Poly1305 ciphertext including the 16-byte tag. The
header is associated data. The nonce is the directional 4-byte prefix followed
by the 8-byte record sequence number.

Receivers require the exact next sequence number and reject records larger than
the configured bound.

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

Structured payloads use compact JSON serialization. `Data` payloads are raw
bytes.

## Transfer sequence

The sender transmits `Offer` and waits for `Decision::Accept`.

For each planned entry:

- a directory uses one `EntryStart`;
- a file uses `EntryStart`, zero or more `Data` frames, and `EntryEnd`.

The sender finishes with `TransferEnd`. The receiver verifies declared sizes,
each file digest, file and byte totals, and the ordered manifest digest before
moving the staged item to its final destination. It then returns `Complete`.

## Path rules

The offer root is one normal path component. Entry paths are UTF-8, relative,
and contain only normal components. Absolute paths, `.` components, `..`
components, duplicate or case-colliding entries, Windows-reserved names, and
characters that are not portable across supported platforms are invalid.

Directory paths use `/` on the wire and native separators after validation.

## Compatibility

v4 intentionally does not implement the earlier Python tar stream or the prior
Rust multi-port protocol. Version mismatches fail during negotiation.

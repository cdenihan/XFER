# XFER Zig protocol v1

This wire format deliberately breaks compatibility with all Rust versions.
All integers below are little-endian. TCP carries one item per connection;
a receiver may listen for many successive connections.

## Committed handshake

Each peer creates an ephemeral X25519 key pair and a secure random 32-byte nonce.
Its 73-byte hello is:

| Offset | Bytes | Value |
| --- | ---: | --- |
| 0 | 8 | ASCII `XFERZ017` |
| 8 | 32 | X25519 public key |
| 40 | 32 | Random nonce |
| 72 | 1 | 0 without a token, 1 with a token |

The commitment is `SHA256(hello)`. The sender writes its 32-byte commitment;
the receiver reads it and writes its commitment. The sender writes its hello;
the receiver reads it and writes its hello. Each validates the remote commitment,
magic and token mode. Neither may select a hello after learning the peer's hello.
All secrets and random nonces must be fresh for every connection.

Let `shared = X25519(local_secret, remote_public)` and:

```text
salt = SHA256("XFER session v1\x00" || sender_hello || receiver_hello || token)
prk  = HKDF-SHA256.extract(salt, shared)
keys = HKDF-SHA256.expand(prk,
         "XFER directional keys and comparison code v1", 96 bytes)
```

`keys[0..32]` encrypts sender-to-receiver records; `keys[32..64]` encrypts
receiver-to-sender records. `keys[64..70]`, rendered as lowercase hex in three
groups of four digits, is the full 48-bit human comparison code. No byte of
plaintext file content is sent before consent. Secrets are cleared on normal
scope exit.

## Records

Each direction maintains an independent implicit `u64` sequence, beginning at 0.
A frame is `u32 length || ciphertext || 16-byte tag`. `length` counts ciphertext
and tag, excluding its own four bytes. Plaintext is `u8 kind || payload`.
Valid lengths are 17 through 65553 inclusive; payloads have at most 65536 bytes.

ChaCha20-Poly1305 uses:

```text
nonce = 4 zero bytes || u64 sequence
AAD   = u32 length || u64 sequence
```

The sequence increments only after a complete authenticated read or successful
write. Exhausted counters, invalid tags and unknown kinds abort the connection.

| Kind | Number | Payload |
| --- | ---: | --- |
| offer | 1 | `u32` encoded-manifest length |
| manifest | 2 | Nonempty fragments of exact manifest bytes |
| accept | 3 | Empty |
| reject | 4 | Empty |
| data | 5 | Nonempty file bytes, at most the current file's remaining size |
| finish | 6 | 32-byte SHA-256 of encoded manifest |
| delivered | 7 | UTF-8 destination name after publication |

## Manifest and approval

The sender encrypts `offer` followed by enough `manifest` records to carry the
specified length, at most 16 MiB. The concatenation is a JSON object:

```json
{
  "name": "photos",
  "entries": [
    {"path": "photos", "kind": "directory", "size": 0, "hash": [0, 0]},
    {"path": "photos/a.jpg", "kind": "file", "size": 1234, "hash": [12, 34]}
  ],
  "total": 1234
}
```

The hash arrays above are shortened for readability; every hash contains
exactly 32 bytes. Zig encodes a hash as an integer array when it is not valid
UTF-8, or as a JSON string when it is (with control bytes escaped). The receiver
accepts either representation and requires exactly 32 decoded bytes. Directory
sizes and hash bytes are zero. Entries are
strictly sorted by UTF-8 bytes and contain exactly one root named by `name`.
Each child requires an already declared directory parent. Portable-name rules,
ASCII case uniqueness, metadata/path/depth caps and the receiver byte limit are
validated before creating files. Hashes are computed before transmission.

Both sides display the offered item and code. Each sends `accept` or `reject`;
file contents may flow only after each side has sent and received `accept`.
Unattended consent requires the explicitly configured shared token and `--yes`.
A rejection, invalid manifest or failed authentication ends the connection.

## Stream and commit

Files stream in manifest order. Each file consumes exactly its declared size
across nonempty `data` records. Zero-length files consume no records. Each
record belongs to one file; it may not cross a file boundary. Directories consume
no data records. Both sides compute each file's SHA-256 and require the offered
hash. The receiver syncs each completed file within private staging.

After all files the sender sends `finish`, with the SHA-256 of the exact
manifest bytes. The receiver requires that digest and the exact total byte
count, then publishes the entire root using nonreplacing atomic rename.
Existing names select a numbered suffix. It reports local delivery and sends
`delivered` with the selected name. The sender waits for this authenticated
acknowledgement before reporting success. No overwrite or folder merge exists.

## Discovery

Nearby discovery uses IPv4 UDP on the configured TCP port. A query is 24 bytes:
ASCII `XFERQ002`, followed by a 16-byte secure random nonce. Clients send it to
IPv4 limited broadcast and loopback, then collect responses for three seconds.

A reply is ASCII `XFERR002` (8 bytes), echoed query nonce (16), TCP port (`u16`),
label length (`u8`, 1–63), a 16-byte random instance identifier, and that many
UTF-8 label bytes. It has no trailing bytes. The identifier changes each time
a receiver starts; browser windows filter their own instance from results.
The client validates the nonce/label and uses the packet source IP plus the
returned port. Labels convey no authenticated identity. Truncated and malformed
packets are ignored. Replies contain no file metadata or persistent identifier.

## Browser control transport

A separate HTTP server listens only on IPv4 loopback at an ephemeral port.
It serves embedded HTML/CSS/JavaScript and accepts bearer-authenticated local
API requests. Its capability and controls are unrelated to LAN discovery or
the encrypted transfer handshake. Browser requests may upload selected bytes,
create selected subdirectories, choose a peer, approve an identified current
session with its exact code, cancel, inspect state, or quit. They may not read
arbitrary source files. Both browser and terminal peers use the TCP protocol
above and interoperate with each other.

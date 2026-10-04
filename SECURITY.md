# Security

## Reporting

Report vulnerabilities privately through the repository's GitHub security
advisory channel. Include the XFER/compiler version, OS, reproducer and impact.
Avoid including real shared secrets or private transferred files.

## Trust model

XFER is for direct sharing over a reachable IP network. Network attackers may
observe, alter, replay or inject packets. Discovery labels and source addresses
are untrusted suggestions, not identities. No cloud service or certificate
issuer participates in a transfer.

Without a shared secret, **both people must compare the full 48-bit session
code through a trusted channel and approve**. Clicking yes without comparison
does not authenticate the other computer. Each connection has a fresh code;
XFER keeps no permanent identity or remembered-peer database.

With `XFER_TOKEN`, knowledge of the same strong random secret authenticates the
encrypted record exchange. `--yes` additionally grants consent for every offered
item within the receiver's limits. The token must be at least 16 bytes; use a
cryptographically random 32-byte token encoded as hex. A captured transcript
permits offline guesses of a weak token. This protocol is not a password-based
key exchange. Secrets should be provided privately via the environment and are
never written to configuration or events.

## Protocol protections

Both peers commit to their ephemeral X25519 public keys and random nonces using
SHA-256 before revealing them. The complete ordered handshake and token enter
HKDF-SHA-256. Independently derived directional ChaCha20-Poly1305 keys protect
record kinds and payloads; authenticated length and implicit sequence counters
bind framing, order and direction. Bad commitments, invalid X25519 points,
mismatched token mode, failed tags, oversized records and unexpected message
kinds abort the session. No insecure fallback exists.

The offered metadata is encrypted, but is exchanged **before human code
verification** so the recipient can review the offer. It includes paths, sizes
and SHA-256 file digests. An active intermediary may therefore learn that
metadata before being rejected. File contents are withheld until both peers
approve. Ordinary passive listeners cannot read metadata or content.

Hashes protect every file and the exact offered manifest. Changes during source
streaming abort the session. Receive staging is on the destination filesystem
and publication uses a nonreplacing atomic rename. Verified data becomes visible
as a complete item. Success on the sender requires an authenticated publication
acknowledgement; losing that acknowledgement can leave the sender uncertain
although the item was delivered.

## Filesystem protections

Validation rejects absolute/traversing paths, empty components, backslashes,
Windows device names, alternate streams, controls, trailing dots/spaces and
ASCII case aliases. A manifest declares exactly one root with existing
parent-before-child directory entries and exact byte totals. Unicode aliases
that collide on the destination fail exclusive creation, rather than merge or
replace. Directory handles are traversed component by component without
following symlinks. Sources containing symlinks or special files skip them.

Unix staging directories are 0700 and files are 0600. Windows inherits the
destination's ACLs; choose a private destination directory. Another process
running as the same OS user or an administrator is outside this threat model.
Malicious transferred file contents remain malicious; XFER verifies bytes and
does not open, execute or scan received files.

## Browser control surface

The browser server binds only to `127.0.0.1` on a random port, separate from the
LAN transfer listener. A fresh 256-bit capability starts in the launch URL's
fragment, is removed from the address bar, and is retained in same-origin
sessionStorage for reloads. Every API request requires its bearer header. Exact
Host validation rejects DNS rebinding; Origin, when present, must be the same
loopback origin. No CORS access is granted. CSP permits only embedded,
same-origin script/style assets and disallows framing. Third-party webpages
cannot upload, approve transfers, change state, or quit the app.

Only user-selected browser files enter a private temporary upload tree; the
control API exposes no arbitrary-file read or command execution endpoint.
Upload paths receive the same portable traversal validation as transfer paths.
Approval binds to the current request identifier and the exact current code.
The UI asks the person to confirm a code comparison; knowledge of the browser
capability or token does not cause implicit approval. Received files are never
opened automatically. A local process running as the same user is outside the
threat model, and can access the browser capability or selected bytes.

HTTP headers are bounded to 16 KiB, JSON requests to 8 KiB, simultaneous browser
connections to 16, and each request to five minutes. The selection uses the
configured transfer byte limit and finite entry/path caps. Cancellation shuts
down a blocked upload socket and removes its temporary tree. As with receive
staging, force-quitting may leave `xfer-upload-*` in the OS temporary directory.
Browser selection makes a local temporary copy; direct CLI sources do not.

## Resource limits and failure behavior

Each record carries at most 64 KiB of payload. The encoded manifest is capped
at 16 MiB and 100,000 entries; paths are limited to 4096 bytes, 64 components,
and names to 255 UTF-8 bytes. The default payload cap is 16 GiB per item.
Discovery collects at most 64 receivers and runs for three seconds. Queries
contain a fresh nonce; replies must echo it and use their packet source as the
endpoint. Receiver replies are rate-limited to one per 20 ms.

Connections have a 10-second timeout; handshake reads allow 15 seconds.
Record headers allow up to 300 seconds for consent/idle time, record bodies
60 seconds, writes 300 seconds. Metadata has a two-minute deadline, consent a five-minute deadline, and the
transfer phase a 24-hour deadline. Every record operation is also capped by its
phase deadline. Terminal consent expires after five minutes. A receiver serves
one session at a time. Untrusted LAN clients can occupy that session until its
timeout or prompt is canceled; firewall restrictions and a shared secret reduce
exposure but do not eliminate denial of service.

Network failures clean up private staging. Process termination or power loss can
leave hidden staging directories that must be removed manually after stopping
XFER. Per-file sync plus atomic rename does not guarantee crash durability on
every filesystem. Received metadata/permissions are not reproduced.

This is a new protocol implementation, not an independently audited product.
The automated tests verify concrete failure cases; they do not prove the
absence of vulnerabilities.

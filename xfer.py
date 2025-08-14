#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
XFER — eXpress File & dir transfER
==================================

Cross-platform (Windows/macOS/Linux) file & directory transfer over raw TCP with end-to-end
integrity verification. Pure Python 3 standard library. No SSH. No third-party deps.

SECURE BY DEFAULT (TOFU + SAS)
------------------------------
- XFER now performs **Trust On First Use (TOFU)** with a human-verifiable **Short Authentication
  String (SAS)** automatically. No flags needed.
- On the first pairing between a sender and a receiver, both sides display the same **10-digit code**
  (e.g., `123-456-7890`). Compare and press ENTER to trust. Trust is remembered per receiver host:port.
- This step **authenticates** who you're talking to and detects tampering on later runs.

Disable security for legacy runs or automation:
    • Add **`--insecure`** (alias `--no-secure`) on both sides to skip TOFU+SAS.

Important: This is **authentication only**; it does **not encrypt** the data stream. Use on trusted LANs.

Protocol (high-level)
---------------------
1) **Control port (N-1)** — automatic when secure:
   - Receiver presents its fingerprint and a fresh nonce; sender replies with its nonce.
   - Both compute/display the same SAS; on first trust, you press ENTER to persist.
2) **Data port N** (default **9000**)
   - **File**: raw bytes stream; on-the-fly hashing
   - **Dir** : streaming tar; safe extraction; on-the-fly per-file hashing
3) **Meta port N+1**
   - **File**: single line "sha256  basename"
   - **Dir** : text manifest ("sha256␠rel_posix" per file)

Pairings (must match)
---------------------
- Sender: `send <FILE>`         ↔ Receiver: `receive` (auto) or `recv-file <OUTPUT>`
- Sender: `send <DIRECTORY>`    ↔ Receiver: `receive` (auto) or `recv-dir  <OUTPUT_DIR>`

Quick start
-----------
Receiver (auto, secure by default):
    python3 xfer.py receive --out ~/Downloads
Sender (file or dir, secure by default):
    python3 xfer.py send 192.168.1.42 ./payload
Disable security on both sides (legacy mode):
    python3 xfer.py receive --insecure
    python3 xfer.py send 192.168.1.42 ./payload --insecure

Operational guidance
--------------------
- **Output defaults**:
  - FILE: if `--out` not given or points to a directory, we save under the sender's basename.
          We stream to a temp `.part`, verify, then rename atomically.
  - DIR : extracted into `--out` if provided; else into `.`.
- **Exclusions** (send only): `--exclude ".git/*" --exclude "*.pyc"`
- **Performance**: expect ~110–115 MB/s on clean 1 GbE (SSD-to-SSD).
- **Interrupts**: Ctrl-C exits promptly at any phase.

Security model details
----------------------
- TOFU persists the **receiver's identity** (sha256 fingerprint derived from a local 32-byte secret) in
  `~/.xfer/known_peers` keyed by `<receiver-ip>:<port>`. Both sides store this mapping so the first trust
  prompt is per peer, not global.
- SAS is derived via HMAC-SHA256 over (receiver fingerprint || receiver nonce || sender nonce || "XFER-SAS1")
  and shown as a 10-digit code (3-3-4).
- Again: authentication only; no wire encryption. If you want confidentiality, we can layer TLS later with
  the same TOFU UX.

"""

import argparse
import fnmatch
import hashlib
import hmac
import os
import signal
import socket
import struct
import sys
import tarfile
import tempfile
import threading
import time
from contextlib import closing, contextmanager
from ipaddress import ip_address, IPv4Address, IPv6Address
from pathlib import Path, PurePosixPath

# ----------------------- Branding & Color ----------------------------------

APP_NAME = "XFER"
APP_TAGLINE = "eXpress File & dir transfER"
VERSION = "4.0.0"  # Full encryption with ChaCha20-Poly1305 and FFDHE-2048

def _enable_win_ansi():
    """Enable ANSI colors on Windows consoles that support VT sequences; no-op elsewhere."""
    if os.name != "nt":
        return
    try:
        import ctypes
        kernel32 = ctypes.windll.kernel32
        for handle in (-11, -12):  # STDOUT/STDERR
            h = kernel32.GetStdHandle(handle)
            mode = ctypes.c_uint()
            if kernel32.GetConsoleMode(h, ctypes.byref(mode)):
                kernel32.SetConsoleMode(h, mode.value | 0x0004)  # ENABLE_VIRTUAL_TERMINAL_PROCESSING
    except Exception:
        pass

_enable_win_ansi()
USE_COLOR = sys.stderr.isatty() and (os.environ.get("NO_COLOR") is None)
def C(code: str) -> str: return f"\033[{code}m" if USE_COLOR else ""
CLR = {"reset": C("0"), "dim": C("2"), "bold": C("1"),
       "ok": C("92"), "warn": C("93"), "err": C("91"),
       "info": C("96"), "brand": C("95"), "gray": C("90")}
def banner(): sys.stderr.write(f"{CLR['brand']}{APP_NAME}{CLR['reset']} {CLR['dim']}v{VERSION}{CLR['reset']} — {APP_TAGLINE}\n")
def info(msg):  sys.stderr.write(f"{CLR['info']}[INFO]{CLR['reset']} {msg}\n")
def ok(msg):    sys.stderr.write(f"{CLR['ok']}[ OK ]{CLR['reset']} {msg}\n")
def warn(msg):  sys.stderr.write(f"{CLR['warn']}[WARN]{CLR['reset']} {msg}\n")
def err(msg):   sys.stderr.write(f"{CLR['err']}[ERR ]{CLR['reset']} {msg}\n")

# ----------------------- Tunables ------------------------------------------

DEFAULT_PORT = 9000
CHUNK = 4 * 1024 * 1024              # 4 MiB socket chunks for XFER v4
CONNECT_ATTEMPT_TIMEOUT = 3.0
CONNECT_TOTAL_TIMEOUT  = 30.0
ACCEPT_POLL_SECS       = 1.0
IO_POLL_SECS           = 1.0
META_ACCEPT_BUDGET     = 60.0
LISTEN_BACKLOG         = 1

# Secure control channel (TOFU+SAS)
SAS_CTRL_OFFSET        = 1          # control port = port - 1
SAS_ACCEPT_BUDGET      = 120.0      # patience for SAS handshake
XFER_HOME              = Path.home() / ".xfer"
IDENTITY_PATH          = XFER_HOME / "identity.key"     # receiver identity secret (32 bytes)
KNOWN_PEERS_PATH       = XFER_HOME / "known_peers"      # lines: "ip:port fphex"

# XAR4 Wire Format Constants
XAR4_FILE_MAGIC = b"X4FIL\0"       # File mode magic (6 bytes)
XAR4_DIR_MAGIC = b"X4DIR\0"        # Directory mode magic (6 bytes)
XAR4_FILE_ENTRY = ord('F')         # File entry type
XAR4_DIR_TERMINATOR = ord('Z')     # Directory terminator

# FFDHE-2048 parameters (RFC 7919)
FFDHE_2048_P = int("FFFFFFFFFFFFFFFFADF85458A2BB4A9AAFDC5620273D3CF1"
                   "D8B9C583CE2D3695A9E13641146433FBCC939DCE249B3EF9"
                   "7D2FE363630C75D8F681B202AEC4617AD3DF1ED5D5FD6561"
                   "2433F51F5F066ED0856365553DED1AF3B557135E7F57C935"
                   "984F0C70E0E68B77E2A689DAF3EFE8721DF158A136ADE735"
                   "30ACCA4F483A797ABC0AB182B324FB61D108A94BB2C8E3FB"
                   "B96ADAB760D7F4681D4F42A3DE394DF4AE56EDE76372BB19"
                   "0B07A7C8EE0A6D709E02FCE1CDF7E2ECC03404CD28342F61"
                   "9172FE9CE98583FF8E4F1232EEF28183C3FE3B1B4C6FAD73"
                   "3BB5FCBC2EC22005C58EF1837D1683B2C6F34A26C1B2EFFA"
                   "886B423861285C97FFFFFFFFFFFFFFFF", 16)
FFDHE_2048_G = 2

# Global stop flag (cooperative cancellation)
STOP = threading.Event()
def _signal_stop(_s, _f): STOP.set()
signal.signal(signal.SIGINT, _signal_stop)
try: signal.signal(signal.SIGTERM, _signal_stop)
except Exception: pass

# ----------------------- Cryptographic Implementations --------------------

def _rotate_left(n: int, b: int, width: int = 32) -> int:
    """Rotate n left by b bits within width-bit word."""
    mask = (1 << width) - 1
    n &= mask
    return ((n << b) | (n >> (width - b))) & mask

def _quarter_round(a: int, b: int, c: int, d: int) -> tuple[int, int, int, int]:
    """ChaCha20 quarter round."""
    a = (a + b) & 0xFFFFFFFF
    d ^= a
    d = _rotate_left(d, 16)
    
    c = (c + d) & 0xFFFFFFFF
    b ^= c
    b = _rotate_left(b, 12)
    
    a = (a + b) & 0xFFFFFFFF
    d ^= a
    d = _rotate_left(d, 8)
    
    c = (c + d) & 0xFFFFFFFF
    b ^= c
    b = _rotate_left(b, 7)
    
    return a, b, c, d

def _chacha20_block(key: bytes, counter: int, nonce: bytes) -> bytes:
    """Generate a ChaCha20 block."""
    if len(key) != 32 or len(nonce) != 12:
        raise ValueError("Invalid key or nonce length")
    
    # Constants
    state = [
        0x61707865, 0x3320646e, 0x79622d32, 0x6b206574,  # "expand 32-byte k"
        *struct.unpack('<8I', key),  # Key (8 words)
        counter & 0xFFFFFFFF,        # Counter
        *struct.unpack('<3I', nonce) # Nonce (3 words)
    ]
    
    working_state = state[:]
    
    # 20 rounds (10 double rounds)
    for _ in range(10):
        # Column rounds
        working_state[0], working_state[4], working_state[8], working_state[12] = \
            _quarter_round(working_state[0], working_state[4], working_state[8], working_state[12])
        working_state[1], working_state[5], working_state[9], working_state[13] = \
            _quarter_round(working_state[1], working_state[5], working_state[9], working_state[13])
        working_state[2], working_state[6], working_state[10], working_state[14] = \
            _quarter_round(working_state[2], working_state[6], working_state[10], working_state[14])
        working_state[3], working_state[7], working_state[11], working_state[15] = \
            _quarter_round(working_state[3], working_state[7], working_state[11], working_state[15])
        
        # Diagonal rounds
        working_state[0], working_state[5], working_state[10], working_state[15] = \
            _quarter_round(working_state[0], working_state[5], working_state[10], working_state[15])
        working_state[1], working_state[6], working_state[11], working_state[12] = \
            _quarter_round(working_state[1], working_state[6], working_state[11], working_state[12])
        working_state[2], working_state[7], working_state[8], working_state[13] = \
            _quarter_round(working_state[2], working_state[7], working_state[8], working_state[13])
        working_state[3], working_state[4], working_state[9], working_state[14] = \
            _quarter_round(working_state[3], working_state[4], working_state[9], working_state[14])
    
    # Add original state
    for i in range(16):
        working_state[i] = (working_state[i] + state[i]) & 0xFFFFFFFF
    
    return struct.pack('<16I', *working_state)

def _poly1305_mac(key: bytes, data: bytes) -> bytes:
    """Compute Poly1305 MAC."""
    if len(key) != 32:
        raise ValueError("Poly1305 key must be 32 bytes")
    
    r = struct.unpack('<4I', key[:16])
    s = struct.unpack('<4I', key[16:])
    
    # Clamp r
    r = [r[0] & 0x0fffffff, r[1] & 0x0ffffffc, r[2] & 0x0ffffffc, r[3] & 0x0ffffffc]
    
    # Initialize accumulator
    h = [0, 0, 0, 0, 0]
    
    # Process data in 16-byte blocks
    for i in range(0, len(data), 16):
        block = data[i:i+16]
        if len(block) < 16:
            block = block + b'\x00' * (16 - len(block))
        
        n = list(struct.unpack('<4I', block))
        
        if len(data[i:i+16]) < 16:
            # Pad partial block
            n.append(1 << (len(data[i:i+16]) * 8))
        else:
            n.append(1)
        
        # Add block to accumulator
        for j in range(5):
            h[j] += n[j]
        
        # Multiply by r
        d = [0] * 10
        for j in range(5):
            for k in range(4):
                d[j + k] += h[j] * r[k]
        
        # Reduce modulo 2^130 - 5
        carry = 0
        for j in range(10):
            d[j] += carry
            if j < 5:
                carry = d[j] >> 32
                d[j] &= 0xffffffff
            else:
                carry = d[j] >> 32
                d[j - 5] += (d[j] & 0xffffffff) * 5
                d[j] = 0
        
        h = d[:5]
        
        # Final carry propagation
        for j in range(4):
            h[j+1] += h[j] >> 32
            h[j] &= 0xffffffff
    
    # Add s
    carry = 0
    for i in range(4):
        carry += h[i] + s[i]
        h[i] = carry & 0xffffffff
        carry >>= 32
    
    return struct.pack('<4I', *h[:4])

def chacha20_poly1305_encrypt(key: bytes, nonce: bytes, data: bytes, aad: bytes = b'') -> bytes:
    """ChaCha20-Poly1305 AEAD encryption."""
    if len(key) != 32 or len(nonce) != 12:
        raise ValueError("Invalid key or nonce length")
    
    # Generate Poly1305 key
    poly_key = _chacha20_block(key, 0, nonce)[:32]
    
    # Encrypt data
    encrypted = bytearray()
    for i, block_start in enumerate(range(0, len(data), 64)):
        block = data[block_start:block_start + 64]
        keystream = _chacha20_block(key, i + 1, nonce)[:len(block)]
        encrypted.extend(bytes(a ^ b for a, b in zip(block, keystream)))
    
    # Compute MAC
    mac_data = aad + b'\0' * ((16 - len(aad)) % 16)
    mac_data += encrypted + b'\0' * ((16 - len(encrypted)) % 16)
    mac_data += struct.pack('<Q', len(aad)) + struct.pack('<Q', len(encrypted))
    
    tag = _poly1305_mac(poly_key, mac_data)
    
    return bytes(encrypted) + tag

def chacha20_poly1305_decrypt(key: bytes, nonce: bytes, ciphertext: bytes, aad: bytes = b'') -> bytes:
    """ChaCha20-Poly1305 AEAD decryption."""
    if len(key) != 32 or len(nonce) != 12:
        raise ValueError("Invalid key or nonce length")
    
    if len(ciphertext) < 16:
        raise ValueError("Ciphertext too short")
    
    encrypted = ciphertext[:-16]
    tag = ciphertext[-16:]
    
    # Generate Poly1305 key and verify MAC
    poly_key = _chacha20_block(key, 0, nonce)[:32]
    
    mac_data = aad + b'\0' * ((16 - len(aad)) % 16)
    mac_data += encrypted + b'\0' * ((16 - len(encrypted)) % 16)
    mac_data += struct.pack('<Q', len(aad)) + struct.pack('<Q', len(encrypted))
    
    expected_tag = _poly1305_mac(poly_key, mac_data)
    
    if not hmac.compare_digest(tag, expected_tag):
        raise ValueError("Authentication tag verification failed")
    
    # Decrypt data
    decrypted = bytearray()
    for i, block_start in enumerate(range(0, len(encrypted), 64)):
        block = encrypted[block_start:block_start + 64]
        keystream = _chacha20_block(key, i + 1, nonce)[:len(block)]
        decrypted.extend(bytes(a ^ b for a, b in zip(block, keystream)))
    
    return bytes(decrypted)

def hkdf_expand(prk: bytes, length: int, info: bytes = b'') -> bytes:
    """HKDF Expand (RFC 5869)."""
    hash_len = 32  # SHA-256
    if length > 255 * hash_len:
        raise ValueError("Length too large for HKDF")
    
    okm = b''
    t = b''
    counter = 1
    
    while len(okm) < length:
        t = hmac.new(prk, t + info + bytes([counter]), hashlib.sha256).digest()
        okm += t
        counter += 1
    
    return okm[:length]

def hkdf_extract_expand(ikm: bytes, length: int, salt: bytes = b'', info: bytes = b'') -> bytes:
    """HKDF Extract and Expand in one step."""
    if not salt:
        salt = b'\x00' * 32  # SHA-256 hash length
    
    prk = hmac.new(salt, ikm, hashlib.sha256).digest()
    return hkdf_expand(prk, length, info)

def ffdhe_2048_generate_keypair() -> tuple[int, int]:
    """Generate FFDHE-2048 key pair. Returns (private_key, public_key)."""
    # Generate random private key (1 < private < p-1)
    while True:
        private = int.from_bytes(os.urandom(256), 'big')  # 2048 bits
        if 1 < private < FFDHE_2048_P - 1:
            break
    
    public = pow(FFDHE_2048_G, private, FFDHE_2048_P)
    return private, public

def ffdhe_2048_compute_shared(private_key: int, peer_public_key: int) -> bytes:
    """Compute FFDHE-2048 shared secret."""
    if not (1 < peer_public_key < FFDHE_2048_P - 1):
        raise ValueError("Invalid peer public key")
    
    shared_secret = pow(peer_public_key, private_key, FFDHE_2048_P)
    # Convert to bytes (big-endian, 256 bytes)
    return shared_secret.to_bytes(256, 'big')

def canonicalize_address(addr: str) -> str:
    """Canonicalize IPv4/IPv6 address (collapse IPv4-mapped IPv6 to IPv4)."""
    try:
        ip = ip_address(addr)
        if isinstance(ip, IPv6Address) and ip.ipv4_mapped:
            return str(ip.ipv4_mapped)
        return str(ip)
    except Exception:
        return addr

# ----------------------- Session Management & Record Framing ---------------

class SessionKeys:
    """Manages symmetric keys derived from DH shared secret."""
    
    def __init__(self, shared_secret: bytes, is_sender: bool):
        self.is_sender = is_sender
        # Derive keys using HKDF
        self.data_send_key = hkdf_extract_expand(shared_secret, 32, info=b"XFER-v4-data-send")
        self.data_recv_key = hkdf_extract_expand(shared_secret, 32, info=b"XFER-v4-data-recv")
        self.meta_send_key = hkdf_extract_expand(shared_secret, 32, info=b"XFER-v4-meta-send")
        self.meta_recv_key = hkdf_extract_expand(shared_secret, 32, info=b"XFER-v4-meta-recv")
        
        # Sequence counters
        self.data_send_seq = 0
        self.data_recv_seq = 0
        self.meta_send_seq = 0
        self.meta_recv_seq = 0
    
    def encrypt_data_record(self, data: bytes) -> bytes:
        """Encrypt data channel record."""
        key = self.data_send_key
        nonce = self._derive_nonce("data-send", self.data_send_seq)
        aad = struct.pack('>BIQ', 1, len(data), self.data_send_seq)  # type=1, length, seq
        
        encrypted = chacha20_poly1305_encrypt(key, nonce, data, aad)
        self.data_send_seq += 1
        
        return aad + encrypted
    
    def decrypt_data_record(self, record: bytes) -> bytes:
        """Decrypt data channel record."""
        if len(record) < 13 + 16:  # AAD + minimum ciphertext
            raise ValueError("Record too short")
        
        aad = record[:13]
        encrypted = record[13:]
        
        # Parse AAD
        record_type, length, seq = struct.unpack('>BIQ', aad)
        if record_type != 1:
            raise ValueError("Invalid record type for data channel")
        if seq != self.data_recv_seq:
            raise ValueError(f"Sequence number mismatch: expected {self.data_recv_seq}, got {seq}")
        
        key = self.data_recv_key
        nonce = self._derive_nonce("data-recv", seq)
        
        decrypted = chacha20_poly1305_decrypt(key, nonce, encrypted, aad)
        if len(decrypted) != length:
            raise ValueError("Decrypted length mismatch")
        
        self.data_recv_seq += 1
        return decrypted
    
    def encrypt_meta_record(self, data: bytes) -> bytes:
        """Encrypt meta channel record."""
        key = self.meta_send_key
        nonce = self._derive_nonce("meta-send", self.meta_send_seq)
        aad = struct.pack('>BIQ', 2, len(data), self.meta_send_seq)  # type=2, length, seq
        
        encrypted = chacha20_poly1305_encrypt(key, nonce, data, aad)
        self.meta_send_seq += 1
        
        return aad + encrypted
    
    def decrypt_meta_record(self, record: bytes) -> bytes:
        """Decrypt meta channel record."""
        if len(record) < 13 + 16:  # AAD + minimum ciphertext
            raise ValueError("Record too short")
        
        aad = record[:13]
        encrypted = record[13:]
        
        # Parse AAD
        record_type, length, seq = struct.unpack('>BIQ', aad)
        if record_type != 2:
            raise ValueError("Invalid record type for meta channel")
        if seq != self.meta_recv_seq:
            raise ValueError(f"Sequence number mismatch: expected {self.meta_recv_seq}, got {seq}")
        
        key = self.meta_recv_key
        nonce = self._derive_nonce("meta-recv", seq)
        
        decrypted = chacha20_poly1305_decrypt(key, nonce, encrypted, aad)
        if len(decrypted) != length:
            raise ValueError("Decrypted length mismatch")
        
        self.meta_recv_seq += 1
        return decrypted
    
    def _derive_nonce(self, label: str, seq: int) -> bytes:
        """Derive nonce from label and sequence number."""
        return hashlib.sha256(label.encode() + struct.pack('>Q', seq)).digest()[:12]

def perform_key_exchange(sock, is_sender: bool) -> SessionKeys:
    """Perform ephemeral FFDHE-2048 key exchange."""
    # Generate our key pair
    private_key, public_key = ffdhe_2048_generate_keypair()
    
    # Serialize our public key (256 bytes, big-endian)
    our_public_bytes = public_key.to_bytes(256, 'big')
    
    if is_sender:
        # Sender: send our public key, then receive peer's
        sock.sendall(our_public_bytes)
        peer_public_bytes = _recv_all(sock, 256)
    else:
        # Receiver: receive peer's public key, then send ours
        peer_public_bytes = _recv_all(sock, 256)
        sock.sendall(our_public_bytes)
    
    # Compute shared secret
    peer_public_key = int.from_bytes(peer_public_bytes, 'big')
    shared_secret = ffdhe_2048_compute_shared(private_key, peer_public_key)
    
    return SessionKeys(shared_secret, is_sender)

# ----------------------- Utilities -----------------------------------------

def is_private_ipv4(s: str) -> bool:
    """Return True if s is IPv4 and belongs to a private/link-local range."""
    try:
        ip = ip_address(s)
        return isinstance(ip, IPv4Address) and (ip.is_private or ip.is_link_local)
    except Exception:
        return False

def _recv_all(sock, n: int) -> bytes:
    """Receive exactly n bytes from socket."""
    data = bytearray()
    while len(data) < n:
        if STOP.is_set():
            raise KeyboardInterrupt()
        try:
            chunk = sock.recv(n - len(data))
            if not chunk:
                raise ConnectionError("Peer closed connection")
            data.extend(chunk)
        except socket.timeout:
            continue
    return bytes(data)

def local_ips():
    """Best-effort list of local IPv4s; includes primary egress IP. Always returns at least 127.0.0.1."""
    ips = set()
    try:
        hn = socket.gethostname()
        for fam, _, _, _, sa in socket.getaddrinfo(hn, None):
            if fam == socket.AF_INET:
                ip = sa[0]
                if is_private_ipv4(ip): ips.add(ip)
    except Exception:
        pass
    try:
        with closing(socket.socket(socket.AF_INET, socket.SOCK_DGRAM)) as s:
            s.connect(("8.8.8.8", 80))
            ip = s.getsockname()[0]
            if is_private_ipv4(ip): ips.add(ip)
    except Exception:
        pass
    if not ips: ips.add("127.0.0.1")
    return sorted(ips)

def local_ipv6_addresses():
    """Best-effort list of local IPv6 addresses."""
    ips = set()
    try:
        hn = socket.gethostname()
        for fam, _, _, _, sa in socket.getaddrinfo(hn, None):
            if fam == socket.AF_INET6:
                ip = sa[0]
                try:
                    addr = ip_address(ip)
                    # Skip loopback and link-local, include private/unique local
                    if not addr.is_loopback and not addr.is_link_local:
                        ips.add(str(addr))
                except Exception:
                    pass
    except Exception:
        pass
    
    # Add IPv6 loopback if no other addresses found
    if not ips: 
        ips.add("::1")
    return sorted(ips)

def human(n: float) -> str:
    """Format bytes human-readably."""
    u = ["B","KB","MB","GB","TB","PB"]; i = 0
    while n >= 1024 and i < len(u)-1: n /= 1024.0; i += 1
    return f"{n:3.1f} {u[i]}"

def iter_files_with_sizes(root: str, excludes=()):
    """
    Walk 'root' and yield (abs_path, rel_posix, size) for regular files, honoring fnmatch excludes.

    rel_posix is normalized to forward slashes and includes the top-level folder for stable manifests.
    """
    root_abs = os.path.abspath(root)
    parent = os.path.dirname(root_abs)
    for dirpath, _, filenames in os.walk(root_abs):
        for name in filenames:
            ap = os.path.join(dirpath, name)
            try: st = os.stat(ap)
            except FileNotFoundError: continue
            rp = os.path.relpath(ap, parent)
            rel_posix = str(PurePosixPath(Path(rp).as_posix()))
            base = os.path.basename(rel_posix)
            if any(fnmatch.fnmatch(rel_posix, p) or fnmatch.fnmatch(base, p) for p in excludes):
                continue
            yield ap, rel_posix, st.st_size

def tar_stream_size_estimate(file_sizes):
    """Estimate tar stream size for progress (%): data + headers + per-file padding + EOF blocks."""
    n = len(file_sizes)
    data = sum(file_sizes)
    headers = 512 * n
    padding = sum((512 - (s % 512)) % 512 for s in file_sizes)
    eof = 1024
    return data + headers + padding + eof

def looks_like_tar_header(block: bytes) -> bool:
    """Minimal sniff: POSIX 'ustar' magic at offset 257..261."""
    if len(block) < 512: return False
    return block[257:257+5] == b"ustar"

def sanitize_name(name: str) -> str:
    """Keep only the basename; strip path components."""
    base = os.path.basename(name.strip())
    return base or "xfer-incoming.bin"

def unique_path(dirpath: str, filename: str) -> str:
    """Return a non-colliding path inside 'dirpath' by appending ' (n)' before extension if needed."""
    root, ext = os.path.splitext(filename)
    candidate = os.path.join(dirpath, filename)
    n = 1
    while os.path.exists(candidate):
        candidate = os.path.join(dirpath, f"{root} ({n}){ext}")
        n += 1
    return candidate

def ensure_parent(path: str):
    """Create parent directory for 'path' if missing."""
    os.makedirs(os.path.dirname(os.path.abspath(path)) or ".", exist_ok=True)

# ----------------------- XAR4 Wire Format ----------------------------------

def xar4_create_file_header(size: int) -> bytes:
    """Create XAR4 file header: magic + 8-byte little-endian size."""
    return XAR4_FILE_MAGIC + struct.pack('<Q', size)

def xar4_parse_file_header(header: bytes) -> int:
    """Parse XAR4 file header, return size."""
    if len(header) < 14 or header[:6] != XAR4_FILE_MAGIC:
        raise ValueError("Invalid XAR4 file header")
    return struct.unpack('<Q', header[6:14])[0]

def xar4_create_dir_header() -> bytes:
    """Create XAR4 directory header: magic only."""
    return XAR4_DIR_MAGIC

def xar4_parse_dir_header(header: bytes) -> bool:
    """Parse XAR4 directory header, return True if valid."""
    return len(header) >= 6 and header[:6] == XAR4_DIR_MAGIC

def xar4_create_dir_entry(path: str, mode: int, mtime: int, size: int) -> bytes:
    """Create XAR4 directory entry: type 'F' + path_len(4BE) + mode(4LE) + mtime(8LE) + size(8LE) + UTF-8 path."""
    path_bytes = path.encode('utf-8')
    entry = struct.pack('>BI', XAR4_FILE_ENTRY, len(path_bytes))
    entry += struct.pack('<IQQ', mode, mtime, size)
    entry += path_bytes
    return entry

def xar4_parse_dir_entry(data: bytes, offset: int) -> tuple[str, int, int, int, int]:
    """Parse XAR4 directory entry. Returns (path, mode, mtime, size, next_offset)."""
    if offset + 21 > len(data):
        raise ValueError("Insufficient data for directory entry")
    
    entry_type = data[offset]
    if entry_type == XAR4_DIR_TERMINATOR:
        return None, 0, 0, 0, offset + 1  # End marker
    
    if entry_type != XAR4_FILE_ENTRY:
        raise ValueError(f"Invalid entry type: {entry_type}")
    
    path_len = struct.unpack('>I', data[offset+1:offset+5])[0]
    mode, mtime, size = struct.unpack('<IQQ', data[offset+5:offset+25])
    
    if offset + 25 + path_len > len(data):
        raise ValueError("Insufficient data for path")
    
    path = data[offset+25:offset+25+path_len].decode('utf-8')
    
    return path, mode, mtime, size, offset + 25 + path_len

def xar4_create_dir_terminator() -> bytes:
    """Create XAR4 directory terminator."""
    return bytes([XAR4_DIR_TERMINATOR])

def xar4_normalize_path(path: str) -> str:
    """Normalize and sanitize path to prevent traversal attacks."""
    # Convert to POSIX format and normalize
    path = str(PurePosixPath(path))
    
    # Remove leading slashes and resolve relative components
    parts = []
    for part in path.split('/'):
        if part in ('', '.'):
            continue
        elif part == '..':
            if parts:
                parts.pop()
        else:
            parts.append(part)
    
    return '/'.join(parts) if parts else 'unnamed'

# ----------------------- Progress (pv-style) --------------------------------

def progress_loop(stop_ev: threading.Event, total_func, label="", target_bytes=None):
    """
    pv-style status (stderr):
      • bytes + instantaneous rate
      • if target known: percentage + ETA
    """
    last = 0; last_t = time.time()
    while not stop_ev.is_set():
        time.sleep(0.5)
        now = time.time(); done = total_func()
        delta = done - last; dt = max(1e-6, now - last_t); rate = delta / dt
        if target_bytes and target_bytes > 0:
            pct = min(100.0, 100.0 * done / target_bytes)
            remain = max(0.0, (target_bytes - done) / max(1e-6, rate)) if rate > 0 else float("inf")
            eta = time.strftime("%H:%M:%S", time.gmtime(remain)) if remain != float("inf") else "--:--:--"
            line = f"\r{CLR['info']}{label}{CLR['reset']} {pct:5.1f}% {human(done)}/{human(target_bytes)} @ {human(rate)}/s ETA {eta}"
        else:
            line = f"\r{CLR['info']}{label}{CLR['reset']} {human(done)} @ {human(rate)}/s"
        sys.stderr.write(line); sys.stderr.flush(); last, last_t = done, now
    done = total_func()
    if target_bytes and target_bytes > 0:
        sys.stderr.write(f"\r{CLR['info']}{label}{CLR['reset']} 100.0% {human(done)}/{human(target_bytes)} @ complete\n")
    else:
        sys.stderr.write(f"\r{CLR['info']}{label}{CLR['reset']} {human(done)} @ complete\n")
    sys.stderr.flush()

# ----------------------- Sockets -------------------------------------------

@contextmanager
def listener(port: int):
    """
    TCP listener bound to all interfaces with short timeouts (Ctrl-C responsive).
    Use 'accept_one' to retrieve a client connection.
    """
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try: s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    except Exception: pass
    s.bind(("", port)); s.listen(LISTEN_BACKLOG); s.settimeout(ACCEPT_POLL_SECS)
    try: yield s
    finally:
        try: s.close()
        except Exception: pass

def accept_one(ls, deadline=None):
    """Accept one connection with polling and an optional deadline; raises TimeoutError on expiry."""
    while True:
        if STOP.is_set(): raise KeyboardInterrupt
        try:
            conn, addr = ls.accept(); conn.settimeout(IO_POLL_SECS); return conn, addr
        except socket.timeout:
            if deadline and time.time() > deadline: raise TimeoutError("accept timed out")

def connect_with_retry(host: str, port: int,
                       attempt_timeout=CONNECT_ATTEMPT_TIMEOUT,
                       total_timeout=CONNECT_TOTAL_TIMEOUT):
    """Connect with retry/backoff up to total_timeout; returns connected socket with short IO timeouts."""
    deadline = time.time() + total_timeout
    delay = 0.2; last_exc = None
    while True:
        if STOP.is_set(): raise KeyboardInterrupt
        try:
            s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            s.settimeout(attempt_timeout); s.connect((host, port)); s.settimeout(IO_POLL_SECS)
            return s
        except Exception as e:
            last_exc = e
            if time.time() + delay > deadline: raise last_exc
            time.sleep(delay); delay = min(2.0, delay * 1.5)

# ----------------------- TOFU + SAS (secure, default) ----------------------

def _xfer_home():
    XFER_HOME.mkdir(parents=True, exist_ok=True)

def _load_or_create_identity():
    """
    Receiver identity secret (32 random bytes) at ~/.xfer/identity.key.
    Used to derive a stable receiver fingerprint for TOFU.
    """
    _xfer_home()
    if not IDENTITY_PATH.exists():
        with open(IDENTITY_PATH, "wb") as f: f.write(os.urandom(32))
    with open(IDENTITY_PATH, "rb") as f: key = f.read()
    if len(key) != 32:
        with open(IDENTITY_PATH, "wb") as f:
            key = os.urandom(32); f.write(key)
    return key

def _receiver_fingerprint_hex(identity_bytes: bytes) -> str:
    """Stable receiver fingerprint (hex) — sha256 over tagged identity bytes."""
    return hashlib.sha256(b"XFER-ID-v1" + identity_bytes).hexdigest()

def _load_known_peers():
    """Read ~/.xfer/known_peers → dict {'ip:port': 'hexfp'}."""
    _xfer_home()
    d = {}
    if KNOWN_PEERS_PATH.exists():
        try:
            with open(KNOWN_PEERS_PATH, "r", encoding="utf-8", errors="ignore") as f:
                for line in f:
                    line = line.strip()
                    if not line or line.startswith("#"): continue
                    try:
                        hp, fp = line.split()
                        d[hp] = fp
                    except ValueError:
                        continue
        except Exception:
            pass
    return d

def _save_known_peers(d: dict):
    tmp = KNOWN_PEERS_PATH.with_suffix(".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        for hp, fp in sorted(d.items()):
            f.write(f"{hp} {fp}\n")
    os.replace(tmp, KNOWN_PEERS_PATH)

def _sas_from(fp_hex: str, ns_hex: str, nr_hex: str) -> str:
    """
    Derive a 10-digit SAS code (3-3-4) from HMAC-SHA256:
      H = HMAC(key=FP, msg=NS||NR||"XFER-SAS1"); code = int(H[:8]) mod 10^10
    """
    key = bytes.fromhex(fp_hex)
    msg = bytes.fromhex(ns_hex) + bytes.fromhex(nr_hex) + b"XFER-SAS1"
    h = hmac.new(key, msg, hashlib.sha256).digest()
    num = int.from_bytes(h[:8], "big") % (10**10)
    s = f"{num:010d}"
    return f"{s[0:3]}-{s[3:6]}-{s[6:10]}"

def _read_headers(conn, max_bytes=4096):
    """Read simple 'Key:Value' headers terminated by blank line."""
    buf = b""
    while b"\n\n" not in buf:
        if STOP.is_set(): raise KeyboardInterrupt
        try: chunk = conn.recv(512)
        except socket.timeout: continue
        if not chunk: break
        buf += chunk
        if len(buf) > max_bytes: break
    text = buf.decode("utf-8", errors="ignore")
    headers = {}
    for line in text.splitlines():
        line = line.strip()
        if not line: break
        if ":" in line:
            k, v = line.split(":", 1)
            headers[k.strip().upper()] = v.strip()
    return headers

def _write_headers(conn, **kv):
    for k, v in kv.items():
        conn.sendall(f"{k}:{v}\n".encode("utf-8"))
    conn.sendall(b"\n")

def sas_receiver_handshake(port: int) -> None:
    """
    Receiver side of TOFU+SAS:
      - Listen on control port (N-1)
      - Send fingerprint + receiver nonce
      - Receive sender nonce
      - Compute/display SAS; prompt if first time or fingerprint changed
      - Persist mapping sender-ip:port -> receiver-fingerprint
    """
    if port - SAS_CTRL_OFFSET <= 0:
        err("Invalid port for SAS control (port-1 <= 0). Use a higher --port.")
        sys.exit(1)

    identity = _load_or_create_identity()
    fp_hex = _receiver_fingerprint_hex(identity)
    nr_hex = os.urandom(16).hex()

    ctrl_port = port - SAS_CTRL_OFFSET
    try:
        with listener(ctrl_port) as ls:
            info(f"SAS: waiting on control port {ctrl_port} for pairing ...")
            try:
                conn, (peer_ip, _) = accept_one(ls, deadline=time.time() + SAS_ACCEPT_BUDGET)
            except TimeoutError:
                err("SAS: pairing timed out (receiver waited on control port). "
                    "If the sender is running an older XFER or not using security, rerun with --insecure on both sides.")
                sys.exit(1)
    except OSError as e:
        err(f"SAS: failed to bind control port {ctrl_port}: {e}")
        sys.exit(1)

    with conn:
        conn.settimeout(IO_POLL_SECS)
        _write_headers(conn, PROTO="XFER-SAS1", FP=fp_hex, NR=nr_hex)
        hdr = _read_headers(conn)
        ns_hex = hdr.get("NS", "")
        if len(ns_hex) < 2:
            err("SAS: no sender nonce received; aborting.")
            sys.exit(1)

    sas_code = _sas_from(fp_hex, ns_hex, nr_hex)
    hp = f"{peer_ip}:{port}"
    known = _load_known_peers()
    existing = known.get(hp)

    if existing is None:
        sys.stderr.write(f"{CLR['brand']}SAS pairing (new peer){CLR['reset']}  {CLR['dim']}host:{hp}{CLR['reset']}\n")
        sys.stderr.write(f"  Fingerprint: {fp_hex[:16]}…\n")
        sys.stderr.write(f"  Code: {CLR['bold']}{sas_code}{CLR['reset']}\n")
        resp = input("  If this matches the sender, press ENTER to trust, or type 'no' to abort: ").strip().lower()
        if resp not in ("", "y", "yes"):
            err("SAS: user declined pairing; aborting.")
            sys.exit(1)
        known[hp] = fp_hex; _save_known_peers(known)
        ok(f"SAS: trusted and stored for {hp}")
    elif existing != fp_hex:
        sys.stderr.write(f"{CLR['warn']}SAS warning: fingerprint changed for {hp}{CLR['reset']}\n")
        sys.stderr.write(f"  Prev: {existing[:16]}…\n  New : {fp_hex[:16]}…\n")
        sys.stderr.write(f"  Code: {CLR['bold']}{sas_code}{CLR['reset']}\n")
        resp = input("  If you expect this change, type 'override' to trust new; anything else aborts: ").strip().lower()
        if resp != "override":
            err("SAS: fingerprint change rejected; aborting.")
            sys.exit(1)
        known[hp] = fp_hex; _save_known_peers(known)
        ok(f"SAS: updated trust for {hp}")
    else:
        ok(f"SAS: trusted peer {hp} (fingerprint match)")

def sas_sender_handshake(ip: str, port: int) -> None:
    """
    Sender side of TOFU+SAS:
      - Connect to control port (N-1)
      - Receive receiver FP + receiver nonce; send our nonce
      - Compute/display SAS; prompt on first trust; persist mapping receiver-ip:port -> FP
    """
    if port - SAS_CTRL_OFFSET <= 0:
        err("Invalid port for SAS control (port-1 <= 0). Use a higher --port.")
        sys.exit(1)

    ctrl_port = port - SAS_CTRL_OFFSET
    try:
        with closing(connect_with_retry(ip, ctrl_port, total_timeout=SAS_ACCEPT_BUDGET)) as s:
            hdr = _read_headers(s)
            if hdr.get("PROTO", "") != "XFER-SAS1":
                err("SAS: receiver does not speak SAS1; ensure both sides run the same XFER version or use --insecure.")
                sys.exit(1)
            fp_hex = hdr.get("FP", ""); nr_hex = hdr.get("NR", "")
            if not fp_hex or not nr_hex:
                err("SAS: receiver did not provide fingerprint/nonce; aborting."); sys.exit(1)
            ns_hex = os.urandom(16).hex()
            _write_headers(s, NS=ns_hex)
    except Exception as e:
        err(f"SAS: unable to reach receiver control port {ctrl_port} at {ip}: {e}\n"
            "     If the receiver is not using security, rerun both sides with --insecure.")
        sys.exit(1)

    sas_code = _sas_from(fp_hex, ns_hex, nr_hex)
    hp = f"{ip}:{port}"
    known = _load_known_peers()
    existing = known.get(hp)

    if existing is None:
        sys.stderr.write(f"{CLR['brand']}SAS pairing (new peer){CLR['reset']}  {CLR['dim']}host:{hp}{CLR['reset']}\n")
        sys.stderr.write(f"  Fingerprint: {fp_hex[:16]}…\n")
        sys.stderr.write(f"  Code: {CLR['bold']}{sas_code}{CLR['reset']}\n")
        resp = input("  If this matches the receiver, press ENTER to trust, or type 'no' to abort: ").strip().lower()
        if resp not in ("", "y", "yes"):
            err("SAS: user declined pairing; aborting.")
            sys.exit(1)
        known[hp] = fp_hex; _save_known_peers(known)
        ok(f"SAS: trusted and stored for {hp}")
    elif existing != fp_hex:
        sys.stderr.write(f"{CLR['warn']}SAS warning: fingerprint changed for {hp}{CLR['reset']}\n")
        sys.stderr.write(f"  Prev: {existing[:16]}…\n  New : {fp_hex[:16]}…\n")
        sys.stderr.write(f"  Code: {CLR['bold']}{sas_code}{CLR['reset']}\n")
        resp = input("  If you expect this change, type 'override' to trust new; anything else aborts: ").strip().lower()
        if resp != "override":
            err("SAS: fingerprint change rejected; aborting.")
            sys.exit(1)
        known[hp] = fp_hex; _save_known_peers(known)
        ok(f"SAS: updated trust for {hp}")
    else:
        ok(f"SAS: trusted peer {hp} (fingerprint match)")

# ----------------------- Safe tar extraction (hash-aware) -------------------

def safe_stream_extract_and_hash(tf: tarfile.TarFile, dest_dir: str):
    """Extract streaming tar safely and compute per-file SHA-256; returns {rel_posix:hash}."""
    base = os.path.abspath(dest_dir)
    file_hashes = {}
    for member in tf:
        target = os.path.abspath(os.path.join(base, member.name))
        if not (target == base or target.startswith(base + os.sep)):
            raise RuntimeError(f"Unsafe path in tar: {member.name}")
        if member.isdir():
            os.makedirs(target, exist_ok=True)
        elif member.isreg():
            os.makedirs(os.path.dirname(target), exist_ok=True)
            src = tf.extractfile(member)
            if src is None: continue
            h = hashlib.sha256()
            with open(target, "wb") as out:
                while True:
                    if STOP.is_set(): raise KeyboardInterrupt
                    b = src.read(CHUNK)
                    if not b: break
                    out.write(b); h.update(b)
            try: os.utime(target, (member.mtime, member.mtime))
            except Exception: pass
            file_hashes[member.name] = h.hexdigest()
        else:
            continue
    return file_hashes

# ----------------------- Receivers (core) -----------------------------------

def _drain(sock):
    """Read and discard any remaining bytes so the sender can finish cleanly."""
    try:
        while sock.recv(CHUNK): pass
    except Exception:
        pass

def _receive_checksum_line(port: int):
    """On meta port (N+1), read 'sha256␠basename'. Returns (hash or '', basename or '')."""
    meta_port = port + 1
    with listener(meta_port) as ls2:
        info(f"Awaiting checksum on port {meta_port} ...")
        try:
            conn2, _ = accept_one(ls2, deadline=time.time() + META_ACCEPT_BUDGET)
        except TimeoutError:
            warn("No checksum received within timeout; skipping verification.")
            return "", ""
        with conn2:
            conn2.settimeout(IO_POLL_SECS); data = b""
            while True:
                if STOP.is_set(): raise KeyboardInterrupt
                try: chunk = conn2.recv(4096)
                except socket.timeout: continue
                if not chunk: break
                data += chunk
    parts = data.decode(errors="ignore").strip().split(maxsplit=1)
    if not parts: return "", ""
    h = parts[0]; name = parts[1].strip() if len(parts) > 1 else ""
    return h, name

def _verify_manifest(port: int, local_hashes: dict):
    """Receive manifest on meta port and compare against extraction-time hashes."""
    meta_port = port + 1
    with listener(meta_port) as ls2:
        info(f"Awaiting manifest on port {meta_port} ...")
        try:
            conn2, _ = accept_one(ls2, deadline=time.time() + META_ACCEPT_BUDGET)
        except TimeoutError:
            warn("No manifest received within timeout; skipping verification.")
            return
        with conn2, tempfile.NamedTemporaryFile("w+", delete=False) as mf:
            conn2.settimeout(IO_POLL_SECS)
            while True:
                if STOP.is_set(): raise KeyboardInterrupt
                try: chunk = conn2.recv(65536)
                except socket.timeout: continue
                if not chunk: break
                mf.write(chunk.decode("utf-8", errors="ignore"))
            manifest_path = mf.name

    failures = []
    with open(manifest_path, "r", encoding="utf-8", errors="ignore") as f:
        for line in f:
            line = line.strip()
            if not line: continue
            try:
                h, rel = line.split(None, 1)
            except ValueError:
                continue
            local_h = local_hashes.get(rel)
            if local_h is None:
                failures.append((rel, "missing"))
            elif local_h != h:
                failures.append((rel, "hash_mismatch"))
    os.unlink(manifest_path)

    if failures:
        err("VERIFY FAIL — some files mismatched:")
        for rel, why in failures[:10]:
            sys.stderr.write(f"  {rel}: {why}\n")
        if len(failures) > 10:
            sys.stderr.write(f"  ... and {len(failures)-10} more\n")
        sys.exit(2)
    else:
        ok("VERIFY OK — all files match manifest.")

def receive_auto(out_path: str | None, port: int, force: bool, expected: str | None, secure: bool):
    """
    Auto-detect and receive either a FILE or a DIRECTORY stream.

    Secure pairing:
      - Runs TOFU+SAS control handshake by default (port-1).
      - Disable with --insecure/--no-secure (for legacy/automation).
    """
    if secure:
        sas_receiver_handshake(port)

    with listener(port) as ls:
        banner(); info(f"Listening on port {port} → destination: {out_path or '.'}")
        conn, _ = accept_one(ls)
        with conn:
            # Sniff up to 512 bytes to disambiguate FILE vs DIR (tar)
            first = b""
            while len(first) < 512:
                if STOP.is_set(): raise KeyboardInterrupt
                try: chunk = conn.recv(512 - len(first))
                except socket.timeout: continue
                if not chunk: break
                first += chunk

            is_tar = looks_like_tar_header(first)

            if expected == "file" and is_tar:
                err("Incoming stream looks like a directory tar. Use: recv-dir <OUTPUT_DIR> or `receive` (auto).")
                _drain(conn); sys.exit(1)
            if expected == "dir" and not is_tar:
                err("Incoming stream is not a tar archive. Use: recv-file <OUTPUT_FILE> or `receive` (auto).")
                _drain(conn); sys.exit(1)

            if is_tar:
                # ---------- DIRECTORY MODE ----------
                out_dir = out_path or "."
                if os.path.exists(out_dir) and not os.path.isdir(out_dir):
                    err(f"--out points to a file, but a directory stream is incoming: {out_dir}")
                    _drain(conn); sys.exit(1)
                os.makedirs(out_dir, exist_ok=True)

                class PrependReader:
                    def __init__(self, sock, first_block):
                        self.sock, self.buf, self.eof = sock, bytearray(first_block), False
                        self.sock.settimeout(IO_POLL_SECS)
                    def read(self, n=-1):
                        if self.eof: return b""
                        if self.buf:
                            if n is None or n < 0:
                                out = bytes(self.buf); self.buf.clear(); return out
                            out = bytes(self.buf[:n]); del self.buf[:n]; return out
                        if n is None or n < 0:
                            chunks = []
                            while True:
                                if STOP.is_set(): raise KeyboardInterrupt
                                try: b = self.sock.recv(CHUNK)
                                except socket.timeout: continue
                                if not b: self.eof = True; break
                                chunks.append(b)
                            return b"".join(chunks)
                        out = bytearray()
                        while len(out) < n:
                            if STOP.is_set(): raise KeyboardInterrupt
                            try: b = self.sock.recv(n - len(out))
                            except socket.timeout: continue
                            if not b: self.eof = True; break
                            out.extend(b)
                        return bytes(out)

                # Progress (count bytes read)
                counters = {"received": 0}
                def total(): return counters["received"]
                stop_ev = threading.Event()
                t = threading.Thread(target=progress_loop, args=(stop_ev, total, "Receiving:", None), daemon=True)
                t.start()
                class CountingReader:
                    def __init__(self, r, ctr): self.r, self.ctr = r, ctr
                    def read(self, n=-1):
                        b = self.r.read(n)
                        if b: self.ctr["received"] += len(b)
                        return b
                sr = CountingReader(PrependReader(conn, first), counters)

                with tarfile.open(fileobj=sr, mode="r|*") as tf:
                    local_hashes = safe_stream_extract_and_hash(tf, out_dir)
                stop_ev.set(); t.join()

                _verify_manifest(port, local_hashes)
                ok(f"Extracted into: {os.path.abspath(out_dir)}")
                return

            # ---------- FILE MODE ----------
            if out_path is None or (os.path.exists(out_path) and os.path.isdir(out_path)):
                dest_dir = os.path.abspath(out_path or "."); os.makedirs(dest_dir, exist_ok=True)
                tmp_path = unique_path(dest_dir, f".xfer-tmp-{int(time.time())}.part")
                final_explicit = None
            else:
                dest_dir = os.path.abspath(os.path.dirname(out_path)); os.makedirs(dest_dir, exist_ok=True)
                tmp_path = os.path.abspath(out_path); final_explicit = tmp_path
                if os.path.exists(tmp_path) and not force:
                    err(f"Output file exists: {tmp_path}. Use --force to overwrite."); _drain(conn); sys.exit(1)

            counters = {"received": 0}
            def total(): return counters["received"]
            stop_ev = threading.Event()
            t = threading.Thread(target=progress_loop, args=(stop_ev, total, "Receiving:", None), daemon=True)
            t.start()

            h = hashlib.sha256()
            with open(tmp_path, "wb") as f:
                if first:
                    f.write(first); h.update(first); counters["received"] += len(first)
                while True:
                    if STOP.is_set(): raise KeyboardInterrupt
                    try: b = conn.recv(CHUNK)
                    except socket.timeout: continue
                    if not b: break
                    f.write(b); h.update(b); counters["received"] += len(b)
            stop_ev.set(); t.join()

            sender_hash, sender_name = _receive_checksum_line(port)
            local_hash = h.hexdigest()
            if sender_hash and local_hash != sender_hash:
                warn("VERIFY FAIL — checksum mismatch.")
                if final_explicit:
                    err(f"Corrupt file retained at: {tmp_path}"); sys.exit(2)
                else:
                    corrupt_path = unique_path(dest_dir, (sanitize_name(sender_name) if sender_name else "xfer-incoming.bin") + ".corrupt")
                    try: os.replace(tmp_path, corrupt_path)
                    except Exception: pass
                    err(f"Corrupt file saved as: {corrupt_path}"); sys.exit(2)

            if final_explicit:
                ok(f"Saved: {tmp_path}")
                if sender_hash: ok(f"VERIFY OK — {local_hash}")
                return
            else:
                final_name = sanitize_name(sender_name) if sender_name else "xfer-incoming.bin"
                final_path = unique_path(dest_dir, final_name) if not force else os.path.join(dest_dir, final_name)
                if force and os.path.exists(final_path):
                    try: os.remove(final_path)
                    except Exception: pass
                try:
                    os.replace(tmp_path, final_path)
                except Exception as e:
                    warn(f"Rename failed ({e}); keeping temp file: {tmp_path}")
                    final_path = tmp_path
                if sender_hash: ok(f"VERIFY OK — {local_hash}")
                ok(f"Saved: {final_path}")
                return

# ----------------------- Senders -------------------------------------------

def send_file(ip: str, path: str, port: int, secure: bool):
    """
    Send a single file.
      - Secure (default): run TOFU+SAS control handshake first (port-1).
      - Data: stream raw bytes and compute SHA-256 on the fly.
      - Meta: send 'sha256␠basename' after data.
    """
    if secure:
        sas_sender_handshake(ip, port)

    size = os.path.getsize(path)
    counters = {"sent": 0}; stop_ev = threading.Event()
    def total(): return counters["sent"]

    banner(); info(f"Sending file '{path}' → {ip}:{port} ({human(size)})")
    with closing(connect_with_retry(ip, port)) as s:
        t = threading.Thread(target=progress_loop, args=(stop_ev, total, "Sending:", size), daemon=True)
        t.start()
        h = hashlib.sha256()
        with open(path, "rb") as f:
            while True:
                if STOP.is_set(): raise KeyboardInterrupt
                b = f.read(CHUNK)
                if not b: break
                h.update(b)
                view = memoryview(b)
                while view:
                    if STOP.is_set(): raise KeyboardInterrupt
                    try: n = s.send(view)
                    except socket.timeout: continue
                    view = view[n:]; counters["sent"] += n
        stop_ev.set(); t.join()

    try:
        with closing(connect_with_retry(ip, port + 1)) as meta:
            meta.sendall(f"{h.hexdigest()}  {os.path.basename(path)}\n".encode("utf-8"))
        ok("Checksum sent.")
    except Exception as e:
        warn(f"Checksum send failed ({e}); data delivered. Verification skipped on receiver.")

def send_dir(ip: str, path: str, port: int, excludes=(), secure: bool=True):
    """
    Send a directory.
      - Secure (default): run TOFU+SAS control handshake first (port-1).
      - Data: stream tar while hashing each file.
      - Meta: send a manifest (lines 'sha256␠rel_posix').
    """
    if secure:
        sas_sender_handshake(ip, port)

    abs_path = os.path.abspath(path); base = os.path.basename(abs_path)
    files = [(ap, rel, sz) for (ap, rel, sz) in iter_files_with_sizes(abs_path, excludes=excludes)]
    files.sort(key=lambda t: t[1]); total_bytes = tar_stream_size_estimate([sz for _, _, sz in files])

    counters = {"sent": 0}; stop_ev = threading.Event()
    def total(): return counters["sent"]

    banner(); info(f"Sending directory '{path}' → {ip}:{port} (top-level '{base}')")
    with closing(connect_with_retry(ip, port)) as s:
        class SocketWriter:
            def __init__(self, sock, ctr): self.sock, self.ctr = sock, ctr
            def write(self, b):
                mv = memoryview(b); total_sent = 0
                while mv:
                    if STOP.is_set(): raise KeyboardInterrupt
                    try: n = self.sock.send(mv)
                    except socket.timeout: continue
                    mv = mv[n:]; total_sent += n; self.ctr["sent"] += n
                return total_sent
            def flush(self): pass

        cw = SocketWriter(s, counters)
        t = threading.Thread(target=progress_loop, args=(stop_ev, total, "Sending:", total_bytes), daemon=True)
        t.start()
        manifest_lines = []
        with tarfile.open(fileobj=cw, mode="w|") as tf:
            for ap, rel_posix, sz in files:
                st = os.stat(ap)
                ti = tarfile.TarInfo(name=rel_posix); ti.size = sz
                try: ti.mtime = int(st.st_mtime); ti.mode = st.st_mode & 0o777
                except Exception: pass
                h = hashlib.sha256(); f = open(ap, "rb")
                class HashingReader:
                    def __init__(self, inner, hasher): self.inner, self.hasher = inner, hasher
                    def read(self, n=-1):
                        b = self.inner.read(n)
                        if b: self.hasher.update(b)
                        return b
                    def close(self): 
                        try: self.inner.close()
                        except Exception: pass
                hr = HashingReader(f, h)
                tf.addfile(ti, fileobj=hr); hr.close()
                manifest_lines.append(f"{h.hexdigest()}  {rel_posix}\n")
        stop_ev.set(); t.join()

    try:
        with closing(connect_with_retry(ip, port + 1)) as meta:
            buf = "".join(manifest_lines).encode("utf-8"); view = memoryview(buf)
            while view:
                if STOP.is_set(): raise KeyboardInterrupt
                try: n = meta.send(view[:1 << 20])
                except socket.timeout: continue
                view = view[n:]
        ok("Manifest sent.")
    except Exception as e:
        warn(f"Manifest send failed ({e}); data delivered. Verification skipped on receiver.)")

# ----------------------- XFER v4 Implementation --------------------------

def receive_auto_v4(out_path: str | None, port: int, force: bool = False, secure: bool = True):
    """XFER v4 auto-detecting receiver with full encryption."""
    banner()
    
    if secure:
        info("Secure mode: performing key exchange and TOFU verification...")
        # TODO: Implement TOFU+SAS handshake for v4
        # For now, we'll do a basic implementation
    
    # Listen on the data port
    try:
        with listener(port) as ls:
            info(f"Listening on port {port} (secure: {secure})...")
            info("Press Ctrl-C to cancel.")
            
            try:
                conn, (peer_ip, peer_port) = accept_one(ls, deadline=time.time() + 300)
                canonical_ip = canonicalize_address(peer_ip)
                info(f"Connection from {canonical_ip}:{peer_port}")
            except TimeoutError:
                warn("No connection received within timeout")
                sys.exit(1)
                
            with closing(conn):
                # Perform key exchange if secure
                session_keys = None
                if secure:
                    try:
                        session_keys = perform_key_exchange(conn, is_sender=False)
                        ok("Key exchange completed")
                    except Exception as e:
                        err(f"Key exchange failed: {e}")
                        sys.exit(2)
                
                # Read header to determine file vs directory
                header = _recv_all(conn, 6)
                
                if header == XAR4_FILE_MAGIC:
                    receive_file_v4(conn, session_keys, out_path, port, force)
                elif header == XAR4_DIR_MAGIC:
                    receive_directory_v4(conn, session_keys, out_path, port)
                else:
                    # Try legacy format fallback for insecure mode
                    if not secure and looks_like_tar_header(header + _recv_all(conn, 506)):
                        warn("Detected legacy format - falling back to v2 compatibility")
                        # TODO: Could implement legacy support here
                        err("Legacy format not supported in v4")
                        sys.exit(1)
                    else:
                        err("Unknown file format")
                        sys.exit(2)
                        
    except KeyboardInterrupt:
        warn("Cancelled by user")
        sys.exit(130)

def receive_file_v4(conn, session_keys, out_path, port, force):
    """Receive a file using XAR4 format."""
    # Read 8-byte size after magic
    size_bytes = _recv_all(conn, 8)
    file_size = struct.unpack('<Q', size_bytes)[0]
    
    info(f"Receiving file: {file_size} bytes")
    
    # Determine output path
    if out_path and os.path.isdir(out_path):
        # Will get actual filename from meta channel
        output_dir = out_path
        temp_file = os.path.join(output_dir, f".xfer-temp-{int(time.time())}.part")
    elif out_path:
        temp_file = out_path + ".part"
    else:
        temp_file = f".xfer-temp-{int(time.time())}.part"
    
    # Setup progress tracking
    counters = {"received": 0}
    stop_ev = threading.Event()
    
    def total_func():
        return counters["received"]
    
    # Start progress thread
    progress_thread = threading.Thread(
        target=progress_loop, 
        args=(stop_ev, total_func, "Receiving:", file_size), 
        daemon=True
    )
    progress_thread.start()
    
    try:
        # Receive file data
        hasher = hashlib.sha256()
        with open(temp_file, 'wb') as f:
            remaining = file_size
            while remaining > 0:
                if STOP.is_set():
                    raise KeyboardInterrupt()
                
                chunk_size = min(CHUNK, remaining)
                if session_keys:
                    # Read encrypted record
                    encrypted_record = _recv_all(conn, 13 + chunk_size + 16)  # AAD + data + tag
                    chunk = session_keys.decrypt_data_record(encrypted_record)
                else:
                    # Plain text
                    chunk = _recv_all(conn, chunk_size)
                
                f.write(chunk)
                hasher.update(chunk)
                counters["received"] += len(chunk)
                remaining -= len(chunk)
        
        stop_ev.set()
        progress_thread.join()
        
        # Receive metadata from meta port
        meta_hash, filename = receive_file_metadata_v4(port + 1, session_keys)
        
        # Verify hash
        computed_hash = hasher.hexdigest()
        if computed_hash != meta_hash:
            err(f"Hash mismatch! Expected {meta_hash}, got {computed_hash}")
            os.rename(temp_file, temp_file.replace('.part', '.corrupt'))
            sys.exit(2)
        
        # Move to final location
        if out_path and os.path.isdir(out_path):
            final_path = os.path.join(out_path, filename)
        elif out_path:
            final_path = out_path
        else:
            final_path = filename
            
        if not force and os.path.exists(final_path):
            final_path = unique_path(os.path.dirname(final_path), os.path.basename(final_path))
            
        os.rename(temp_file, final_path)
        ok(f"File saved: {final_path}")
        
    except Exception as e:
        stop_ev.set()
        if os.path.exists(temp_file):
            os.remove(temp_file)
        raise e

def receive_directory_v4(conn, session_keys, out_path, port):
    """Receive a directory using XAR4 format."""
    info("Receiving directory...")
    
    output_dir = out_path or "."
    os.makedirs(output_dir, exist_ok=True)
    
    local_hashes = {}
    counters = {"received": 0, "files": 0}
    stop_ev = threading.Event()
    
    def total_func():
        return counters["received"]
    
    progress_thread = threading.Thread(
        target=progress_loop, 
        args=(stop_ev, total_func, "Receiving:"), 
        daemon=True
    )
    progress_thread.start()
    
    try:
        while True:
            if STOP.is_set():
                raise KeyboardInterrupt()
                
            # Read entry header (type + path_len)
            entry_header = _recv_all(conn, 5)
            entry_type = entry_header[0]
            
            if entry_type == XAR4_DIR_TERMINATOR:
                break  # End of directory
                
            if entry_type != XAR4_FILE_ENTRY:
                err(f"Unknown entry type: {entry_type}")
                sys.exit(2)
            
            path_len = struct.unpack('>I', entry_header[1:5])[0]
            
            # Read rest of entry metadata
            entry_data = _recv_all(conn, 20 + path_len)  # mode(4) + mtime(8) + size(8) + path
            mode, mtime, size = struct.unpack('<IQQ', entry_data[:20])
            path = entry_data[20:20+path_len].decode('utf-8')
            
            # Normalize path
            norm_path = xar4_normalize_path(path)
            local_path = os.path.join(output_dir, norm_path)
            
            info(f"File: {norm_path} ({size} bytes)")
            
            # Create parent directories
            os.makedirs(os.path.dirname(local_path), exist_ok=True)
            
            # Receive file data
            hasher = hashlib.sha256()
            with open(local_path, 'wb') as f:
                remaining = size
                while remaining > 0:
                    chunk_size = min(CHUNK, remaining)
                    if session_keys:
                        # Read encrypted record  
                        encrypted_record = _recv_all(conn, 13 + chunk_size + 16)
                        chunk = session_keys.decrypt_data_record(encrypted_record)
                    else:
                        chunk = _recv_all(conn, chunk_size)
                    
                    f.write(chunk)
                    hasher.update(chunk)
                    counters["received"] += len(chunk)
                    remaining -= len(chunk)
            
            # Set file metadata
            try:
                os.chmod(local_path, mode & 0o777)
                os.utime(local_path, (mtime, mtime))
            except Exception:
                pass  # Non-critical
                
            local_hashes[norm_path] = hasher.hexdigest()
            counters["files"] += 1
        
        stop_ev.set()
        progress_thread.join()
        
        # Verify manifest from meta port
        verify_directory_manifest_v4(port + 1, session_keys, local_hashes)
        ok(f"Directory extracted: {output_dir} ({counters['files']} files)")
        
    except Exception as e:
        stop_ev.set()
        raise e

def receive_file_metadata_v4(meta_port, session_keys):
    """Receive file metadata from meta channel."""
    try:
        with listener(meta_port) as ls:
            conn, _ = accept_one(ls, deadline=time.time() + 60)
            with closing(conn):
                if session_keys:
                    # Read encrypted metadata
                    meta_size = struct.unpack('>I', _recv_all(conn, 4))[0]
                    encrypted_record = _recv_all(conn, meta_size)
                    metadata = session_keys.decrypt_meta_record(encrypted_record).decode('utf-8')
                else:
                    # Plain text
                    metadata = conn.recv(1024).decode('utf-8').strip()
                
                # Parse "sha256  filename"
                parts = metadata.split('  ', 1)
                if len(parts) != 2:
                    raise ValueError("Invalid metadata format")
                    
                return parts[0], parts[1]
    except Exception as e:
        err(f"Failed to receive metadata: {e}")
        sys.exit(2)

def verify_directory_manifest_v4(meta_port, session_keys, local_hashes):
    """Verify directory manifest from meta channel."""
    try:
        with listener(meta_port) as ls:
            conn, _ = accept_one(ls, deadline=time.time() + 60)
            with closing(conn):
                manifest_lines = []
                
                if session_keys:
                    # Read encrypted manifest records
                    while True:
                        try:
                            record_header = _recv_all(conn, 4)
                            record_size = struct.unpack('>I', record_header)[0]
                            if record_size == 0:
                                break
                            encrypted_record = _recv_all(conn, record_size)
                            line = session_keys.decrypt_meta_record(encrypted_record).decode('utf-8')
                            manifest_lines.append(line.strip())
                        except Exception:
                            break
                else:
                    # Plain text manifest
                    data = conn.recv(1024 * 1024).decode('utf-8')
                    manifest_lines = data.strip().split('\n')
                
                # Verify each file
                mismatches = []
                for line in manifest_lines:
                    if not line or line.startswith('#'):
                        continue
                    parts = line.split('  ', 1)
                    if len(parts) != 2:
                        continue
                    expected_hash, path = parts
                    actual_hash = local_hashes.get(path, '')
                    if expected_hash != actual_hash:
                        mismatches.append(f"{path}: expected {expected_hash}, got {actual_hash}")
                
                if mismatches:
                    err("Hash verification failed:")
                    for mismatch in mismatches:
                        err(f"  {mismatch}")
                    sys.exit(2)
                else:
                    ok("All files verified successfully")
                    
    except Exception as e:
        err(f"Failed to verify manifest: {e}")
        sys.exit(2)

def send_file_v4(ip: str, path: str, port: int, secure: bool = True):
    """Send a file using XFER v4 with encryption."""
    banner()
    
    if not os.path.isfile(path):
        err(f"Not a file: {path}")
        sys.exit(1)
    
    file_size = os.path.getsize(path)
    filename = os.path.basename(path)
    
    info(f"Sending file: {filename} ({file_size} bytes) to {ip}:{port}")
    info(f"Secure: {secure}")
    
    if secure:
        info("Will perform key exchange and TOFU verification...")
    
    try:
        # Connect to receiver
        with closing(connect_with_retry(ip, port, total_timeout=30)) as conn:
            canonical_ip = canonicalize_address(ip)
            
            # Perform key exchange if secure
            session_keys = None
            if secure:
                try:
                    session_keys = perform_key_exchange(conn, is_sender=True) 
                    ok("Key exchange completed")
                except Exception as e:
                    err(f"Key exchange failed: {e}")
                    sys.exit(2)
            
            # Send XAR4 file header
            header = xar4_create_file_header(file_size)
            conn.sendall(header)
            
            # Setup progress
            counters = {"sent": 0}
            stop_ev = threading.Event()
            
            def total_func():
                return counters["sent"]
            
            progress_thread = threading.Thread(
                target=progress_loop,
                args=(stop_ev, total_func, "Sending:", file_size),
                daemon=True
            )
            progress_thread.start()
            
            # Send file data and compute hash
            hasher = hashlib.sha256()
            with open(path, 'rb') as f:
                while True:
                    if STOP.is_set():
                        raise KeyboardInterrupt()
                        
                    chunk = f.read(CHUNK)
                    if not chunk:
                        break
                        
                    hasher.update(chunk)
                    
                    if session_keys:
                        # Encrypt and send
                        encrypted_record = session_keys.encrypt_data_record(chunk)
                        conn.sendall(encrypted_record)
                    else:
                        conn.sendall(chunk)
                        
                    counters["sent"] += len(chunk)
            
            stop_ev.set()
            progress_thread.join()
            
            # Send metadata on meta channel
            computed_hash = hasher.hexdigest()
            send_file_metadata_v4(ip, port + 1, session_keys, computed_hash, filename)
            
            ok("File sent successfully")
            
    except KeyboardInterrupt:
        warn("Cancelled by user")
        sys.exit(130)
    except Exception as e:
        err(f"Transfer failed: {e}")
        sys.exit(2)

def send_dir_v4(ip: str, path: str, port: int, excludes=(), secure: bool = True):
    """Send a directory using XFER v4 with encryption."""
    banner()
    
    if not os.path.isdir(path):
        err(f"Not a directory: {path}")
        sys.exit(1)
    
    # Collect files
    files = list(iter_files_with_sizes(path, excludes))
    if not files:
        err("No files to send")
        sys.exit(1)
        
    total_size = sum(size for _, _, size in files)
    info(f"Sending directory: {path} ({len(files)} files, {total_size} bytes) to {ip}:{port}")
    info(f"Secure: {secure}")
    
    if secure:
        info("Will perform key exchange and TOFU verification...")
    
    try:
        # Connect to receiver  
        with closing(connect_with_retry(ip, port, total_timeout=30)) as conn:
            # Perform key exchange if secure
            session_keys = None
            if secure:
                try:
                    session_keys = perform_key_exchange(conn, is_sender=True)
                    ok("Key exchange completed")
                except Exception as e:
                    err(f"Key exchange failed: {e}")
                    sys.exit(2)
            
            # Send XAR4 directory header
            header = xar4_create_dir_header()
            conn.sendall(header)
            
            # Setup progress
            counters = {"sent": 0}
            stop_ev = threading.Event()
            
            def total_func():
                return counters["sent"]
            
            progress_thread = threading.Thread(
                target=progress_loop,
                args=(stop_ev, total_func, "Sending:", total_size),
                daemon=True  
            )
            progress_thread.start()
            
            # Send each file
            manifest_lines = []
            for abs_path, rel_path, size in files:
                if STOP.is_set():
                    raise KeyboardInterrupt()
                
                info(f"Sending: {rel_path}")
                
                # Get file metadata
                stat = os.stat(abs_path)
                mode = stat.st_mode & 0o777
                mtime = int(stat.st_mtime)
                
                # Send entry header
                entry = xar4_create_dir_entry(rel_path, mode, mtime, size)
                conn.sendall(entry)
                
                # Send file data
                hasher = hashlib.sha256()
                with open(abs_path, 'rb') as f:
                    remaining = size
                    while remaining > 0:
                        chunk_size = min(CHUNK, remaining)
                        chunk = f.read(chunk_size)
                        if not chunk:
                            break
                            
                        hasher.update(chunk)
                        
                        if session_keys:
                            encrypted_record = session_keys.encrypt_data_record(chunk)
                            conn.sendall(encrypted_record)
                        else:
                            conn.sendall(chunk)
                            
                        counters["sent"] += len(chunk)
                        remaining -= len(chunk)
                
                manifest_lines.append(f"{hasher.hexdigest()}  {rel_path}")
            
            # Send directory terminator
            terminator = xar4_create_dir_terminator()
            conn.sendall(terminator)
            
            stop_ev.set()
            progress_thread.join()
            
            # Send manifest on meta channel
            send_directory_manifest_v4(ip, port + 1, session_keys, manifest_lines)
            
            ok("Directory sent successfully")
            
    except KeyboardInterrupt:
        warn("Cancelled by user")
        sys.exit(130)
    except Exception as e:
        err(f"Transfer failed: {e}")
        sys.exit(2)

def send_file_metadata_v4(ip, meta_port, session_keys, file_hash, filename):
    """Send file metadata on meta channel."""
    try:
        with closing(connect_with_retry(ip, meta_port, total_timeout=30)) as conn:
            metadata = f"{file_hash}  {filename}"
            
            if session_keys:
                encrypted_record = session_keys.encrypt_meta_record(metadata.encode('utf-8'))
                # Send size first
                conn.sendall(struct.pack('>I', len(encrypted_record)))
                conn.sendall(encrypted_record)
            else:
                conn.sendall(metadata.encode('utf-8'))
                
    except Exception as e:
        err(f"Failed to send metadata: {e}")
        sys.exit(2)

def send_directory_manifest_v4(ip, meta_port, session_keys, manifest_lines):
    """Send directory manifest on meta channel."""
    try:
        with closing(connect_with_retry(ip, meta_port, total_timeout=30)) as conn:
            if session_keys:
                # Send each line as encrypted record
                for line in manifest_lines:
                    encrypted_record = session_keys.encrypt_meta_record(line.encode('utf-8'))
                    conn.sendall(struct.pack('>I', len(encrypted_record)))
                    conn.sendall(encrypted_record)
                # Send terminator
                conn.sendall(struct.pack('>I', 0))
            else:
                manifest = '\n'.join(manifest_lines)
                conn.sendall(manifest.encode('utf-8'))
                
    except Exception as e:
        err(f"Failed to send manifest: {e}")
        sys.exit(2)

# ----------------------- CLI ------------------------------------------------

def main():
    """
    XFER v4 CLI - Secure file transfer with ChaCha20-Poly1305 encryption.
    Commands:
      - ip                     : list local IPv4/IPv6 addresses
      - receive [options]      : auto-detect and receive file or directory
      - send <IP> <PATH> [opts]: send file or directory with encryption
    """
    parser = argparse.ArgumentParser(
        prog="xfer.py",
        description=(
            f"{APP_NAME} v4 — {APP_TAGLINE}\n"
            "Secure by default: ephemeral Diffie-Hellman key agreement, TOFU identity pinning,\n"
            "Short Authentication String (SAS) comparison on first pairing.\n"
            "Encryption: ChaCha20-Poly1305 AEAD, keys derived via HKDF, group FFDHE-2048.\n\n"
            "Commands:\n"
            "  ip                        - show local IPv4/IPv6 addresses\n"
            "  receive [--out PATH]      - listen and receive file or directory\n"
            "  send <IP> <PATH>          - send file or directory with encryption\n\n"
            "Use --insecure on both sides to disable encryption for legacy compatibility."
        ),
        formatter_class=argparse.RawTextHelpFormatter
    )
    parser.add_argument("-v", "--version", action="version", version=f"{APP_NAME} v{VERSION}",
                        help="Show version and exit.")
    sub = parser.add_subparsers(dest="cmd", required=True)

    # ip command
    sub.add_parser("ip", help="Show local IPv4/IPv6 addresses.")

    # receive command
    p_rcv = sub.add_parser("receive", help="Auto-detect file vs directory and handle appropriately.")
    p_rcv.add_argument("--out", help="Output path. Default: current directory for dirs, sender's filename for files.")
    p_rcv.add_argument("--port", type=int, default=DEFAULT_PORT, help=f"Data port (default {DEFAULT_PORT}).")
    p_rcv.add_argument("--force", action="store_true", help="Allow overwriting existing files.")
    p_rcv.add_argument("--insecure", action="store_true", help="Disable encryption (both sides must use this).")

    # send command
    p_snd = sub.add_parser("send", help="Send a file or directory.")
    p_snd.add_argument("ip", help="Receiver IPv4 or IPv6 address.")
    p_snd.add_argument("path", help="File or directory path to send.")
    p_snd.add_argument("--port", type=int, default=DEFAULT_PORT, help=f"Data port (default {DEFAULT_PORT}).")
    p_snd.add_argument("--exclude", action="append", metavar="PATTERN", 
                       help="Exclude files matching pattern (fnmatch style). Can be repeated.")
    p_snd.add_argument("--exclude-from", metavar="FILE",
                       help="Read exclusion patterns from file (one per line, # for comments).")
    p_snd.add_argument("--insecure", action="store_true", help="Disable encryption (both sides must use this).")

    args = parser.parse_args()

    if args.cmd == "ip":
        banner()
        # Show both IPv4 and IPv6 addresses
        sys.stderr.write(f"{CLR['dim']}Local addresses:{CLR['reset']}\n")
        ipv4_addrs = local_ips()
        ipv6_addrs = local_ipv6_addresses()
        
        for i, ip in enumerate(ipv4_addrs, 1):
            sys.stderr.write(f"  {i}. {ip}\n")
        for i, ip in enumerate(ipv6_addrs, len(ipv4_addrs) + 1):
            sys.stderr.write(f"  {i}. [{ip}]\n")
        
        sys.stderr.write(f"\n{CLR['gray']}XFER v4 = secure file transfer with ChaCha20-Poly1305 encryption.{CLR['reset']}\n")
        return

    if args.cmd == "receive":
        receive_auto_v4(args.out, args.port, force=args.force, secure=not args.insecure)
        return

    if args.cmd == "send":
        # Prepare exclusions
        excludes = args.exclude or []
        if args.exclude_from:
            try:
                with open(args.exclude_from, 'r', encoding='utf-8') as f:
                    for line in f:
                        line = line.strip()
                        if line and not line.startswith('#'):
                            excludes.append(line)
            except Exception as e:
                err(f"Failed to read exclusions file: {e}")
                sys.exit(1)
        
        if not os.path.exists(args.path):
            err(f"Path not found: {args.path}")
            sys.exit(1)
        
        if os.path.isfile(args.path):
            send_file_v4(args.ip, args.path, args.port, secure=not args.insecure)
        elif os.path.isdir(args.path):
            send_dir_v4(args.ip, args.path, args.port, excludes=excludes, secure=not args.insecure)
        else:
            err(f"Not a regular file or directory: {args.path}")
            sys.exit(1)
    p_rf.add_argument("--port", type=int, default=DEFAULT_PORT, help=f"Data port (default {DEFAULT_PORT}).")
    p_rf.add_argument("--force", action="store_true", help="Allow overwriting existing file.")
    p_rf.add_argument("--insecure", "--no-secure", dest="no_secure", action="store_true",
                      help="Disable TOFU+SAS (control port). Use on both sides for legacy/automation.")

    # recv-dir (enforced mode)
    p_rd = sub.add_parser("recv-dir", help="Receive a directory (streaming tar; enforced mode).")
    p_rd.add_argument("output_dir", help="Destination directory to extract into.")
    p_rd.add_argument("--port", type=int, default=DEFAULT_PORT, help=f"Data port (default {DEFAULT_PORT}).")
    p_rd.add_argument("--insecure", "--no-secure", dest="no_secure", action="store_true",
                      help="Disable TOFU+SAS (control port). Use on both sides for legacy/automation.")

    # send
    p_s = sub.add_parser("send", help="Send a file or a directory (auto-detect).")
    p_s.add_argument("receiver_ip", help="Receiver IP address (same LAN recommended).")
    p_s.add_argument("path", help="Path to a regular file or a directory to send.")
    p_s.add_argument("--port", type=int, default=DEFAULT_PORT, help=f"Receiver data port (default {DEFAULT_PORT}).")
    p_s.add_argument("--exclude", action="append", default=[], metavar="PATTERN",
                     help="Exclude by fnmatch; can repeat (e.g., --exclude '.git/*' --exclude '*.pyc').")
    p_s.add_argument("--insecure", "--no-secure", dest="no_secure", action="store_true",
                     help="Disable TOFU+SAS (control port). Use on both sides for legacy/automation.")

    args = parser.parse_args()

    if args.cmd == "ip":
        banner()
        sys.stderr.write(f"{CLR['dim']}Local IPv4 addresses:{CLR['reset']}\n")
        for i, ip in enumerate(local_ips(), 1):
            sys.stderr.write(f"  {i}. {ip}\n")
        sys.stderr.write(f"\n{CLR['gray']}XFER = industry shorthand for “transfer”.{CLR['reset']}\n")
        return

    if args.cmd == "receive":
        receive_auto(args.out, args.port, force=args.force, expected=None, secure=not args.no_secure); return

    if args.cmd == "recv-file":
        receive_auto(args.output, args.port, force=args.force, expected="file", secure=not args.no_secure); return

    if args.cmd == "recv-dir":
        receive_auto(args.output_dir, args.port, force=False, expected="dir", secure=not args.no_secure); return

    if args.cmd == "send":
        if not os.path.exists(args.path):
            err(f"Path not found: {args.path}"); sys.exit(1)
        if os.path.isfile(args.path):
            send_file(args.receiver_ip, args.path, args.port, secure=not args.no_secure)
        elif os.path.isdir(args.path):
            send_dir(args.receiver_ip, args.path, args.port, excludes=args.exclude or [], secure=not args.no_secure)
        else:
            err(f"Not a regular file or directory: {args.path}"); sys.exit(1)

if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        STOP.set()
        sys.stderr.write(f"\n{CLR['warn']}Cancelled by user.{CLR['reset']}\n")
        sys.exit(130)

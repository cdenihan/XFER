use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chacha20poly1305::aead::{Aead, KeyInit as AeadKeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use clap::{Parser, Subcommand};
use glob::Pattern;
use hmac::{Hmac, Mac};
use hmac::digest::KeyInit as HmacKeyInit;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tar::{Archive, Builder, Header};
use walkdir::WalkDir;
use x25519_dalek::{PublicKey, StaticSecret};

type HmacSha256 = Hmac<Sha256>;

const APP_NAME: &str = "XFER";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_PORT: u16 = 9000;
const CHUNK: usize = 4 * 1024 * 1024;
const SAS_CTRL_OFFSET: u16 = 1;
const STATUS_OFFSET: u16 = 2;
const META_ACCEPT_BUDGET: Duration = Duration::from_secs(60);
const SAS_ACCEPT_BUDGET: Duration = Duration::from_secs(120);

#[derive(Parser, Debug)]
#[command(name = "xfer", version = VERSION, about = "Fast file/directory transfer with TOFU+SAS and end-to-end encryption")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Show local IPv4 addresses
    Ip,
    /// Receive file or directory automatically
    Receive {
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        #[arg(long)]
        force: bool,
        #[arg(long = "insecure", alias = "no-secure")]
        insecure: bool,
    },
    /// Receive file only
    RecvFile {
        output: PathBuf,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        #[arg(long)]
        force: bool,
        #[arg(long = "insecure", alias = "no-secure")]
        insecure: bool,
    },
    /// Receive directory only
    RecvDir {
        output_dir: PathBuf,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        #[arg(long = "insecure", alias = "no-secure")]
        insecure: bool,
    },
    /// Send file or directory
    Send {
        receiver_ip: String,
        path: PathBuf,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        #[arg(long = "exclude")]
        excludes: Vec<String>,
        #[arg(long = "insecure", alias = "no-secure")]
        insecure: bool,
    },
}

#[derive(Clone, Debug)]
struct Session {
    key: [u8; 32],
}

fn main() {
    if let Err(e) = run() {
        eprintln!("[ERR ] {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Ip => {
            println!("{APP_NAME} v{VERSION}");
            for ip in local_ips() {
                println!("{ip}");
            }
        }
        Commands::Receive {
            out,
            port,
            force,
            insecure,
        } => receive_auto(out, port, force, None, !insecure)?,
        Commands::RecvFile {
            output,
            port,
            force,
            insecure,
        } => receive_auto(Some(output), port, force, Some("file"), !insecure)?,
        Commands::RecvDir {
            output_dir,
            port,
            insecure,
        } => receive_auto(Some(output_dir), port, false, Some("dir"), !insecure)?,
        Commands::Send {
            receiver_ip,
            path,
            port,
            excludes,
            insecure,
        } => {
            if !path.exists() {
                return Err(format!("Path not found: {}", path.display()));
            }
            if path.is_file() {
                send_file(&receiver_ip, &path, port, !insecure)?;
            } else if path.is_dir() {
                send_dir(&receiver_ip, &path, port, &excludes, !insecure)?;
            } else {
                return Err(format!("Not a regular file or directory: {}", path.display()));
            }
        }
    }

    Ok(())
}

fn local_ips() -> Vec<String> {
    let mut ips: Vec<String> = Vec::new();
    if let Ok(host) = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| std::env::var("USERDOMAIN"))
    {
        if let Ok(iter) = (host.as_str(), 0).to_socket_addrs() {
            for sa in iter {
                let ip = sa.ip();
                if ip.is_ipv4() {
                    let s = ip.to_string();
                    if !ips.contains(&s) {
                        ips.push(s);
                    }
                }
            }
        }
    }
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
        let _ = sock.connect("8.8.8.8:80");
        if let Ok(SocketAddr::V4(v4)) = sock.local_addr() {
            let s = v4.ip().to_string();
            if !ips.contains(&s) {
                ips.push(s);
            }
        }
    }
    if !ips.iter().any(|x| x == "127.0.0.1") {
        ips.push("127.0.0.1".to_string());
    }
    ips.sort();
    ips
}

fn xfer_home() -> Result<PathBuf, String> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| "Unable to locate home directory".to_string())?;
    let p = Path::new(&home).join(".xfer");
    fs::create_dir_all(&p).map_err(|e| format!("create {}: {e}", p.display()))?;
    Ok(p)
}

fn identity_path() -> Result<PathBuf, String> {
    Ok(xfer_home()?.join("identity.key"))
}

fn known_peers_path() -> Result<PathBuf, String> {
    Ok(xfer_home()?.join("known_peers"))
}

fn load_or_create_identity() -> Result<StaticSecret, String> {
    let path = identity_path()?;
    let mut b = [0u8; 32];
    if path.exists() {
        let raw = fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        if raw.len() == 32 {
            b.copy_from_slice(&raw);
            return Ok(StaticSecret::from(b));
        }
    }
    rand::thread_rng().fill_bytes(&mut b);
    fs::write(&path, b).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(StaticSecret::from(b))
}

fn receiver_fingerprint_hex(secret: &StaticSecret) -> String {
    let mut h = Sha256::new();
    h.update(b"XFER-ID-v1");
    h.update(secret.to_bytes());
    hex::encode(h.finalize())
}

fn load_known_peers() -> Result<HashMap<String, String>, String> {
    let path = known_peers_path()?;
    let mut map = HashMap::new();
    if !path.exists() {
        return Ok(map);
    }
    let s = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        if let (Some(k), Some(v)) = (it.next(), it.next()) {
            map.insert(k.to_string(), v.to_string());
        }
    }
    Ok(map)
}

fn save_known_peers(map: &HashMap<String, String>) -> Result<(), String> {
    let path = known_peers_path()?;
    let mut keys: Vec<_> = map.keys().cloned().collect();
    keys.sort();
    let mut s = String::new();
    for k in keys {
        if let Some(v) = map.get(&k) {
            s.push_str(&format!("{k} {v}\n"));
        }
    }
    fs::write(&path, s).map_err(|e| format!("write {}: {e}", path.display()))
}

fn sas_from(fp_hex: &str, ns_hex: &str, nr_hex: &str) -> Result<String, String> {
    let key = hex::decode(fp_hex).map_err(|e| format!("bad fp hex: {e}"))?;
    let ns = hex::decode(ns_hex).map_err(|e| format!("bad ns hex: {e}"))?;
    let nr = hex::decode(nr_hex).map_err(|e| format!("bad nr hex: {e}"))?;

    let mut msg = Vec::with_capacity(ns.len() + nr.len() + 9);
    msg.extend_from_slice(&ns);
    msg.extend_from_slice(&nr);
    msg.extend_from_slice(b"XFER-SAS1");

    let mut mac = <HmacSha256 as HmacKeyInit>::new_from_slice(&key).map_err(|e| format!("hmac: {e}"))?;
    mac.update(&msg);
    let out = mac.finalize().into_bytes();
    let mut nbuf = [0u8; 8];
    nbuf.copy_from_slice(&out[..8]);
    let n = u64::from_be_bytes(nbuf) % 10_000_000_000;
    let s = format!("{n:010}");
    Ok(format!("{}-{}-{}", &s[0..3], &s[3..6], &s[6..10]))
}

fn prompt_trust(prompt: &str) -> Result<bool, String> {
    print!("{prompt}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut input = String::new();
    io::stdin().read_line(&mut input).map_err(|e| e.to_string())?;
    let v = input.trim().to_ascii_lowercase();
    Ok(v.is_empty() || v == "y" || v == "yes")
}

fn prompt_override(prompt: &str) -> Result<bool, String> {
    print!("{prompt}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut input = String::new();
    io::stdin().read_line(&mut input).map_err(|e| e.to_string())?;
    Ok(input.trim().eq_ignore_ascii_case("override"))
}

fn read_headers(conn: &mut TcpStream, max_bytes: usize) -> Result<HashMap<String, String>, String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 512];
    loop {
        if buf.windows(2).any(|w| w == b"\n\n") {
            break;
        }
        match conn.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > max_bytes {
                    break;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            out.insert(k.trim().to_ascii_uppercase(), v.trim().to_string());
        }
    }
    Ok(out)
}

fn write_headers(conn: &mut TcpStream, kv: &[(&str, String)]) -> Result<(), String> {
    for (k, v) in kv {
        conn.write_all(format!("{k}:{v}\n").as_bytes())
            .map_err(|e| e.to_string())?;
    }
    conn.write_all(b"\n").map_err(|e| e.to_string())
}

fn derive_session_key(shared: [u8; 32], ns_hex: &str, nr_hex: &str) -> Result<[u8; 32], String> {
    let ns = hex::decode(ns_hex).map_err(|e| e.to_string())?;
    let nr = hex::decode(nr_hex).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    h.update(b"XFER-E2E1");
    h.update(shared);
    h.update(ns);
    h.update(nr);
    let d = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&d);
    Ok(out)
}

fn connect_with_retry(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let deadline = Instant::now() + timeout;
    let mut delay = Duration::from_millis(200);
    let mut last = "connect failed".to_string();
    while Instant::now() < deadline {
        match TcpStream::connect((host, port)) {
            Ok(s) => {
                s.set_nonblocking(false).ok();
                s.set_read_timeout(None).ok();
                s.set_write_timeout(None).ok();
                return Ok(s);
            }
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(delay);
                delay = std::cmp::min(delay.mul_f32(1.5), Duration::from_secs(2));
            }
        }
    }
    Err(last)
}

fn listener(port: u16) -> Result<TcpListener, String> {
    let l = TcpListener::bind(("0.0.0.0", port)).map_err(|e| format!("bind port {port}: {e}"))?;
    l.set_nonblocking(true).map_err(|e| e.to_string())?;
    Ok(l)
}

fn accept_one(l: &TcpListener, deadline: Option<Instant>) -> Result<(TcpStream, String), String> {
    loop {
        match l.accept() {
            Ok((s, a)) => {
                s.set_nonblocking(false).ok();
                s.set_read_timeout(None).ok();
                s.set_write_timeout(None).ok();
                return Ok((s, a.ip().to_string()));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                if let Some(d) = deadline {
                    if Instant::now() > d {
                        return Err("timeout".to_string());
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn status_port(port: u16) -> Option<u16> {
    port.checked_add(STATUS_OFFSET)
}

fn spawn_status_receiver(port: u16) -> Option<std::thread::JoinHandle<()>> {
    let p = status_port(port)?;
    Some(std::thread::spawn(move || {
        let Ok(l) = listener(p) else { return };
        let Ok((stream, _)) = accept_one(&l, Some(Instant::now() + Duration::from_secs(180))) else {
            return;
        };
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let msg = line.trim();
                    if !msg.is_empty() {
                        println!("[STATUS] {msg}");
                    }
                }
                Err(_) => break,
            }
        }
    }))
}

fn spawn_status_sender(
    ip: String,
    port: u16,
    bytes_done: Arc<AtomicU64>,
    total: Option<u64>,
    finished: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    let p = status_port(port)?;
    Some(std::thread::spawn(move || {
        let Ok(mut stream) = connect_with_retry(&ip, p, Duration::from_secs(30)) else {
            return;
        };
        loop {
            let done = bytes_done.load(Ordering::Relaxed);
            let msg = match total {
                Some(t) => format!("sent={} total={}", done, t),
                None => format!("sent={done}"),
            };
            if stream.write_all(msg.as_bytes()).is_err() || stream.write_all(b"\n").is_err() {
                return;
            }
            if finished.load(Ordering::Relaxed) {
                let _ = stream.write_all(b"done=1\n");
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }))
}

fn sas_receiver_handshake(port: u16) -> Result<Session, String> {
    if port <= SAS_CTRL_OFFSET {
        return Err("Invalid --port: control port would be <= 0".to_string());
    }
    let control = port - SAS_CTRL_OFFSET;
    let identity = load_or_create_identity()?;
    let receiver_pub = PublicKey::from(&identity);
    let fp_hex = receiver_fingerprint_hex(&identity);
    let mut nr = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nr);
    let nr_hex = hex::encode(nr);

    println!("[INFO] SAS: waiting on control port {control} ...");
    let l = listener(control)?;
    let (mut conn, peer_ip) = accept_one(&l, Some(Instant::now() + SAS_ACCEPT_BUDGET))?;

    write_headers(
        &mut conn,
        &[
            ("PROTO", "XFER-SAS2".to_string()),
            ("FP", fp_hex.clone()),
            ("RPUB", hex::encode(receiver_pub.as_bytes())),
            ("NR", nr_hex.clone()),
        ],
    )?;
    let hdr = read_headers(&mut conn, 4096)?;
    let ns_hex = hdr.get("NS").cloned().unwrap_or_default();
    let spub_hex = hdr.get("SPUB").cloned().unwrap_or_default();
    if ns_hex.is_empty() || spub_hex.is_empty() {
        return Err("SAS: sender nonce/public key missing".to_string());
    }

    let sas = sas_from(&fp_hex, &ns_hex, &nr_hex)?;
    let hp = format!("{peer_ip}:{port}");
    let mut peers = load_known_peers()?;

    match peers.get(&hp) {
        None => {
            println!("SAS pairing (new peer) host:{hp}");
            println!("  Fingerprint: {}…", &fp_hex[..16]);
            println!("  Code: {sas}");
            if !prompt_trust("  If this matches sender, press ENTER to trust or type 'no': ")? {
                return Err("SAS: user declined pairing".to_string());
            }
            peers.insert(hp.clone(), fp_hex.clone());
            save_known_peers(&peers)?;
        }
        Some(existing) if existing != &fp_hex => {
            println!("SAS warning: fingerprint changed for {hp}");
            println!("  Prev: {}…", &existing[..16]);
            println!("  New : {}…", &fp_hex[..16]);
            println!("  Code: {sas}");
            if !prompt_override("  Type 'override' to trust new fingerprint: ")? {
                return Err("SAS: fingerprint change rejected".to_string());
            }
            peers.insert(hp.clone(), fp_hex.clone());
            save_known_peers(&peers)?;
        }
        _ => println!("[ OK ] SAS: trusted peer {hp}"),
    }

    let spub_raw = hex::decode(spub_hex).map_err(|e| e.to_string())?;
    if spub_raw.len() != 32 {
        return Err("SAS: invalid sender public key".to_string());
    }
    let mut sbytes = [0u8; 32];
    sbytes.copy_from_slice(&spub_raw);
    let sender_pub = PublicKey::from(sbytes);
    let shared = identity.diffie_hellman(&sender_pub).to_bytes();
    let key = derive_session_key(shared, &ns_hex, &nr_hex)?;
    Ok(Session { key })
}

fn sas_sender_handshake(ip: &str, port: u16) -> Result<Session, String> {
    if port <= SAS_CTRL_OFFSET {
        return Err("Invalid --port: control port would be <= 0".to_string());
    }
    let control = port - SAS_CTRL_OFFSET;
    let mut conn = connect_with_retry(ip, control, SAS_ACCEPT_BUDGET)?;

    let hdr = read_headers(&mut conn, 4096)?;
    if hdr.get("PROTO").map(String::as_str) != Some("XFER-SAS2") {
        return Err("SAS: receiver does not speak XFER-SAS2".to_string());
    }
    let fp_hex = hdr.get("FP").cloned().unwrap_or_default();
    let nr_hex = hdr.get("NR").cloned().unwrap_or_default();
    let rpub_hex = hdr.get("RPUB").cloned().unwrap_or_default();
    if fp_hex.is_empty() || nr_hex.is_empty() || rpub_hex.is_empty() {
        return Err("SAS: receiver handshake missing fields".to_string());
    }

    let mut ns = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut ns);
    let ns_hex = hex::encode(ns);
    let sender_secret = StaticSecret::random_from_rng(rand::thread_rng());
    let sender_pub = PublicKey::from(&sender_secret);

    write_headers(
        &mut conn,
        &[("NS", ns_hex.clone()), ("SPUB", hex::encode(sender_pub.as_bytes()))],
    )?;

    let sas = sas_from(&fp_hex, &ns_hex, &nr_hex)?;
    let hp = format!("{ip}:{port}");
    let mut peers = load_known_peers()?;

    match peers.get(&hp) {
        None => {
            println!("SAS pairing (new peer) host:{hp}");
            println!("  Fingerprint: {}…", &fp_hex[..16]);
            println!("  Code: {sas}");
            if !prompt_trust("  If this matches receiver, press ENTER to trust or type 'no': ")? {
                return Err("SAS: user declined pairing".to_string());
            }
            peers.insert(hp.clone(), fp_hex.clone());
            save_known_peers(&peers)?;
        }
        Some(existing) if existing != &fp_hex => {
            println!("SAS warning: fingerprint changed for {hp}");
            println!("  Prev: {}…", &existing[..16]);
            println!("  New : {}…", &fp_hex[..16]);
            println!("  Code: {sas}");
            if !prompt_override("  Type 'override' to trust new fingerprint: ")? {
                return Err("SAS: fingerprint change rejected".to_string());
            }
            peers.insert(hp.clone(), fp_hex.clone());
            save_known_peers(&peers)?;
        }
        _ => println!("[ OK ] SAS: trusted peer {hp}"),
    }

    let rpub_raw = hex::decode(rpub_hex).map_err(|e| e.to_string())?;
    if rpub_raw.len() != 32 {
        return Err("SAS: invalid receiver public key".to_string());
    }
    let mut rbytes = [0u8; 32];
    rbytes.copy_from_slice(&rpub_raw);
    let receiver_pub = PublicKey::from(rbytes);
    let shared = sender_secret.diffie_hellman(&receiver_pub).to_bytes();
    let key = derive_session_key(shared, &ns_hex, &nr_hex)?;
    Ok(Session { key })
}

struct EncryptWriter<W: Write> {
    inner: W,
    cipher: ChaCha20Poly1305,
    ctr: u64,
}

impl<W: Write> EncryptWriter<W> {
    fn new(inner: W, key: &[u8; 32]) -> Self {
        Self {
            inner,
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            ctr: 0,
        }
    }

    fn nonce_for(&self) -> Nonce {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&self.ctr.to_be_bytes());
        *Nonce::from_slice(&n)
    }
}

impl<W: Write> Write for EncryptWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let nonce = self.nonce_for();
        let encrypted = self
            .cipher
            .encrypt(&nonce, buf)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "encrypt failed"))?;
        let len = (encrypted.len() as u32).to_be_bytes();
        self.inner.write_all(&len)?;
        self.inner.write_all(&encrypted)?;
        self.ctr = self.ctr.wrapping_add(1);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct DecryptReader<R: Read> {
    inner: R,
    cipher: ChaCha20Poly1305,
    ctr: u64,
    buf: Vec<u8>,
    off: usize,
    eof: bool,
}

impl<R: Read> DecryptReader<R> {
    fn new(inner: R, key: &[u8; 32]) -> Self {
        Self {
            inner,
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            ctr: 0,
            buf: Vec::new(),
            off: 0,
            eof: false,
        }
    }

    fn fill_next(&mut self) -> io::Result<()> {
        if self.eof {
            return Ok(());
        }
        let mut lbuf = [0u8; 4];
        match self.inner.read_exact(&mut lbuf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                self.eof = true;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        let len = u32::from_be_bytes(lbuf) as usize;
        if len == 0 {
            self.eof = true;
            return Ok(());
        }
        let mut cbuf = vec![0u8; len];
        self.inner.read_exact(&mut cbuf)?;

        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&self.ctr.to_be_bytes());
        let nonce = Nonce::from_slice(&n);
        let plain = self
            .cipher
            .decrypt(nonce, cbuf.as_ref())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "decrypt failed"))?;
        self.ctr = self.ctr.wrapping_add(1);
        self.buf = plain;
        self.off = 0;
        Ok(())
    }
}

impl<R: Read> Read for DecryptReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.off >= self.buf.len() {
            self.fill_next()?;
            if self.off >= self.buf.len() {
                return Ok(0);
            }
        }
        let n = std::cmp::min(out.len(), self.buf.len() - self.off);
        out[..n].copy_from_slice(&self.buf[self.off..self.off + n]);
        self.off += n;
        Ok(n)
    }
}

struct HashingReader<R: Read> {
    inner: R,
    hasher: Sha256,
}

impl<R: Read> HashingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    fn finalize_hex(self) -> String {
        hex::encode(self.hasher.finalize())
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.hasher.update(&buf[..n]);
        }
        Ok(n)
    }
}

fn write_preface(stream: &mut TcpStream, mode: &str, name: &str) -> Result<(), String> {
    write_headers(
        stream,
        &[
            ("PROTO", "XFER2".to_string()),
            ("MODE", mode.to_string()),
            ("NAME", sanitize_name(name)),
        ],
    )
}

fn read_preface(stream: &mut TcpStream) -> Result<(String, String), String> {
    let mut buf = Vec::new();
    let mut one = [0u8; 1];
    while !buf.ends_with(b"\n\n") {
        match stream.read(&mut one) {
            Ok(0) => break,
            Ok(1) => {
                buf.push(one[0]);
                if buf.len() > 4096 {
                    return Err("preface too large".to_string());
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut hdr = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            hdr.insert(k.trim().to_ascii_uppercase(), v.trim().to_string());
        }
    }
    if hdr.get("PROTO").map(String::as_str) != Some("XFER2") {
        return Err("bad preface".to_string());
    }
    let mode = hdr.get("MODE").cloned().unwrap_or_default();
    let name = hdr.get("NAME").cloned().unwrap_or_default();
    if mode.is_empty() {
        return Err("missing mode".to_string());
    }
    Ok((mode, name))
}

fn sanitize_name(name: &str) -> String {
    Path::new(name)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "xfer-incoming.bin".to_string())
}

fn unique_path(dir: &Path, filename: &str) -> PathBuf {
    let path = dir.join(filename);
    if !path.exists() {
        return path;
    }
    let stem = Path::new(filename)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let ext = Path::new(filename)
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    for i in 1..100_000 {
        let p = dir.join(format!("{stem} ({i}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem}-{}.tmp", now_secs()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn iter_files_with_sizes(root: &Path, excludes: &[String]) -> Result<Vec<(PathBuf, String, u64)>, String> {
    let patterns: Vec<Pattern> = excludes.iter().filter_map(|p| Pattern::new(p).ok()).collect();
    let root_abs = fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let parent = root_abs.parent().ok_or_else(|| "missing parent".to_string())?;
    let mut files = Vec::new();

    for ent in WalkDir::new(&root_abs).into_iter().filter_map(Result::ok) {
        if !ent.file_type().is_file() {
            continue;
        }
        let ap = ent.path().to_path_buf();
        let rel = ap
            .strip_prefix(parent)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let base = Path::new(&rel)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let excluded = patterns
            .iter()
            .any(|p| p.matches(&rel) || (!base.is_empty() && p.matches(&base)));
        if excluded {
            continue;
        }
        let sz = ent.metadata().map_err(|e| e.to_string())?.len();
        files.push((ap, rel, sz));
    }
    files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(files)
}

fn recv_meta_string(port: u16, session: Option<&Session>) -> Result<Option<String>, String> {
    let meta_port = port + 1;
    let l = listener(meta_port)?;
    println!("[INFO] Awaiting metadata on port {meta_port} ...");
    let accepted = accept_one(&l, Some(Instant::now() + META_ACCEPT_BUDGET));
    let (stream, _) = match accepted {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let mut data = Vec::new();
    if let Some(sess) = session {
        let mut dr = DecryptReader::new(stream, &sess.key);
        dr.read_to_end(&mut data).map_err(|e| e.to_string())?;
    } else {
        let mut s = stream;
        s.read_to_end(&mut data).map_err(|e| e.to_string())?;
    }
    Ok(Some(String::from_utf8_lossy(&data).to_string()))
}

fn send_meta_string(ip: &str, port: u16, payload: &str, session: Option<&Session>) -> Result<(), String> {
    let meta_port = port + 1;
    let stream = connect_with_retry(ip, meta_port, Duration::from_secs(120))?;
    if let Some(sess) = session {
        let mut ew = EncryptWriter::new(stream, &sess.key);
        ew.write_all(payload.as_bytes()).map_err(|e| e.to_string())?;
        ew.flush().map_err(|e| e.to_string())?;
    } else {
        let mut s = stream;
        s.write_all(payload.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn receive_auto(
    out_path: Option<PathBuf>,
    port: u16,
    force: bool,
    expected: Option<&str>,
    secure: bool,
) -> Result<(), String> {
    let session = if secure { Some(sas_receiver_handshake(port)?) } else { None };
    let status_handle = spawn_status_receiver(port);

    println!("[INFO] Listening on data port {port} ...");
    let l = listener(port)?;
    let (mut conn, _) = accept_one(&l, None)?;
    let (mode, remote_name) = read_preface(&mut conn)?;

    if let Some(exp) = expected {
        if exp != mode {
            return Err(format!("Incoming stream is '{mode}', but receiver expects '{exp}'"));
        }
    }

    let result = match mode.as_str() {
        "file" => receive_file_stream(conn, out_path, force, port, session.as_ref(), &remote_name),
        "dir" => receive_dir_stream(conn, out_path, port, session.as_ref()),
        _ => Err(format!("Unsupported mode: {mode}")),
    };
    if let Some(h) = status_handle {
        let _ = h.join();
    }
    result
}

fn receive_file_stream(
    conn: TcpStream,
    out_path: Option<PathBuf>,
    force: bool,
    port: u16,
    session: Option<&Session>,
    remote_name: &str,
) -> Result<(), String> {
    let sender_name = sanitize_name(remote_name);
    let (tmp_path, final_path_explicit, dest_dir) = match out_path {
        None => {
            let dir = PathBuf::from(".");
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            (unique_path(&dir, &format!(".xfer-tmp-{}.part", now_secs())), None, dir)
        }
        Some(ref p) if p.exists() && p.is_dir() => {
            fs::create_dir_all(p).map_err(|e| e.to_string())?;
            (unique_path(p, &format!(".xfer-tmp-{}.part", now_secs())), None, p.clone())
        }
        Some(p) => {
            if p.exists() && !force {
                return Err(format!("Output file exists: {} (use --force)", p.display()));
            }
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let dir = p.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
            (p.clone(), Some(p), dir)
        }
    };

    let mut out = File::create(&tmp_path).map_err(|e| format!("create {}: {e}", tmp_path.display()))?;
    let mut hasher = Sha256::new();

    if let Some(sess) = session {
        let mut dr = DecryptReader::new(conn, &sess.key);
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = dr.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            hasher.update(&buf[..n]);
        }
    } else {
        let mut stream = conn;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            hasher.update(&buf[..n]);
        }
    }

    let local_hash = hex::encode(hasher.finalize());
    let mut verified = false;
    if let Some(m) = recv_meta_string(port, session)? {
        let mut it = m.trim().splitn(2, char::is_whitespace);
        if let Some(sender_hash) = it.next() {
            if !sender_hash.is_empty() && sender_hash != local_hash {
                let corrupt = unique_path(&dest_dir, &(sender_name.clone() + ".corrupt"));
                fs::rename(&tmp_path, &corrupt).ok();
                return Err(format!("VERIFY FAIL checksum mismatch; corrupt file saved as {}", corrupt.display()));
            } else if !sender_hash.is_empty() {
                verified = true;
            }
        }
    }

    if let Some(final_explicit) = final_path_explicit {
        if final_explicit != tmp_path {
            fs::rename(&tmp_path, &final_explicit).map_err(|e| e.to_string())?;
        }
        println!("[ OK ] Saved: {}", final_explicit.display());
    } else {
        let final_name = if sender_name.is_empty() {
            "xfer-incoming.bin".to_string()
        } else {
            sender_name
        };
        let final_path = if force {
            dest_dir.join(&final_name)
        } else {
            unique_path(&dest_dir, &final_name)
        };
        if force && final_path.exists() {
            fs::remove_file(&final_path).ok();
        }
        fs::rename(&tmp_path, &final_path).map_err(|e| e.to_string())?;
        println!("[ OK ] Saved: {}", final_path.display());
    }
    if verified {
        println!("[ OK ] VERIFY OK — {local_hash}");
    } else {
        println!("[WARN] No checksum received; verification skipped.");
    }
    Ok(())
}

fn safe_rel_path(path: &Path) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => continue,
            _ => return Err(format!("Unsafe path in tar: {}", path.display())),
        }
    }
    Ok(out)
}

fn receive_dir_stream(
    conn: TcpStream,
    out_path: Option<PathBuf>,
    port: u16,
    session: Option<&Session>,
) -> Result<(), String> {
    let out_dir = out_path.unwrap_or_else(|| PathBuf::from("."));
    if out_dir.exists() && !out_dir.is_dir() {
        return Err(format!("--out points to file but directory stream incoming: {}", out_dir.display()));
    }
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;

    let mut local_hashes: HashMap<String, String> = HashMap::new();

    if let Some(sess) = session {
        let dr = DecryptReader::new(conn, &sess.key);
        extract_tar_and_hash(dr, &out_dir, &mut local_hashes)?;
    } else {
        extract_tar_and_hash(conn, &out_dir, &mut local_hashes)?;
    }

    if let Some(mf) = recv_meta_string(port, session)? {
        verify_manifest(&mf, &local_hashes)?;
    }

    println!("[ OK ] Extracted into: {}", out_dir.display());
    Ok(())
}

fn extract_tar_and_hash<R: Read>(reader: R, out_dir: &Path, local_hashes: &mut HashMap<String, String>) -> Result<(), String> {
    let mut ar = Archive::new(reader);
    let entries = ar.entries().map_err(|e| e.to_string())?;
    for item in entries {
        let mut e = item.map_err(|e| e.to_string())?;
        let rel_src = e.path().map_err(|e| e.to_string())?.to_path_buf();
        let rel = safe_rel_path(&rel_src)?;
        let target = out_dir.join(&rel);

        if e.header().entry_type().is_dir() {
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            continue;
        }
        if !e.header().entry_type().is_file() {
            continue;
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = File::create(&target).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = e.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            hasher.update(&buf[..n]);
        }
        let k = rel.to_string_lossy().replace('\\', "/");
        local_hashes.insert(k, hex::encode(hasher.finalize()));
    }
    Ok(())
}

fn verify_manifest(manifest: &str, local_hashes: &HashMap<String, String>) -> Result<(), String> {
    let mut failures = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((h, rel)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let rel = rel.trim();
        match local_hashes.get(rel) {
            None => failures.push(format!("{rel}: missing")),
            Some(v) if v != h => failures.push(format!("{rel}: hash_mismatch")),
            _ => {}
        }
    }
    if failures.is_empty() {
        println!("[ OK ] VERIFY OK — all files match manifest.");
        Ok(())
    } else {
        Err(format!("VERIFY FAIL — {} mismatches (showing first: {})", failures.len(), failures[0]))
    }
}

fn send_file(ip: &str, path: &Path, port: u16, secure: bool) -> Result<(), String> {
    let session = if secure { Some(sas_sender_handshake(ip, port)?) } else { None };

    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "xfer.bin".to_string());

    println!("[INFO] Sending file '{}' -> {}:{}", path.display(), ip, port);
    let mut stream = connect_with_retry(ip, port, Duration::from_secs(30))?;
    write_preface(&mut stream, "file", &filename)?;
    let total = fs::metadata(path).map_err(|e| e.to_string())?.len();
    let status_count = Arc::new(AtomicU64::new(0));
    let status_done = Arc::new(AtomicBool::new(false));
    let status_thread = spawn_status_sender(
        ip.to_string(),
        port,
        status_count.clone(),
        Some(total),
        status_done.clone(),
    );

    let mut hasher = Sha256::new();
    let mut f = File::open(path).map_err(|e| e.to_string())?;

    if let Some(sess) = session.as_ref() {
        let mut ew = EncryptWriter::new(stream, &sess.key);
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            ew.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            status_count.fetch_add(n as u64, Ordering::Relaxed);
        }
        ew.flush().map_err(|e| e.to_string())?;
    } else {
        let mut plain = stream;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            plain.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            status_count.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
    status_done.store(true, Ordering::Relaxed);
    if let Some(h) = status_thread {
        let _ = h.join();
    }

    let hash = hex::encode(hasher.finalize());
    let payload = format!("{hash}  {filename}\n");
    send_meta_string(ip, port, &payload, session.as_ref())?;
    println!("[ OK ] Checksum sent.");
    Ok(())
}

fn send_dir(ip: &str, path: &Path, port: u16, excludes: &[String], secure: bool) -> Result<(), String> {
    let session = if secure { Some(sas_sender_handshake(ip, port)?) } else { None };

    let files = iter_files_with_sizes(path, excludes)?;
    let total_data: u64 = files.iter().map(|(_, _, sz)| *sz).sum();
    println!("[INFO] Sending directory '{}' -> {}:{}", path.display(), ip, port);

    let mut manifest = String::new();
    let mut stream = connect_with_retry(ip, port, Duration::from_secs(30))?;
    let base = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "dir".to_string());
    write_preface(&mut stream, "dir", &base)?;
    let status_count = Arc::new(AtomicU64::new(0));
    let status_done = Arc::new(AtomicBool::new(false));
    let status_thread = spawn_status_sender(
        ip.to_string(),
        port,
        status_count.clone(),
        Some(total_data),
        status_done.clone(),
    );

    if let Some(sess) = session.as_ref() {
        let mut ew = EncryptWriter::new(stream, &sess.key);
        {
            let mut tw = Builder::new(&mut ew);
            for (ap, rel, _sz) in &files {
                let file = File::open(ap).map_err(|e| e.to_string())?;
                let meta = file.metadata().map_err(|e| e.to_string())?;
                let mut hdr = Header::new_gnu();
                hdr.set_size(meta.len());
                hdr.set_mode(0o644);
                hdr.set_cksum();
                let mut hr = HashingReader::new(file);
                tw.append_data(&mut hdr, rel, &mut hr).map_err(|e| e.to_string())?;
                manifest.push_str(&format!("{}  {rel}\n", hr.finalize_hex()));
                status_count.fetch_add(meta.len(), Ordering::Relaxed);
            }
            tw.finish().map_err(|e| e.to_string())?;
        }
        ew.flush().map_err(|e| e.to_string())?;
    } else {
        let mut tw = Builder::new(stream);
        for (ap, rel, _sz) in &files {
            let file = File::open(ap).map_err(|e| e.to_string())?;
            let meta = file.metadata().map_err(|e| e.to_string())?;
            let mut hdr = Header::new_gnu();
            hdr.set_size(meta.len());
            hdr.set_mode(0o644);
            hdr.set_cksum();
            let mut hr = HashingReader::new(file);
            tw.append_data(&mut hdr, rel, &mut hr).map_err(|e| e.to_string())?;
            manifest.push_str(&format!("{}  {rel}\n", hr.finalize_hex()));
            status_count.fetch_add(meta.len(), Ordering::Relaxed);
        }
        tw.finish().map_err(|e| e.to_string())?;
    }
    status_done.store(true, Ordering::Relaxed);
    if let Some(h) = status_thread {
        let _ = h.join();
    }

    send_meta_string(ip, port, &manifest, session.as_ref())?;
    println!("[ OK ] Manifest sent.");
    Ok(())
}

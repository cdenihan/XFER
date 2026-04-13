use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chacha20poly1305::aead::{Aead, KeyInit as AeadKeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use clap::{Parser, Subcommand};
use glob::Pattern;
use hmac::digest::KeyInit as HmacKeyInit;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use tar::{Archive, Builder, Header};
use walkdir::WalkDir;
use x25519_dalek::{PublicKey, StaticSecret};

mod client;
mod server;
mod tui;

type HmacSha256 = Hmac<Sha256>;

const APP_NAME: &str = "XFER";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_PORT: u16 = 9000;
const CHUNK: usize = 4 * 1024 * 1024;
const SAS_CTRL_OFFSET: u16 = 1;
const STATUS_OFFSET: u16 = 2;
const HEARTBEAT_OFFSET: u16 = 3;
const META_ACCEPT_BUDGET: Duration = Duration::from_secs(60);
const SAS_ACCEPT_BUDGET: Duration = Duration::from_secs(120);
const PROGRESS_BAR_WIDTH: usize = 30;

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
    /// Interactive TUI mode
    Tui,
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
        } => server::receive(out, port, force, None, !insecure)?,
        Commands::RecvFile {
            output,
            port,
            force,
            insecure,
        } => server::receive(Some(output), port, force, Some("file"), !insecure)?,
        Commands::RecvDir {
            output_dir,
            port,
            insecure,
        } => server::receive(Some(output_dir), port, false, Some("dir"), !insecure)?,
        Commands::Send {
            receiver_ip,
            path,
            port,
            excludes,
            insecure,
        } => client::send(&receiver_ip, &path, port, &excludes, !insecure)?,
        Commands::Tui => tui::run_tui()?,
    }

    Ok(())
}

pub(crate) fn channel_ports(port: u16) -> (u16, u16, u16, u16) {
    (
        port.saturating_sub(SAS_CTRL_OFFSET),
        port,
        port.saturating_add(1),
        port.saturating_add(2),
    )
}

pub(crate) fn local_ips() -> Vec<String> {
    let mut ips: Vec<String> = Vec::new();
    if let Ok(host) = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| std::env::var("USERDOMAIN"))
    {
        if let Ok(iter) = (host.as_str(), 0).to_socket_addrs() {
            for sa in iter {
                let ip = sa.ip();
                if should_include_ip(ip) {
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
            if should_include_ip((*v4.ip()).into()) {
                let s = v4.ip().to_string();
                if !ips.contains(&s) {
                    ips.push(s);
                }
            }
        }
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

fn should_include_ip(ip: std::net::IpAddr) -> bool {
    ip.is_ipv4() && !ip.is_loopback() && !ip.is_unspecified()
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

    let mut mac =
        <HmacSha256 as HmacKeyInit>::new_from_slice(&key).map_err(|e| format!("hmac: {e}"))?;
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
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| e.to_string())?;
    let v = input.trim().to_ascii_lowercase();
    Ok(v.is_empty() || v == "y" || v == "yes")
}

fn prompt_override(prompt: &str) -> Result<bool, String> {
    print!("{prompt}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| e.to_string())?;
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
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                continue;
            }
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

#[derive(Debug)]
struct TransferProgress {
    sent_bytes: AtomicU64,
    total_bytes: AtomicU64,
    file_sent_bytes: AtomicU64,
    file_total_bytes: AtomicU64,
    files_done: AtomicU64,
    files_total: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
struct ProgressSnapshot {
    sent_bytes: u64,
    total_bytes: u64,
    file_sent_bytes: u64,
    file_total_bytes: u64,
    files_done: u64,
    files_total: u64,
}

impl TransferProgress {
    fn new(total_bytes: u64, files_total: u64) -> Self {
        Self {
            sent_bytes: AtomicU64::new(0),
            total_bytes: AtomicU64::new(total_bytes),
            file_sent_bytes: AtomicU64::new(0),
            file_total_bytes: AtomicU64::new(0),
            files_done: AtomicU64::new(0),
            files_total: AtomicU64::new(files_total),
        }
    }

    fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            sent_bytes: self.sent_bytes.load(Ordering::Relaxed),
            total_bytes: self.total_bytes.load(Ordering::Relaxed),
            file_sent_bytes: self.file_sent_bytes.load(Ordering::Relaxed),
            file_total_bytes: self.file_total_bytes.load(Ordering::Relaxed),
            files_done: self.files_done.load(Ordering::Relaxed),
            files_total: self.files_total.load(Ordering::Relaxed),
        }
    }
}

fn progress_pct(done: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (done as f64 * 100.0) / total as f64
    }
}

fn bar(done: u64, total: u64) -> String {
    if total == 0 {
        return "-".repeat(PROGRESS_BAR_WIDTH);
    }
    let filled = ((done as f64 / total as f64) * PROGRESS_BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(PROGRESS_BAR_WIDTH);
    format!(
        "{}{}",
        "=".repeat(filled),
        "-".repeat(PROGRESS_BAR_WIDTH.saturating_sub(filled))
    )
}

fn human_bytes_per_sec(v: f64) -> String {
    let units = ["B/s", "KiB/s", "MiB/s", "GiB/s"];
    let mut n = v.max(0.0);
    let mut idx = 0usize;
    while n >= 1024.0 && idx + 1 < units.len() {
        n /= 1024.0;
        idx += 1;
    }
    format!("{n:.2} {}", units[idx])
}

fn human_bytes(v: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut n = v as f64;
    let mut idx = 0usize;
    while n >= 1024.0 && idx + 1 < units.len() {
        n /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} {}", v, units[idx])
    } else {
        format!("{n:.2} {}", units[idx])
    }
}

fn eta_string(done: u64, total: u64, speed_bps: f64) -> String {
    if total <= done || speed_bps <= 0.0 {
        return "--:--".to_string();
    }
    let secs = ((total - done) as f64 / speed_bps).ceil() as u64;
    let m = secs / 60;
    let s = secs % 60;
    if m >= 60 {
        let h = m / 60;
        let mm = m % 60;
        format!("{h:02}:{mm:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

fn render_progress(prefix: &str, snap: ProgressSnapshot, speed_bps: f64) -> String {
    let overall_pct = progress_pct(snap.sent_bytes, snap.total_bytes);
    let file_pct = progress_pct(snap.file_sent_bytes, snap.file_total_bytes);
    let overall_bar = bar(snap.sent_bytes, snap.total_bytes);
    let file_bar = bar(snap.file_sent_bytes, snap.file_total_bytes);
    let eta = eta_string(snap.sent_bytes, snap.total_bytes, speed_bps);
    format!(
        "\r[{prefix}] overall [{overall_bar}] {:>6.2}% {} / {} | file [{file_bar}] {:>6.2}% | files {}/{} | {} | ETA {}",
        overall_pct,
        human_bytes(snap.sent_bytes),
        human_bytes(snap.total_bytes),
        file_pct,
        snap.files_done,
        snap.files_total,
        human_bytes_per_sec(speed_bps),
        eta,
    )
}

/// Reads from `reader`, retrying automatically when interrupted by a signal.
///
/// This keeps long-running transfer loops resilient to transient `EINTR`
/// interruptions without treating them as fatal transfer failures.
fn read_retry<R: Read>(reader: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match reader.read(buf) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

fn parse_status_line(line: &str) -> Option<ProgressSnapshot> {
    if line.trim().is_empty() {
        return None;
    }
    let mut sent = None;
    let mut total = None;
    let mut file_sent = None;
    let mut file_total = None;
    let mut files_done = None;
    let mut files_total = None;
    for kv in line.split_whitespace() {
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        let Ok(num) = v.parse::<u64>() else {
            continue;
        };
        match k {
            "sent" => sent = Some(num),
            "total" => total = Some(num),
            "file_sent" => file_sent = Some(num),
            "file_total" => file_total = Some(num),
            "files_done" => files_done = Some(num),
            "files_total" => files_total = Some(num),
            _ => {}
        }
    }
    Some(ProgressSnapshot {
        sent_bytes: sent.unwrap_or(0),
        total_bytes: total.unwrap_or(0),
        file_sent_bytes: file_sent.unwrap_or(0),
        file_total_bytes: file_total.unwrap_or(0),
        files_done: files_done.unwrap_or(0),
        files_total: files_total.unwrap_or(0),
    })
}

fn status_port(port: u16) -> Option<u16> {
    port.checked_add(STATUS_OFFSET)
}

fn heartbeat_port(port: u16) -> Option<u16> {
    port.checked_add(HEARTBEAT_OFFSET)
}

fn spawn_status_receiver(port: u16) -> Option<std::thread::JoinHandle<()>> {
    let p = status_port(port)?;
    Some(std::thread::spawn(move || {
        let Ok(l) = listener(p) else { return };
        let Ok((stream, _)) = accept_one(&l, Some(Instant::now() + Duration::from_secs(180)))
        else {
            return;
        };
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let started = Instant::now();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let msg = line.trim();
                    if !msg.is_empty() {
                        if msg == "done=1" {
                            break;
                        }
                        if let Some(snap) = parse_status_line(msg) {
                            let elapsed = started.elapsed().as_secs_f64().max(0.001);
                            let speed = snap.sent_bytes as f64 / elapsed;
                            let rendered = render_progress("RECV", snap, speed);
                            print!("{rendered}");
                            let _ = io::stdout().flush();
                        }
                    }
                }
                Err(_) => break,
            }
        }
        println!();
    }))
}

fn spawn_status_sender(
    ip: String,
    port: u16,
    progress: Arc<TransferProgress>,
    finished: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    let p = status_port(port)?;
    Some(std::thread::spawn(move || {
        let Ok(mut stream) = connect_with_retry(&ip, p, Duration::from_secs(30)) else {
            return;
        };
        loop {
            let snap = progress.snapshot();
            let msg = format!(
                "sent={} total={} file_sent={} file_total={} files_done={} files_total={}",
                snap.sent_bytes,
                snap.total_bytes,
                snap.file_sent_bytes,
                snap.file_total_bytes,
                snap.files_done,
                snap.files_total
            );
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

fn spawn_local_progress(
    label: &'static str,
    progress: Arc<TransferProgress>,
    done: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let started = Instant::now();
        loop {
            let snap = progress.snapshot();
            let elapsed = started.elapsed().as_secs_f64().max(0.001);
            let speed = snap.sent_bytes as f64 / elapsed;
            let rendered = render_progress(label, snap, speed);
            print!("{rendered}");
            let _ = io::stdout().flush();
            if done.load(Ordering::Relaxed) {
                println!();
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    })
}

fn spawn_heartbeat_receiver(
    port: u16,
    done: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    let p = heartbeat_port(port)?;
    Some(std::thread::spawn(move || {
        let Ok(l) = listener(p) else { return };
        let Ok((stream, _)) = accept_one(&l, Some(Instant::now() + Duration::from_secs(180)))
        else {
            return;
        };
        println!("[INFO] Heartbeat channel connected on port {p}.");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        let mut last_seen = Instant::now();
        while !done.load(Ordering::Relaxed) {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    last_seen = Instant::now();
                    if line.trim() == "done=1" {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    if last_seen.elapsed() > Duration::from_secs(10) {
                        println!("[WARN] heartbeat timeout on port {p}");
                        break;
                    }
                }
            }
        }
        println!("[INFO] Heartbeat channel closed.");
    }))
}

fn spawn_heartbeat_sender(
    ip: String,
    port: u16,
    done: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    let p = heartbeat_port(port)?;
    Some(std::thread::spawn(move || {
        let Ok(mut stream) = connect_with_retry(&ip, p, Duration::from_secs(30)) else {
            return;
        };
        let mut seq: u64 = 0;
        loop {
            let payload = format!("seq={seq} ts={}", now_secs());
            if stream.write_all(payload.as_bytes()).is_err() || stream.write_all(b"\n").is_err() {
                return;
            }
            if done.load(Ordering::Relaxed) {
                let _ = stream.write_all(b"done=1\n");
                return;
            }
            seq = seq.wrapping_add(1);
            std::thread::sleep(Duration::from_secs(1));
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
        &[
            ("NS", ns_hex.clone()),
            ("SPUB", hex::encode(sender_pub.as_bytes())),
        ],
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
        let n = read_retry(&mut self.inner, buf)?;
        if n > 0 {
            self.hasher.update(&buf[..n]);
        }
        Ok(n)
    }
}

struct ProgressHashingReader<R: Read> {
    inner: HashingReader<R>,
    progress: Arc<TransferProgress>,
}

impl<R: Read> ProgressHashingReader<R> {
    fn new(inner: R, progress: Arc<TransferProgress>) -> Self {
        Self {
            inner: HashingReader::new(inner),
            progress,
        }
    }

    fn finalize_hex(self) -> String {
        self.inner.finalize_hex()
    }
}

impl<R: Read> Read for ProgressHashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.progress
                .sent_bytes
                .fetch_add(n as u64, Ordering::Relaxed);
            self.progress
                .file_sent_bytes
                .fetch_add(n as u64, Ordering::Relaxed);
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
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                continue;
            }
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

fn iter_files_with_sizes(
    root: &Path,
    excludes: &[String],
) -> Result<Vec<(PathBuf, String, u64)>, String> {
    let patterns: Vec<Pattern> = excludes
        .iter()
        .filter_map(|p| Pattern::new(p).ok())
        .collect();
    let root_abs = fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let parent = root_abs
        .parent()
        .ok_or_else(|| "missing parent".to_string())?;
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

fn send_meta_string(
    ip: &str,
    port: u16,
    payload: &str,
    session: Option<&Session>,
) -> Result<(), String> {
    let meta_port = port + 1;
    let stream = connect_with_retry(ip, meta_port, Duration::from_secs(120))?;
    if let Some(sess) = session {
        let mut ew = EncryptWriter::new(stream, &sess.key);
        ew.write_all(payload.as_bytes())
            .map_err(|e| e.to_string())?;
        ew.flush().map_err(|e| e.to_string())?;
    } else {
        let mut s = stream;
        s.write_all(payload.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(crate) fn receive_auto(
    out_path: Option<PathBuf>,
    port: u16,
    force: bool,
    expected: Option<&str>,
    secure: bool,
) -> Result<(), String> {
    let session = if secure {
        Some(sas_receiver_handshake(port)?)
    } else {
        None
    };
    let hb_done = Arc::new(AtomicBool::new(false));
    let status_handle = spawn_status_receiver(port);
    let heartbeat_handle = spawn_heartbeat_receiver(port, hb_done.clone());

    println!("[INFO] Listening on data port {port} ...");
    let l = listener(port)?;
    let (mut conn, _) = accept_one(&l, None)?;
    let (mode, remote_name) = read_preface(&mut conn)?;

    if let Some(exp) = expected {
        if exp != mode {
            return Err(format!(
                "Incoming stream is '{mode}', but receiver expects '{exp}'"
            ));
        }
    }

    let result = match mode.as_str() {
        "file" => receive_file_stream(conn, out_path, force, port, session.as_ref(), &remote_name),
        "dir" => receive_dir_stream(conn, out_path, port, session.as_ref()),
        _ => Err(format!("Unsupported mode: {mode}")),
    };
    hb_done.store(true, Ordering::Relaxed);
    if let Some(h) = status_handle {
        let _ = h.join();
    }
    if let Some(h) = heartbeat_handle {
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
            (
                unique_path(&dir, &format!(".xfer-tmp-{}.part", now_secs())),
                None,
                dir,
            )
        }
        Some(ref p) if p.exists() && p.is_dir() => {
            fs::create_dir_all(p).map_err(|e| e.to_string())?;
            (
                unique_path(p, &format!(".xfer-tmp-{}.part", now_secs())),
                None,
                p.clone(),
            )
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

    let mut out =
        File::create(&tmp_path).map_err(|e| format!("create {}: {e}", tmp_path.display()))?;
    let mut hasher = Sha256::new();

    if let Some(sess) = session {
        let mut dr = DecryptReader::new(conn, &sess.key);
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = read_retry(&mut dr, &mut buf).map_err(|e| e.to_string())?;
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
            let n = read_retry(&mut stream, &mut buf).map_err(|e| e.to_string())?;
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
                return Err(format!(
                    "VERIFY FAIL checksum mismatch; corrupt file saved as {}",
                    corrupt.display()
                ));
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
        return Err(format!(
            "--out points to file but directory stream incoming: {}",
            out_dir.display()
        ));
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

fn extract_tar_and_hash<R: Read>(
    reader: R,
    out_dir: &Path,
    local_hashes: &mut HashMap<String, String>,
) -> Result<(), String> {
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
        Err(format!(
            "VERIFY FAIL — {} mismatches (showing first: {})",
            failures.len(),
            failures[0]
        ))
    }
}

pub(crate) fn send_file(ip: &str, path: &Path, port: u16, secure: bool) -> Result<(), String> {
    let session = if secure {
        Some(sas_sender_handshake(ip, port)?)
    } else {
        None
    };

    let filename = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "xfer.bin".to_string());

    println!(
        "[INFO] Sending file '{}' -> {}:{}",
        path.display(),
        ip,
        port
    );
    let mut stream = connect_with_retry(ip, port, Duration::from_secs(30))?;
    write_preface(&mut stream, "file", &filename)?;
    let total = fs::metadata(path).map_err(|e| e.to_string())?.len();
    let progress = Arc::new(TransferProgress::new(total, 1));
    progress.file_total_bytes.store(total, Ordering::Relaxed);
    let status_done = Arc::new(AtomicBool::new(false));
    let heartbeat_done = Arc::new(AtomicBool::new(false));
    let local_progress_done = Arc::new(AtomicBool::new(false));
    let status_thread =
        spawn_status_sender(ip.to_string(), port, progress.clone(), status_done.clone());
    let heartbeat_thread = spawn_heartbeat_sender(ip.to_string(), port, heartbeat_done.clone());
    let local_progress_thread =
        spawn_local_progress("SEND", progress.clone(), local_progress_done.clone());

    let mut hasher = Sha256::new();
    let mut f = File::open(path).map_err(|e| e.to_string())?;

    if let Some(sess) = session.as_ref() {
        let mut ew = EncryptWriter::new(stream, &sess.key);
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = read_retry(&mut f, &mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            ew.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            progress.sent_bytes.fetch_add(n as u64, Ordering::Relaxed);
            progress
                .file_sent_bytes
                .fetch_add(n as u64, Ordering::Relaxed);
        }
        ew.flush().map_err(|e| e.to_string())?;
    } else {
        let mut plain = stream;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = read_retry(&mut f, &mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            plain.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            progress.sent_bytes.fetch_add(n as u64, Ordering::Relaxed);
            progress
                .file_sent_bytes
                .fetch_add(n as u64, Ordering::Relaxed);
        }
    }
    progress.files_done.store(1, Ordering::Relaxed);
    status_done.store(true, Ordering::Relaxed);
    heartbeat_done.store(true, Ordering::Relaxed);
    local_progress_done.store(true, Ordering::Relaxed);
    if let Some(h) = status_thread {
        let _ = h.join();
    }
    if let Some(h) = heartbeat_thread {
        let _ = h.join();
    }
    let _ = local_progress_thread.join();

    let hash = hex::encode(hasher.finalize());
    let payload = format!("{hash}  {filename}\n");
    send_meta_string(ip, port, &payload, session.as_ref())?;
    println!("[ OK ] Checksum sent.");
    Ok(())
}

pub(crate) fn send_dir(
    ip: &str,
    path: &Path,
    port: u16,
    excludes: &[String],
    secure: bool,
) -> Result<(), String> {
    let session = if secure {
        Some(sas_sender_handshake(ip, port)?)
    } else {
        None
    };

    let files = iter_files_with_sizes(path, excludes)?;
    let total_data: u64 = files.iter().map(|(_, _, sz)| *sz).sum();
    println!(
        "[INFO] Sending directory '{}' -> {}:{}",
        path.display(),
        ip,
        port
    );

    let mut manifest = String::new();
    let mut stream = connect_with_retry(ip, port, Duration::from_secs(30))?;
    let base = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "dir".to_string());
    write_preface(&mut stream, "dir", &base)?;
    let progress = Arc::new(TransferProgress::new(total_data, files.len() as u64));
    let status_done = Arc::new(AtomicBool::new(false));
    let heartbeat_done = Arc::new(AtomicBool::new(false));
    let local_progress_done = Arc::new(AtomicBool::new(false));
    let status_thread =
        spawn_status_sender(ip.to_string(), port, progress.clone(), status_done.clone());
    let heartbeat_thread = spawn_heartbeat_sender(ip.to_string(), port, heartbeat_done.clone());
    let local_progress_thread =
        spawn_local_progress("SEND", progress.clone(), local_progress_done.clone());

    if let Some(sess) = session.as_ref() {
        let mut ew = EncryptWriter::new(stream, &sess.key);
        {
            let mut tw = Builder::new(&mut ew);
            for (ap, rel, _sz) in &files {
                let file = File::open(ap).map_err(|e| e.to_string())?;
                let meta = file.metadata().map_err(|e| e.to_string())?;
                progress
                    .file_total_bytes
                    .store(meta.len(), Ordering::Relaxed);
                progress.file_sent_bytes.store(0, Ordering::Relaxed);
                let mut hdr = Header::new_gnu();
                hdr.set_size(meta.len());
                hdr.set_mode(0o644);
                hdr.set_cksum();
                let mut hr = ProgressHashingReader::new(file, progress.clone());
                tw.append_data(&mut hdr, rel, &mut hr)
                    .map_err(|e| e.to_string())?;
                manifest.push_str(&format!("{}  {rel}\n", hr.finalize_hex()));
                progress.files_done.fetch_add(1, Ordering::Relaxed);
            }
            tw.finish().map_err(|e| e.to_string())?;
        }
        ew.flush().map_err(|e| e.to_string())?;
    } else {
        let mut tw = Builder::new(stream);
        for (ap, rel, _sz) in &files {
            let file = File::open(ap).map_err(|e| e.to_string())?;
            let meta = file.metadata().map_err(|e| e.to_string())?;
            progress
                .file_total_bytes
                .store(meta.len(), Ordering::Relaxed);
            progress.file_sent_bytes.store(0, Ordering::Relaxed);
            let mut hdr = Header::new_gnu();
            hdr.set_size(meta.len());
            hdr.set_mode(0o644);
            hdr.set_cksum();
            let mut hr = ProgressHashingReader::new(file, progress.clone());
            tw.append_data(&mut hdr, rel, &mut hr)
                .map_err(|e| e.to_string())?;
            manifest.push_str(&format!("{}  {rel}\n", hr.finalize_hex()));
            progress.files_done.fetch_add(1, Ordering::Relaxed);
        }
        tw.finish().map_err(|e| e.to_string())?;
    }
    status_done.store(true, Ordering::Relaxed);
    heartbeat_done.store(true, Ordering::Relaxed);
    local_progress_done.store(true, Ordering::Relaxed);
    if let Some(h) = status_thread {
        let _ = h.join();
    }
    if let Some(h) = heartbeat_thread {
        let _ = h.join();
    }
    let _ = local_progress_thread.join();

    send_meta_string(ip, port, &manifest, session.as_ref())?;
    println!("[ OK ] Manifest sent.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::Read;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn sanitize_filename_works() {
        assert_eq!(sanitize_name("a/b/c.txt"), "c.txt");
        assert_eq!(sanitize_name(""), "xfer-incoming.bin");
    }

    #[test]
    fn channel_ports_mapping_is_stable() {
        let (ctrl, data, meta, status) = channel_ports(9000);
        assert_eq!(ctrl, 8999);
        assert_eq!(data, 9000);
        assert_eq!(meta, 9001);
        assert_eq!(status, 9002);
        assert_eq!(heartbeat_port(9000), Some(9003));
    }

    #[test]
    fn manifest_verification_passes_and_fails() {
        let mut hashes = HashMap::new();
        hashes.insert("dir/file.txt".to_string(), "abc".to_string());

        let ok = verify_manifest("abc  dir/file.txt\n", &hashes);
        assert!(ok.is_ok());

        let bad = verify_manifest("def  dir/file.txt\n", &hashes);
        assert!(bad.is_err());
    }

    #[test]
    fn status_line_parsing_extracts_progress() {
        let snap = parse_status_line(
            "sent=120 total=200 file_sent=20 file_total=50 files_done=1 files_total=4",
        )
        .expect("snapshot");
        assert_eq!(snap.sent_bytes, 120);
        assert_eq!(snap.total_bytes, 200);
        assert_eq!(snap.file_sent_bytes, 20);
        assert_eq!(snap.file_total_bytes, 50);
        assert_eq!(snap.files_done, 1);
        assert_eq!(snap.files_total, 4);
    }

    #[test]
    fn ip_filter_excludes_loopback_and_unspecified() {
        assert!(should_include_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))));
        assert!(!should_include_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(!should_include_ip(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))));
    }

    #[test]
    fn render_progress_is_human_readable() {
        let snap = ProgressSnapshot {
            sent_bytes: 1024 * 1024,
            total_bytes: 4 * 1024 * 1024,
            file_sent_bytes: 256 * 1024,
            file_total_bytes: 1024 * 1024,
            files_done: 1,
            files_total: 4,
        };
        let line = render_progress("SEND", snap, 1024.0 * 1024.0);
        assert!(line.contains("overall"));
        assert!(line.contains("file"));
        assert!(line.contains("files 1/4"));
        assert!(line.contains("MiB/s"));
        assert!(line.contains("ETA"));
        assert!(line.contains("1.00 MiB / 4.00 MiB"));
    }

    struct InterruptOnceReader {
        interrupted: Cell<bool>,
        data: &'static [u8],
        pos: usize,
    }

    impl Read for InterruptOnceReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted.get() {
                self.interrupted.set(true);
                return Err(io::Error::new(io::ErrorKind::Interrupted, "retry"));
            }
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let remaining = &self.data[self.pos..];
            let n = remaining.len().min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn read_retry_handles_interrupted_io() {
        let mut reader = InterruptOnceReader {
            interrupted: Cell::new(false),
            data: b"abc",
            pos: 0,
        };
        let mut out = [0u8; 8];
        let n = read_retry(&mut reader, &mut out).expect("read should succeed after interrupted");
        assert_eq!(n, 3);
        assert_eq!(&out[..n], b"abc");
    }
}

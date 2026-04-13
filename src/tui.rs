use std::io::{self, Write};
use std::path::PathBuf;

fn ask(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut s = String::new();
    io::stdin().read_line(&mut s).map_err(|e| e.to_string())?;
    Ok(s.trim().to_string())
}

fn ask_bool(prompt: &str, default_yes: bool) -> Result<bool, String> {
    let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
    let v = ask(&format!("{} {} ", prompt, suffix))?;
    if v.is_empty() {
        return Ok(default_yes);
    }
    let l = v.to_ascii_lowercase();
    Ok(l == "y" || l == "yes")
}

fn ask_u16(prompt: &str, default: u16) -> Result<u16, String> {
    let v = ask(&format!("{} [{}]: ", prompt, default))?;
    if v.is_empty() {
        return Ok(default);
    }
    v.parse::<u16>().map_err(|e| format!("invalid port: {e}"))
}

fn print_setup_stats(path: &PathBuf) {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_file() {
            println!("[TUI] setup: file={} bytes={}", path.display(), meta.len());
            return;
        }
    }
    if path.is_dir() {
        let mut files = 0u64;
        let mut bytes = 0u64;
        for ent in walkdir::WalkDir::new(path).into_iter().flatten() {
            if ent.file_type().is_file() {
                files += 1;
                if let Ok(m) = ent.metadata() {
                    bytes += m.len();
                }
            }
        }
        println!("[TUI] setup: dir={} files={} bytes={}", path.display(), files, bytes);
    }
}

pub(crate) fn run_tui() -> Result<(), String> {
    println!("XFER TUI");
    println!("========");
    println!("Simple mode for fast setup; advanced users can customize ports/security.");

    loop {
        println!();
        println!("1) Receive");
        println!("2) Send");
        println!("3) Show local IPs");
        println!("q) Quit");

        let choice = ask("Select option: ")?;
        match choice.as_str() {
            "1" => {
                let out = ask("Output path (blank for current dir): ")?;
                let port = ask_u16("Data port", super::DEFAULT_PORT)?;
                let secure = ask_bool("Use secure mode (TOFU+SAS + Rust E2E encryption)?", true)?;
                let force = ask_bool("Allow overwrite for file mode?", false)?;
                let (ctrl, data, meta, status) = super::channel_ports(port);
                println!(
                    "[TUI] ports: ctrl={} data={} meta={} status={} heartbeat={}",
                    ctrl,
                    data,
                    meta,
                    status,
                    status.saturating_add(1)
                );
                let out_opt = if out.is_empty() { None } else { Some(PathBuf::from(out)) };
                super::server::receive(out_opt, port, force, None, secure)?;
            }
            "2" => {
                let ip = ask("Receiver IP: ")?;
                let path = PathBuf::from(ask("Path to file or directory: ")?);
                let port = ask_u16("Data port", super::DEFAULT_PORT)?;
                let secure = ask_bool("Use secure mode (TOFU+SAS + Rust E2E encryption)?", true)?;
                let excludes = ask("Exclude patterns (comma separated, optional): ")?;
                let excludes_vec: Vec<String> = excludes
                    .split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
                    .collect();
                let (ctrl, data, meta, status) = super::channel_ports(port);
                println!(
                    "[TUI] ports: ctrl={} data={} meta={} status={} heartbeat={}",
                    ctrl,
                    data,
                    meta,
                    status,
                    status.saturating_add(1)
                );
                print_setup_stats(&path);
                super::client::send(&ip, &path, port, &excludes_vec, secure)?;
            }
            "3" => {
                for ip in super::local_ips() {
                    println!("{ip}");
                }
            }
            "q" | "Q" => return Ok(()),
            _ => println!("Invalid selection."),
        }
    }
}

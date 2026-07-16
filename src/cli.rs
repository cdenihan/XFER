use std::{
    cmp::Ordering,
    io::{self, IsTerminal, Write},
    path::PathBuf,
    time::Duration,
};

use anyhow::Context;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};

use crate::{
    config::{Identity, Paths, TrustStore},
    crypto::{display_fingerprint, fingerprint},
    discovery,
    filesystem::build_plan,
    net,
    protocol::DEFAULT_PORT,
    reporter::CliReporter,
    transfer::{ReceiveOptions, SendOptions, human_bytes, receive, send},
    tui, update,
};

#[derive(Debug, Parser)]
#[command(
    name = "xfer",
    version = crate::VERSION,
    about = "Secure file and directory transfer for local networks",
    long_about = "XFER sends files and directories directly between machines over TCP. Transfers use authenticated encryption and remembered peer identities by default."
)]
pub struct Cli {
    /// Store identity and trust data in this directory instead of ~/.xfer.
    #[arg(long, global = true, env = "XFER_CONFIG_DIR")]
    config_dir: Option<PathBuf>,

    /// Emit newline-delimited JSON events instead of interactive progress.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Send one file or directory.
    Send {
        /// Receiver host name or IPv4/IPv6 address.
        host: String,

        /// File or directory to send.
        path: PathBuf,

        /// Receiver TCP port.
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,

        /// Exclude a glob from a directory transfer; repeat as needed.
        #[arg(long)]
        exclude: Vec<String>,

        /// Follow symlinks that remain inside the transfer root.
        #[arg(long)]
        follow_links: bool,

        /// Disable encryption and peer authentication.
        #[arg(long, alias = "no-secure")]
        insecure: bool,

        /// Trust an unseen peer without prompting. Changed identities still require confirmation.
        #[arg(long)]
        accept_new: bool,

        /// Mix a shared secret into session key derivation.
        #[arg(long, env = "XFER_TOKEN", hide_env_values = true)]
        token: Option<String>,

        /// Maximum time to establish a connection.
        #[arg(long, default_value_t = 30)]
        connect_timeout: u64,

        /// Inspect and report the transfer plan without connecting.
        #[arg(long)]
        dry_run: bool,
    },

    /// Receive one file or directory, verify it, and exit.
    Receive {
        /// Directory where the received item will be placed.
        #[arg(short, long, alias = "out", default_value = ".")]
        output: PathBuf,

        /// Local IPv4/IPv6 address to listen on.
        #[arg(long, default_value = "::")]
        bind: String,

        /// TCP port to listen on.
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,

        /// Replace an existing destination with the same name.
        #[arg(long)]
        overwrite: bool,

        /// Do not advertise this receiver to XFER senders on the local network.
        #[arg(long)]
        no_discovery: bool,

        /// Disable encryption and peer authentication.
        #[arg(long, alias = "no-secure")]
        insecure: bool,

        /// Shared secret used by the sender.
        #[arg(long, env = "XFER_TOKEN", hide_env_values = true)]
        token: Option<String>,
    },

    /// Launch the interactive terminal interface.
    Tui,

    /// List useful local IPv4 and IPv6 addresses.
    Ip,

    /// Passively list XFER receivers advertising on the local network.
    Discover {
        /// Seconds to listen for receiver announcements.
        #[arg(
            long,
            default_value_t = 5,
            value_parser = clap::value_parser!(u64).range(1..=60)
        )]
        timeout: u64,
    },

    /// Inspect or remove remembered peers.
    Peers {
        #[command(subcommand)]
        command: PeerCommand,
    },

    /// Check local configuration and networking prerequisites.
    Doctor,

    /// Replace the currently installed executable with an official release.
    Update {
        /// Release version to install, such as 2026.07.16.2.
        #[arg(long, default_value = "latest")]
        version: String,
    },

    /// Generate shell completion scripts.
    Completions {
        /// Shell to generate a completion script for.
        shell: Shell,
    },
}

#[derive(Debug, Subcommand)]
enum PeerCommand {
    /// List remembered receiver identities.
    List,

    /// Forget one endpoint, such as 192.168.1.10:9000.
    Forget { endpoint: String },

    /// Forget every remembered peer.
    Clear {
        /// Confirm removal without an interactive prompt.
        #[arg(long)]
        yes: bool,
    },
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Send {
            host,
            path,
            port,
            exclude,
            follow_links,
            insecure,
            accept_new,
            token,
            connect_timeout,
            dry_run,
        } => {
            if dry_run {
                let plan = build_plan(&path, &exclude, follow_links)?;
                if cli.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "root_name": plan.root_name,
                            "kind": format!("{:?}", plan.kind).to_lowercase(),
                            "total_bytes": plan.total_bytes,
                            "file_count": plan.file_count,
                            "entry_count": plan.entries.len(),
                            "skipped_count": plan.skipped_count,
                        })
                    );
                } else {
                    println!(
                        "{}: {}, {} file(s), {} entries, {} skipped",
                        plan.root_name,
                        human_bytes(plan.total_bytes),
                        plan.file_count,
                        plan.entries.len(),
                        plan.skipped_count
                    );
                }
                return Ok(());
            }
            let reporter = CliReporter::new(accept_new, cli.json);
            let summary = send(
                &SendOptions {
                    host,
                    port,
                    input: path,
                    excludes: exclude,
                    follow_links,
                    secure: !insecure,
                    token,
                    connect_timeout: Duration::from_secs(connect_timeout),
                    config_dir: cli.config_dir,
                },
                &reporter,
            )?;
            reporter.finish();
            print_summary(&summary, cli.json, "sent");
            handle_version_mismatch(&summary, cli.json)?;
        }
        Command::Receive {
            output,
            bind,
            port,
            overwrite,
            no_discovery,
            insecure,
            token,
        } => {
            let reporter = CliReporter::new(false, cli.json);
            let summary = receive(
                &ReceiveOptions {
                    bind,
                    port,
                    output,
                    overwrite,
                    discoverable: !no_discovery,
                    secure: !insecure,
                    token,
                    config_dir: cli.config_dir,
                },
                &reporter,
            )?;
            reporter.finish();
            print_summary(&summary, cli.json, "received");
            handle_version_mismatch(&summary, cli.json)?;
        }
        Command::Tui => tui::run(cli.config_dir)?,
        Command::Ip => {
            let addresses = net::local_addresses()?;
            if cli.json {
                println!("{}", serde_json::to_string(&addresses)?);
            } else if addresses.is_empty() {
                println!("No non-loopback addresses found.");
            } else {
                for address in addresses {
                    println!("{address}");
                }
            }
        }
        Command::Discover { timeout } => {
            let peers = discovery::discover_for(Duration::from_secs(timeout))?;
            if cli.json {
                println!("{}", serde_json::to_string(&peers)?);
            } else if peers.is_empty() {
                println!("No XFER receivers found.");
            } else {
                for peer in peers {
                    let security = if peer.secure { "secure" } else { "insecure" };
                    println!("{}  {}  {security}", peer.name, peer.address);
                }
            }
        }
        Command::Peers { command } => {
            let paths = Paths::discover(cli.config_dir)?;
            handle_peers(command, &paths, cli.json)?;
        }
        Command::Doctor => {
            let paths = Paths::discover(cli.config_dir)?;
            doctor(&paths, cli.json)?;
        }
        Command::Update { version } => {
            if !cli.json {
                if version == "latest" {
                    eprintln!("• checking for the latest XFER release");
                } else {
                    eprintln!("• installing XFER release {version}");
                }
            }
            let summary = update::update_current(&version, cli.json)?;
            print_update_summary(&summary, cli.json)?;
        }
        Command::Completions { shell } => {
            generate(shell, &mut Cli::command(), "xfer", &mut io::stdout());
        }
    }
    Ok(())
}

fn handle_peers(command: PeerCommand, paths: &Paths, json: bool) -> anyhow::Result<()> {
    match command {
        PeerCommand::List => {
            let store = TrustStore::load(paths)?;
            let peers = store
                .iter()
                .map(|(endpoint, peer)| {
                    serde_json::json!({
                        "endpoint": endpoint,
                        "fingerprint": display_fingerprint(&peer.fingerprint),
                        "first_seen": peer.first_seen,
                        "last_seen": peer.last_seen,
                    })
                })
                .collect::<Vec<_>>();
            if json {
                println!("{}", serde_json::to_string(&peers)?);
            } else if peers.is_empty() {
                println!("No remembered peers.");
            } else {
                for peer in peers {
                    println!(
                        "{}  {}",
                        peer["endpoint"].as_str().unwrap_or_default(),
                        peer["fingerprint"].as_str().unwrap_or_default()
                    );
                }
            }
        }
        PeerCommand::Forget { endpoint } => {
            let removed = TrustStore::update(paths, |store| Ok(store.remove(&endpoint)))?;
            if !removed {
                anyhow::bail!("no remembered peer named {endpoint}");
            }
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "ok",
                        "action": "forgot",
                        "endpoint": endpoint,
                    })
                );
            } else {
                println!("Forgot {endpoint}.");
            }
        }
        PeerCommand::Clear { yes } => {
            if !yes {
                anyhow::bail!("refusing to clear every peer without --yes");
            }
            TrustStore::update(paths, |store| {
                store.clear();
                Ok(())
            })?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "ok",
                        "action": "cleared",
                    })
                );
            } else {
                println!("Forgot all peers.");
            }
        }
    }
    Ok(())
}

fn doctor(paths: &Paths, json: bool) -> anyhow::Result<()> {
    paths.ensure()?;
    let identity = Identity::load_or_create(paths)?;
    let identity_fingerprint = display_fingerprint(&fingerprint(identity.public().as_bytes()));
    let addresses = net::local_addresses().context("could not enumerate network interfaces")?;
    let report = serde_json::json!({
        "version": crate::VERSION,
        "config_directory": paths.root(),
        "identity_fingerprint": identity_fingerprint,
        "addresses": addresses,
        "default_port": DEFAULT_PORT,
        "discovery_multicast": discovery::group_address(),
        "status": "ok",
    });
    if json {
        println!("{report}");
    } else {
        println!("XFER {}: OK", crate::VERSION);
        println!("Configuration: {}", paths.root().display());
        println!("Identity: {identity_fingerprint}");
        println!("Default port: {DEFAULT_PORT}");
        println!(
            "LAN discovery: {} (multicast TTL 1)",
            discovery::group_address()
        );
        if addresses.is_empty() {
            println!("Addresses: none detected (loopback transfers still work)");
        } else {
            println!(
                "Addresses: {}",
                addresses
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    Ok(())
}

fn print_summary(summary: &crate::transfer::TransferSummary, json: bool, action: &str) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "event": "complete",
                "action": action,
                "destination": summary.destination,
                "file_count": summary.file_count,
                "total_bytes": summary.total_bytes,
                "peer": summary.peer,
                "peer_version": summary.peer_version,
            })
        );
    } else {
        println!(
            "{} {} across {} file(s) with {}",
            action,
            human_bytes(summary.total_bytes),
            summary.file_count,
            summary.peer
        );
    }
}

fn handle_version_mismatch(
    summary: &crate::transfer::TransferSummary,
    json: bool,
) -> anyhow::Result<()> {
    let Some(peer_version) = summary.peer_version.as_deref() else {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "event": "version_unknown",
                    "local_version": crate::VERSION,
                    "peer_version": null,
                })
            );
        } else {
            eprintln!(
                "WARNING: the peer did not report its XFER release version and is probably using an older release."
            );
            eprintln!("Run `xfer update` on the peer before the next transfer.");
        }
        return Ok(());
    };
    if peer_version == crate::VERSION {
        return Ok(());
    }

    let ordering = update::compare_versions(crate::VERSION, peer_version);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "event": "version_mismatch",
                "local_version": crate::VERSION,
                "peer_version": peer_version,
                "local_is_older": ordering == Some(Ordering::Less),
            })
        );
        return Ok(());
    }

    eprintln!(
        "WARNING: this machine is using XFER {}, but the peer is using XFER {}.",
        crate::VERSION,
        peer_version
    );
    match ordering {
        Some(Ordering::Less) => {
            if !io::stdin().is_terminal() {
                eprintln!(
                    "Run `xfer update --version {peer_version}` to update this installation."
                );
                return Ok(());
            }
            eprint!("Update this installation to XFER {peer_version} now? [y/N] ");
            io::stderr().flush()?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                let update = update::update_current(peer_version, false)?;
                print_update_summary(&update, false)?;
            } else {
                eprintln!("Update skipped. Run `xfer update --version {peer_version}` when ready.");
            }
        }
        Some(Ordering::Greater) => {
            eprintln!(
                "The peer is older. Its CLI will offer to update after the transfer; otherwise run `xfer update --version {}` on that machine.",
                crate::VERSION
            );
        }
        Some(Ordering::Equal) => {}
        None => {
            eprintln!(
                "These version formats cannot be ordered. Run `xfer update` on both machines to align them."
            );
        }
    }
    Ok(())
}

fn print_update_summary(summary: &update::UpdateSummary, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string(summary)?);
    } else if let Some(installed_version) = &summary.installed_version {
        println!(
            "Updated XFER {} → {} at {}",
            summary.previous_version,
            installed_version,
            summary.executable.display()
        );
    } else {
        println!(
            "Update scheduled for {}; XFER will be replaced after this process exits.",
            summary.executable.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn clap_configuration_is_valid() {
        Cli::command().debug_assert();
    }
}

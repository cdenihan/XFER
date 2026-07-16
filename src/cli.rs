use std::{io, path::PathBuf, time::Duration};

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
    tui,
};

#[derive(Debug, Parser)]
#[command(
    name = "xfer",
    version,
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
    let paths = Paths::discover(cli.config_dir.clone())?;
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
        Command::Peers { command } => handle_peers(command, &paths, cli.json)?,
        Command::Doctor => doctor(&paths, cli.json)?,
        Command::Completions { shell } => {
            generate(shell, &mut Cli::command(), "xfer", &mut io::stdout());
        }
    }
    Ok(())
}

fn handle_peers(command: PeerCommand, paths: &Paths, json: bool) -> anyhow::Result<()> {
    let mut store = TrustStore::load(paths)?;
    match command {
        PeerCommand::List => {
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
            if !store.remove(&endpoint) {
                anyhow::bail!("no remembered peer named {endpoint}");
            }
            store.save(paths)?;
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
            store.clear();
            store.save(paths)?;
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
        "version": env!("CARGO_PKG_VERSION"),
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
        println!("XFER {}: OK", env!("CARGO_PKG_VERSION"));
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

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn clap_configuration_is_valid() {
        Cli::command().debug_assert();
    }
}

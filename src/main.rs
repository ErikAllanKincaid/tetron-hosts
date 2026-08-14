//! `tetron-hosts`: syncs tetron peer hostnames into `/etc/hosts` as
//! `<hostname>.<network>`. No DNS server, no port 53 -- reads only
//! `tetron status` over the daemon's existing IPC socket. See this
//! addon's `README.md` for the full design rationale.

mod config;
mod install_wizard;
mod ipc_client;
mod service;
mod sync;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Stamped by build.rs, same pattern as tetron core's/tetron-systray's own
/// `FULL_VERSION`.
pub(crate) const FULL_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_SHA"), ")");

#[derive(Parser)]
#[command(
    name = "tetron-hosts",
    about = "Syncs tetron peer hostnames into /etc/hosts",
    version = FULL_VERSION
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run one sync pass now (writes /etc/hosts). Requires root.
    Sync,
    /// Interactive setup: pick which networks to sync and whether to
    /// schedule automatic runs. Requires root.
    Install,
    /// Remove the scheduled timer/service. Does not remove already-synced
    /// /etc/hosts entries or the saved config -- `sync` after clearing the
    /// allow-list (`config remove-network`) handles that. Requires root.
    Uninstall,
    /// View or edit the network allow-list and scheduling config.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Print the version.
    Version,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Show the current config.
    List,
    /// Add a network (by its `tetron status` display name) to the sync allow-list. Requires root.
    AddNetwork { name: String },
    /// Remove a network from the sync allow-list. Requires root.
    RemoveNetwork { name: String },
}

fn check_root() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("tetron-hosts requires root to write /etc/hosts. Run with sudo.");
        std::process::exit(1);
    }
}

fn print_sync_summary(summary: &sync::SyncSummary) {
    println!(
        "synced {} network(s), {} entries written",
        summary.networks_synced.len(),
        summary.entries_written
    );
    if !summary.networks_missing.is_empty() {
        println!(
            "note: in the allow-list but not currently joined/known, skipped: {}",
            summary.networks_missing.join(", ")
        );
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Sync => {
            check_root();
            match sync::run_sync(&sync::hosts_file_path()).await {
                Ok(summary) => print_sync_summary(&summary),
                Err(e) => {
                    eprintln!("error: {e:#}");
                    std::process::exit(1);
                }
            }
        }
        Command::Install => {
            check_root();
            let existing = config::load()?;
            let cfg = install_wizard::run(&existing).await?;
            config::save(&cfg)?;
            if cfg.scheduled {
                service::install(cfg.interval_secs)?;
            } else {
                // Idempotent -- harmless if nothing was previously installed.
                let _ = service::uninstall();
            }
            println!("running an initial sync pass...");
            match sync::run_sync(&sync::hosts_file_path()).await {
                Ok(summary) => print_sync_summary(&summary),
                Err(e) => eprintln!("initial sync failed (config was still saved): {e:#}"),
            }
        }
        Command::Uninstall => {
            check_root();
            service::uninstall()?;
        }
        Command::Config { action } => match action {
            ConfigAction::List => {
                let cfg = config::load()?;
                println!("scheduled: {}", cfg.scheduled);
                println!("interval_secs: {}", cfg.interval_secs);
                println!(
                    "synced_networks: {}",
                    if cfg.synced_networks.is_empty() {
                        "(none)".to_string()
                    } else {
                        cfg.synced_networks.join(", ")
                    }
                );
            }
            ConfigAction::AddNetwork { name } => {
                check_root();
                let mut cfg = config::load()?;
                if cfg.synced_networks.contains(&name) {
                    println!("'{name}' is already in the sync allow-list");
                } else {
                    cfg.synced_networks.push(name.clone());
                    config::save(&cfg)?;
                    println!("added '{name}' to the sync allow-list (run `tetron-hosts sync` to apply)");
                }
            }
            ConfigAction::RemoveNetwork { name } => {
                check_root();
                let mut cfg = config::load()?;
                let before = cfg.synced_networks.len();
                cfg.synced_networks.retain(|n| n != &name);
                if cfg.synced_networks.len() == before {
                    println!("'{name}' was not in the sync allow-list");
                } else {
                    config::save(&cfg)?;
                    println!("removed '{name}' from the sync allow-list (run `tetron-hosts sync` to apply)");
                }
            }
        },
        Command::Version => println!("tetron-hosts {FULL_VERSION}"),
    }
    Ok(())
}

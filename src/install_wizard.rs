//! Interactive first-install flow: bundles the per-network opt-in picker
//! and the scheduling choice into a single pass. Otherwise "scheduling on
//! by default" would just mean "enabled and syncing nothing" until a
//! second, easy-to-forget manual step -- defeating the point of defaulting
//! it on in the first place.
//!
//! `have_tty`/reading from `/dev/tty` mirrors `install-tetron-suite.sh`'s
//! own trick: stdin is always non-interactive under `curl | bash` even at
//! a real terminal (stdin there is the piped script itself), but
//! `/dev/tty` reaches the controlling terminal directly regardless --
//! this addon behaves the same whether run directly or via that
//! installer's `--install-hosts`.

use std::io::{BufRead, Write};

use anyhow::{Context, Result};

use crate::config::{Config, DEFAULT_INTERVAL_SECS};

pub fn have_tty() -> bool {
    std::fs::File::open("/dev/tty").is_ok()
}

fn read_line_from_tty(question: &str) -> Result<String> {
    print!("{question}");
    std::io::stdout().flush().ok();
    let file = std::fs::File::open("/dev/tty").context("no controlling terminal")?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(line.trim().to_string())
}

/// Runs the interactive wizard, or returns a sensible non-interactive
/// default (scheduling on at the default interval, empty network
/// allow-list -- nothing known yet to opt into) when no controlling
/// terminal is reachable (cron/CI/a container run without `-it`, or
/// `install-tetron-suite.sh`'s own no-TTY path).
pub async fn run(existing: &Config) -> Result<Config> {
    let known_networks: Vec<String> = match crate::sync::fetch_networks().await {
        Ok(nets) => nets.into_iter().map(|n| n.network).collect(),
        Err(_) => Vec::new(),
    };

    if !have_tty() {
        println!(
            "no controlling terminal detected -- defaulting to scheduling on ({}s interval), no networks synced yet",
            DEFAULT_INTERVAL_SECS
        );
        println!(
            "add networks later with: sudo tetron-hosts config add-network <name>"
        );
        return Ok(Config {
            synced_networks: existing.synced_networks.clone(),
            scheduled: true,
            interval_secs: DEFAULT_INTERVAL_SECS,
        });
    }

    let mut synced_networks = existing.synced_networks.clone();
    if known_networks.is_empty() {
        println!("no joined networks known yet -- add some later with: sudo tetron-hosts config add-network <name>");
    } else {
        println!("known networks: {}", known_networks.join(", "));
        let default_hint = if synced_networks.is_empty() {
            "none".to_string()
        } else {
            synced_networks.join(",")
        };
        let answer = read_line_from_tty(&format!(
            "Sync which into /etc/hosts? (comma-separated, 'all', or empty for none) [{default_hint}]: "
        ))?;
        if !answer.is_empty() {
            synced_networks = if answer.eq_ignore_ascii_case("all") {
                known_networks.clone()
            } else {
                answer
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            };
        }
    }

    let scheduled_default = if existing.scheduled { "Y/n" } else { "y/N" };
    let sched_answer = read_line_from_tty(&format!(
        "Enable automatic sync via systemd timer/launchd? [{scheduled_default}]: "
    ))?;
    let scheduled = if sched_answer.is_empty() {
        existing.scheduled
    } else {
        sched_answer.eq_ignore_ascii_case("y")
    };

    let mut interval_secs = existing.interval_secs;
    if scheduled {
        let default_minutes = existing.interval_secs / 60;
        let interval_answer = read_line_from_tty(&format!(
            "Sync interval in minutes [{default_minutes}]: "
        ))?;
        if !interval_answer.is_empty() {
            match interval_answer.parse::<u64>() {
                Ok(minutes) if minutes > 0 => interval_secs = minutes * 60,
                _ => println!("invalid interval, keeping {default_minutes} minutes"),
            }
        }
    }

    Ok(Config {
        synced_networks,
        scheduled,
        interval_secs,
    })
}

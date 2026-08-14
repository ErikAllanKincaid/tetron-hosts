//! Core sync logic: read `tetron status` over IPC, build a
//! `<hostname>.<network>` block per opted-in network, and rewrite
//! `/etc/hosts`'s managed section.
//!
//! **Full block rewrite every pass, not incremental patching.** Every run
//! strips *every* existing `TETRON-HOSTS`-managed block (regardless of
//! which network it names) and rewrites fresh blocks only for networks
//! that are both in the local opt-in list and currently reported by the
//! daemon. A kicked/left peer -- or an entire opted-out network --
//! disappears on the very next run automatically, with no separate
//! cleanup path. Directly informed by a stale-roster-entry problem found
//! and fixed by hand during tetron's own OOM investigation (peers kicked
//! from a network kept generating reconnect activity; nothing pruned
//! stale downstream state on its own).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tetron_proto::ipc::{IpcMessage, NetworkStatus};

use crate::config::Config;
use crate::ipc_client;

const BEGIN_PREFIX: &str = "# BEGIN TETRON-HOSTS ";
const END_PREFIX: &str = "# END TETRON-HOSTS ";

/// `TETRON_HOSTS_FILE` overrides the target path (default `/etc/hosts`),
/// same override-then-fixed-default shape `config::config_dir` uses --
/// lets a chroot/container run, or manual testing, point this elsewhere
/// without touching the real system file.
pub fn hosts_file_path() -> PathBuf {
    std::env::var_os("TETRON_HOSTS_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/etc/hosts"))
}

pub struct SyncSummary {
    pub networks_synced: Vec<String>,
    pub networks_missing: Vec<String>,
    pub entries_written: usize,
}

/// Sanitizes one hostname/network-name label for safe use in an
/// `/etc/hosts` entry (RFC 952/1123-style): lowercase, only
/// `[a-z0-9-]`, no leading/trailing/repeated `-`. `/etc/hosts` fields are
/// whitespace-separated, so an unsanitized name (spaces, odd punctuation)
/// could otherwise corrupt the file's parsing for every other entry in it,
/// not just tetron's own.
fn sanitize_label(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_was_dash = false;
    for c in raw.to_ascii_lowercase().chars() {
        let mapped = if c.is_ascii_alphanumeric() { c } else { '-' };
        if mapped == '-' {
            if last_was_dash || out.is_empty() {
                continue;
            }
            last_was_dash = true;
        } else {
            last_was_dash = false;
        }
        out.push(mapped);
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Fetches the daemon's current status. Bare `anyhow::Error` (not
/// `String`) at this boundary since callers propagate with `?`/`context`.
pub async fn fetch_networks() -> Result<Vec<NetworkStatus>> {
    let resp = ipc_client::call(IpcMessage::Status)
        .await
        .map_err(anyhow::Error::msg)?;
    match resp {
        IpcMessage::StatusResponse { networks, .. } => Ok(networks),
        IpcMessage::Error { message } => anyhow::bail!("daemon error: {message}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// Builds one network's managed block: self + every roster peer with a
/// known hostname, both IPv4 and IPv6 where present. Every roster member
/// (online or not) has a deterministic, identity-derived IP, so this
/// mirrors the whole roster unconditionally -- same reason `tetron
/// status` itself lists offline peers Tailscale-style instead of only
/// live connections.
fn build_block(net: &NetworkStatus) -> Option<String> {
    let net_label = sanitize_label(&net.network);
    if net_label.is_empty() {
        return None;
    }
    let mut lines = Vec::new();

    let mut push_entry = |hostname: Option<&str>, ip4: Option<std::net::Ipv4Addr>, ip6: Option<std::net::Ipv6Addr>| {
        let Some(hostname) = hostname else { return };
        let label = sanitize_label(hostname);
        if label.is_empty() {
            return;
        }
        let fqdn = format!("{label}.{net_label}");
        if let Some(v4) = ip4 {
            lines.push(format!("{v4} {fqdn}"));
        }
        if let Some(v6) = ip6 {
            lines.push(format!("{v6} {fqdn}"));
        }
    };

    push_entry(net.my_hostname.as_deref(), Some(net.my_ip), net.my_ipv6);
    for peer in &net.peers {
        push_entry(peer.hostname.as_deref(), Some(peer.ip), peer.ipv6);
    }

    if lines.is_empty() {
        return None;
    }

    let mut block = String::new();
    block.push_str(BEGIN_PREFIX);
    block.push_str(&net.network);
    block.push('\n');
    for line in &lines {
        block.push_str(line);
        block.push('\n');
    }
    block.push_str(END_PREFIX);
    block.push_str(&net.network);
    block.push('\n');
    Some(block)
}

/// Strips every existing `# BEGIN TETRON-HOSTS ... # END TETRON-HOSTS`
/// block from `existing` (whatever network they name -- treated as one
/// opaque managed region, not patched per-network), then appends the
/// freshly built `blocks`. Never touches anything outside the markers.
fn rewrite_hosts_content(existing: &str, blocks: &[String]) -> String {
    let mut out = String::with_capacity(existing.len());
    let mut in_managed_block = false;
    for line in existing.lines() {
        if line.starts_with(BEGIN_PREFIX) {
            in_managed_block = true;
            continue;
        }
        if line.starts_with(END_PREFIX) {
            in_managed_block = false;
            continue;
        }
        if in_managed_block {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    // Trailing-blank-line hygiene: avoid stacking blank lines across
    // repeated syncs (each pass would otherwise add one more).
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !blocks.is_empty() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        for block in blocks {
            out.push_str(block);
        }
    }
    out
}

/// Atomic write: temp file in the same directory (so the final rename is
/// same-filesystem and therefore atomic), permissions explicitly set to
/// the conventional `/etc/hosts` mode -- `rename` replaces the inode
/// wholesale, so it does not inherit the destination's prior permissions
/// the way an in-place write would.
#[cfg(unix)]
fn write_atomic(path: &Path, content: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let dir = path.parent().context("hosts file path has no parent directory")?;
    let tmp_path = dir.join(format!(
        ".tetron-hosts.tmp.{}",
        std::process::id()
    ));
    std::fs::write(&tmp_path, content)
        .with_context(|| format!("failed to write temp file {}", tmp_path.display()))?;
    std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o644))
        .with_context(|| format!("failed to set permissions on {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to rename {} -> {}", tmp_path.display(), path.display()))?;
    Ok(())
}

/// Runs one full sync pass against `hosts_path` (normally `/etc/hosts`,
/// parameterized for tests). Errors clearly if never configured (no point
/// distinguishing "empty allow-list" from "never ran `install`" any other
/// way -- both otherwise look like a no-op sync).
pub async fn run_sync(hosts_path: &Path) -> Result<SyncSummary> {
    if !crate::config::exists() {
        anyhow::bail!(
            "tetron-hosts is not configured yet -- run `sudo tetron-hosts install` first"
        );
    }
    let cfg: Config = crate::config::load()?;
    let networks = fetch_networks().await?;

    let mut blocks = Vec::new();
    let mut networks_synced = Vec::new();
    let mut networks_missing = Vec::new();
    let mut entries_written = 0usize;

    for wanted in &cfg.synced_networks {
        match networks.iter().find(|n| &n.network == wanted) {
            Some(net) => match build_block(net) {
                Some(block) => {
                    entries_written += block
                        .lines()
                        .filter(|l| !l.starts_with(BEGIN_PREFIX) && !l.starts_with(END_PREFIX))
                        .count();
                    blocks.push(block);
                    networks_synced.push(wanted.clone());
                }
                None => networks_missing.push(wanted.clone()),
            },
            None => networks_missing.push(wanted.clone()),
        }
    }

    let existing = match std::fs::read_to_string(hosts_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", hosts_path.display())),
    };
    let new_content = rewrite_hosts_content(&existing, &blocks);
    write_atomic(hosts_path, &new_content)?;

    Ok(SyncSummary {
        networks_synced,
        networks_missing,
        entries_written,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use tetron_proto::ipc::{NetworkRole, PeerStatus};

    fn fake_endpoint_id() -> iroh::EndpointId {
        iroh::SecretKey::generate().public()
    }

    fn fake_network(name: &str, my_hostname: Option<&str>) -> NetworkStatus {
        NetworkStatus {
            network: name.to_string(),
            role: NetworkRole::Member,
            my_ip: "10.88.0.2".parse().unwrap(),
            my_ipv6: Some("fd00::2".parse::<Ipv6Addr>().unwrap()),
            my_hostname: my_hostname.map(|s| s.to_string()),
            network_key: None,
            member_count: 1,
            peers: vec![PeerStatus {
                endpoint_id: fake_endpoint_id(),
                ip: "10.88.0.3".parse::<Ipv4Addr>().unwrap(),
                ipv6: Some("fd00::3".parse::<Ipv6Addr>().unwrap()),
                hostname: Some("node1".to_string()),
                connection: None,
                is_coordinator: false,
            }],
            tun_name: String::new(),
            nuke_proposals: vec![],
            active: true,
            subnet: "10.88.0.0/24".to_string(),
            nuke_consensus_threshold: 2,
        }
    }

    #[test]
    fn sanitize_label_strips_and_dedupes() {
        assert_eq!(sanitize_label("Testing Delete Me!"), "testing-delete-me");
        assert_eq!(sanitize_label("node1"), "node1");
        assert_eq!(sanitize_label("--weird--"), "weird");
        assert_eq!(sanitize_label(""), "");
    }

    #[test]
    fn build_block_emits_self_and_peer_v4_v6() {
        let net = fake_network("testing-delete-me", Some("myhost"));
        let block = build_block(&net).unwrap();
        assert!(block.starts_with("# BEGIN TETRON-HOSTS testing-delete-me\n"));
        assert!(block.ends_with("# END TETRON-HOSTS testing-delete-me\n"));
        assert!(block.contains("10.88.0.2 myhost.testing-delete-me"));
        assert!(block.contains("fd00::2 myhost.testing-delete-me"));
        assert!(block.contains("10.88.0.3 node1.testing-delete-me"));
        assert!(block.contains("fd00::3 node1.testing-delete-me"));
    }

    #[test]
    fn build_block_skips_self_without_hostname() {
        let net = fake_network("testing-delete-me", None);
        let block = build_block(&net).unwrap();
        assert!(!block.contains("myhost"));
        assert!(block.contains("node1.testing-delete-me"));
    }

    #[test]
    fn rewrite_replaces_only_managed_blocks() {
        let existing = "127.0.0.1 localhost\n\
# BEGIN TETRON-HOSTS old-net\n\
10.0.0.1 stale.old-net\n\
# END TETRON-HOSTS old-net\n\
192.168.1.1 manual-entry\n";
        let new_block = "# BEGIN TETRON-HOSTS testing-delete-me\n\
10.88.0.2 myhost.testing-delete-me\n\
# END TETRON-HOSTS testing-delete-me\n";
        let result = rewrite_hosts_content(existing, &[new_block.to_string()]);
        assert!(result.contains("127.0.0.1 localhost"));
        assert!(result.contains("192.168.1.1 manual-entry"));
        assert!(!result.contains("stale.old-net"));
        assert!(result.contains("myhost.testing-delete-me"));
    }

    #[test]
    fn rewrite_with_no_blocks_just_cleans_stale_ones() {
        let existing = "127.0.0.1 localhost\n\
# BEGIN TETRON-HOSTS old-net\n\
10.0.0.1 stale.old-net\n\
# END TETRON-HOSTS old-net\n";
        let result = rewrite_hosts_content(existing, &[]);
        assert!(result.contains("127.0.0.1 localhost"));
        assert!(!result.contains("stale.old-net"));
        assert!(!result.contains("TETRON-HOSTS"));
    }
}

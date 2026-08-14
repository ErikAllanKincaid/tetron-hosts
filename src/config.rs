//! Persistent config: which joined networks get synced into `/etc/hosts`,
//! and the scheduled-sync interval. Root-owned, since the whole tool only
//! ever runs as root (writing `/etc/hosts` requires it regardless) -- no
//! benefit to a lower-privilege location the way tetron-webui/tetron-systray
//! use `~/.config` for their own per-user configs.
//!
//! Deliberately separate from tetron core's own config (`/etc/tetron/`):
//! the core gate must never couple to addons, and this addon must never
//! write into core's config tree either, matching that same boundary in
//! the other direction.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// 15 minutes -- the agreed default: no strong reason to sync faster,
/// since roster changes (join/leave/kick) are infrequent, deliberate
/// operator actions, and `tetron-hosts sync` is always available to
/// hand-run for the "I want it now" case.
pub const DEFAULT_INTERVAL_SECS: u64 = 900;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Networks (by `tetron status`'s `network` display name) to sync into
    /// `/etc/hosts`. This is the real consent/privacy boundary -- which
    /// networks' hostnames end up in a system-wide file -- independent of
    /// whether syncing is scheduled or manual.
    #[serde(default)]
    pub synced_networks: Vec<String>,
    /// Whether the systemd timer (or launchd periodic job) should be
    /// installed/enabled. `sync` itself always works by hand regardless.
    #[serde(default = "default_true")]
    pub scheduled: bool,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
}

fn default_true() -> bool {
    true
}

fn default_interval() -> u64 {
    DEFAULT_INTERVAL_SECS
}

impl Default for Config {
    fn default() -> Self {
        Config {
            synced_networks: Vec::new(),
            scheduled: true,
            interval_secs: DEFAULT_INTERVAL_SECS,
        }
    }
}

/// `TETRON_HOSTS_CONFIG_DIR` overrides the resolved path, same
/// override-then-fixed-default shape tetron core's own `config::config_dir`
/// uses (`TETRON_CONFIG_DIR`) -- lets tests and manual overrides avoid
/// touching the real `/etc/tetron-hosts`.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("TETRON_HOSTS_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from("/etc/tetron-hosts")
}

fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Loads the config, or `Config::default()` if it does not exist yet (a
/// missing file is not an error -- distinguishes "not configured" from
/// "configured with an empty network list", both of which are valid
/// states reachable via `tetron-hosts install`).
pub fn load() -> Result<Config> {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(raw) => toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
    }
}

/// Whether a config file has ever been written -- `sync` uses this to
/// distinguish "never installed, point at `tetron-hosts install`" from
/// "installed with an empty (still valid) network list".
pub fn exists() -> bool {
    config_path().exists()
}

pub fn save(cfg: &Config) -> Result<()> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let raw = toml::to_string_pretty(cfg).context("failed to serialize config")?;
    let path = config_path();
    std::fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serializes tests that mutate TETRON_HOSTS_CONFIG_DIR, matching
    // tetron core's own CONFIG_ENV_LOCK precedent -- this crate can't
    // reach into core's lock directly since it's a separate crate.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_temp_config_dir<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("tetron-hosts-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe { std::env::set_var("TETRON_HOSTS_CONFIG_DIR", &tmp) };
        let result = f();
        unsafe { std::env::remove_var("TETRON_HOSTS_CONFIG_DIR") };
        let _ = std::fs::remove_dir_all(&tmp);
        result
    }

    #[test]
    fn missing_config_loads_as_default() {
        with_temp_config_dir(|| {
            assert!(!exists());
            let cfg = load().unwrap();
            assert!(cfg.synced_networks.is_empty());
            assert!(cfg.scheduled);
            assert_eq!(cfg.interval_secs, DEFAULT_INTERVAL_SECS);
        });
    }

    #[test]
    fn save_then_load_round_trips() {
        with_temp_config_dir(|| {
            let cfg = Config {
                synced_networks: vec!["testing-delete-me".to_string()],
                scheduled: false,
                interval_secs: 600,
            };
            save(&cfg).unwrap();
            assert!(exists());
            let loaded = load().unwrap();
            assert_eq!(loaded.synced_networks, vec!["testing-delete-me".to_string()]);
            assert!(!loaded.scheduled);
            assert_eq!(loaded.interval_secs, 600);
        });
    }
}

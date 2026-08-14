//! Root-level scheduled-sync install/uninstall: a systemd `.timer` +
//! oneshot `.service` pair on Linux, a periodic (`StartInterval`) launchd
//! **LaunchDaemon** on macOS -- system-wide, not per-user, since writing
//! `/etc/hosts` needs root regardless (unlike `tetron-webui`/
//! `tetron-systray`'s own per-user `systemd --user`/LaunchAgent services,
//! which never need elevated privilege). Mirrors tetron core's own
//! `src/cli/service.rs` shape for exactly that reason: same privilege
//! tier, same file locations.
//!
//! A one-shot `.service` triggered by a `.timer` (rather than a single
//! long-running unit) matches the design decision to keep this addon
//! stateless: each run holds root only for the milliseconds of one sync
//! pass, not continuously.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

fn run_cmd(program: &str, args: &[&str]) {
    match Command::new(program).args(args).status() {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("warning: `{program}` exited with {status}"),
        Err(e) => eprintln!("warning: failed to run `{program}`: {e}"),
    }
}

/// `tetron-hosts install`'s service half: writes/enables the timer+service
/// (Linux) or plist (macOS) for `interval_secs`. Idempotent -- safe to call
/// on every install, keeping the exec path and interval fresh if either
/// changes (same idiom tetron core's own `ensure_service_installed` uses).
pub fn install(interval_secs: u64) -> Result<()> {
    let exe = std::env::current_exe()
        .context("failed to determine current executable path")?
        .to_string_lossy()
        .into_owned();

    #[cfg(target_os = "linux")]
    {
        let service_path = Path::new("/etc/systemd/system/tetron-hosts.service");
        let timer_path = Path::new("/etc/systemd/system/tetron-hosts.timer");
        let service =
            include_str!("../contrib/tetron-hosts.service").replace("/usr/local/bin/tetron-hosts", &exe);
        let timer = include_str!("../contrib/tetron-hosts.timer")
            .replace("__INTERVAL_SECS__", &interval_secs.to_string());
        std::fs::write(service_path, service)
            .with_context(|| format!("failed to write {}", service_path.display()))?;
        std::fs::write(timer_path, timer)
            .with_context(|| format!("failed to write {}", timer_path.display()))?;
        println!(
            "installed systemd units 'tetron-hosts.service'/'tetron-hosts.timer' (every {interval_secs}s)"
        );
        run_cmd("systemctl", &["daemon-reload"]);
        // Enabling/starting the *timer* is what schedules recurring runs;
        // the oneshot .service itself is only ever invoked by the timer
        // (or a manual `systemctl start tetron-hosts.service`), never
        // enabled directly.
        run_cmd("systemctl", &["enable", "--now", "tetron-hosts.timer"]);
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let path = Path::new("/Library/LaunchDaemons/com.tetron.hosts.plist");
        let plist = include_str!("../contrib/com.tetron.hosts.plist")
            .replace("/usr/local/bin/tetron-hosts", &exe)
            .replace("__INTERVAL_SECS__", &interval_secs.to_string());
        std::fs::write(path, plist).with_context(|| format!("failed to write {}", path.display()))?;
        println!("installed launchd job 'com.tetron.hosts' (every {interval_secs}s) -> {}", path.display());
        // Tear down any previously loaded job before loading the fresh one,
        // same reload-safety idiom as core's own install for KeepAlive jobs.
        run_cmd("launchctl", &["unload", &path.to_string_lossy()]);
        run_cmd("launchctl", &["load", "-w", &path.to_string_lossy()]);
        return Ok(());
    }

    #[allow(unreachable_code)]
    {
        anyhow::bail!("scheduled sync is not supported on this platform");
    }
}

/// `tetron-hosts uninstall`: disable/remove the timer+service or plist.
/// Does **not** remove already-synced `/etc/hosts` entries -- that is
/// `sync`'s own job (an empty allow-list on the next manual `sync` clears
/// them), keeping this a pure scheduling-only operation.
pub fn uninstall() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let service_path = Path::new("/etc/systemd/system/tetron-hosts.service");
        let timer_path = Path::new("/etc/systemd/system/tetron-hosts.timer");
        if timer_path.exists() || service_path.exists() {
            run_cmd("systemctl", &["disable", "--now", "tetron-hosts.timer"]);
            if timer_path.exists() {
                std::fs::remove_file(timer_path)?;
            }
            if service_path.exists() {
                std::fs::remove_file(service_path)?;
            }
            run_cmd("systemctl", &["daemon-reload"]);
            println!("Removed systemd units 'tetron-hosts.service'/'tetron-hosts.timer'.");
        } else {
            println!("Scheduled sync was not installed.");
        }
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let path = Path::new("/Library/LaunchDaemons/com.tetron.hosts.plist");
        if path.exists() {
            run_cmd("launchctl", &["unload", "-w", &path.to_string_lossy()]);
            std::fs::remove_file(path)?;
            println!("Removed launchd job.");
        } else {
            println!("Scheduled sync was not installed.");
        }
        return Ok(());
    }

    #[allow(unreachable_code)]
    {
        anyhow::bail!("scheduled sync is not supported on this platform");
    }
}

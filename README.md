# tetron-hosts

Syncs [tetron](https://github.com/ErikAllanKincaid/tetron) peer hostnames into `/etc/hosts` as `<hostname>.<network>` (e.g. `node1.testing-delete-me`), so peers resolve by name without a DNS server. Reads only `tetron status` over the daemon's existing IPC socket — no daemon changes required.

**Not a DNS server, no port 53.** Tetron's own Magic-DNS-equivalent was deliberately removed from core (`MINIMAL-012`) as part of its "do one thing well" philosophy — host naming was left as `/etc/hosts`'s job. This is that seam, done as a standalone, opt-in addon: `/etc/hosts` is already consulted by every resolver on the system, so this needs no new network-facing surface at all.

**Optional and separate from tetron on purpose**, same as [`tetron-webui`](https://github.com/ErikAllanKincaid/tetron-webui)/[`tetron-systray`](https://github.com/ErikAllanKincaid/tetron-systray): tetron itself stays name-resolution-free by default; this is a genuinely separate, opt-in product for anyone who wants `ssh peer-hostname.mynetwork` to just work.

## How it works

- `tetron-hosts sync` reads `tetron status` and rewrites a clearly delimited, atomically-written block in `/etc/hosts` per opted-in network (`# BEGIN TETRON-HOSTS <network>` / `# END TETRON-HOSTS <network>`) — nothing outside those markers is ever touched.
- **Full rewrite every pass, not incremental patching.** Every run regenerates its managed blocks from scratch from the current roster. A peer that gets kicked, leaves, or an entire network you opt back out of disappears from `/etc/hosts` on the very next sync automatically — no separate cleanup step.
- Every roster member (online or not) gets an entry for both its `hostname.network` (IPv4) and, if it has one, the same name (IPv6) — including your own machine's entry. Peer IPs on tetron are deterministic (identity-derived), so this doesn't depend on the peer being currently reachable.
- **Always network-qualified, never a bare hostname** — two different joined networks can both have a "node1" with zero ambiguity, since every entry always carries its network name.

## Per-network opt-in (the real privacy/consent boundary)

Nothing is synced until you explicitly add a network to the allow-list — this is the actual gate on what ends up in a system-wide file, independent of whether syncing itself is scheduled or manual:

```bash
sudo tetron-hosts config add-network testing-delete-me
sudo tetron-hosts config remove-network testing-delete-me
sudo tetron-hosts config list
```

## Scheduling

No persistent daemon — `sync` is a stateless, idempotent one-shot, so it's driven by a systemd timer (Linux) or a periodic launchd job (macOS) rather than a long-running root process holding write privilege continuously. `tetron-hosts sync` is always available to run by hand regardless of whether scheduling is on.

`tetron-hosts install` bundles the network opt-in picker and the scheduling choice into one interactive pass — defaults to **scheduling on, 15-minute interval** (configurable), since the allow-list above is the actual consent boundary, not the timer. Running non-interactively (no controlling terminal — cron, CI, or `install-tetron-suite.sh`'s piped install) skips the prompts and applies that same default with an empty network list; add networks afterward with `config add-network`.

```bash
sudo tetron-hosts install     # interactive: pick networks, scheduling on/off, interval
sudo tetron-hosts uninstall   # removes the timer/service only -- config and /etc/hosts entries are untouched
```

## File locations

| What | Default path |
|---|---|
| **Binary** | `/usr/local/bin/tetron-hosts` |
| **Config** | `/etc/tetron-hosts/config.toml` |
| **Linux service unit** | `/etc/systemd/system/tetron-hosts.service` (oneshot, triggered by the timer) |
| **Linux timer unit** | `/etc/systemd/system/tetron-hosts.timer` |
| **macOS LaunchDaemon** | `/Library/LaunchDaemons/com.tetron.hosts.plist` |
| **Linux logs** | systemd journal (`journalctl -u tetron-hosts`) |
| **macOS logs** | `/var/log/tetron-hosts.log` |

All root-owned — writing `/etc/hosts` needs root regardless, unlike `tetron-webui`/`tetron-systray`'s own per-user services.

## Running it

```bash
curl -Lo tetron-hosts https://github.com/ErikAllanKincaid/tetron-hosts/releases/latest/download/tetron-hosts-linux-x86_64
chmod +x tetron-hosts
sudo install tetron-hosts /usr/local/bin/tetron-hosts
sudo tetron-hosts install
```

Or via [`contrib/install-tetron-suite.sh`](https://github.com/ErikAllanKincaid/tetron/blob/main/contrib/install-tetron-suite.sh) from the tetron repo (`--install-hosts`), alongside core/webui/systray/backup.

## Build from source

```bash
cargo build --release
```

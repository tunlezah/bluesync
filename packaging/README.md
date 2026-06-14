# SoundSync — packaging

## What gets installed where

| Artifact | Path |
|---|---|
| Daemon binary | `/usr/bin/soundsync` |
| Web UI (SPA) | `/usr/share/soundsync/webui/` |
| User systemd unit | `/usr/lib/systemd/user/soundsync.service` |
| System adapter unit | `/usr/lib/systemd/system/soundsync-adapter.service` |
| CoD watcher unit | `/usr/lib/systemd/system/soundsync-adapter-cod.service` |
| CoD watcher script | `/usr/lib/soundsync/soundsync-cod-watch.sh` |
| WirePlumber config | `/etc/wireplumber/wireplumber.conf.d/51-soundsync.conf` (written at install) |
| Per-user config | `~/.config/soundsync/` (created at runtime; not touched by uninstall) |

---

## User-vs-system service split

SoundSync uses **two** systemd services with distinct privilege levels:

### `soundsync.service` — systemd **USER** service (unprivileged)

- Runs as your login user inside the PipeWire/WirePlumber session.
- PipeWire is per-user-session; the daemon must live there to access the audio graph.
- `ExecStart=/usr/bin/soundsync` with `SOUNDSYNC_WEB_DIR` + `SOUNDSYNC_BIND` environment.
- Enabled globally (`systemctl --global enable`) so it starts in every user session automatically.

### `soundsync-adapter.service` — systemd **SYSTEM** oneshot (root)

- Runs once at boot (`After=bluetooth.service`, `WantedBy=bluetooth.target`).
- Executes `soundsync apply-adapter-config` which calls `hciconfig` to set the Bluetooth
  adapter Class-of-Device (`0x240414`, marks the device as an audio sink) and the HCI kernel
  name. These operations require `CAP_NET_ADMIN` and cannot be done from the unprivileged
  user service.
- `RemainAfterExit=yes` so `systemctl status` shows it as active after it completes.
- The user-visible Bluetooth alias is set live over D-Bus by the daemon (no root needed).

### `soundsync-adapter-cod.service` — systemd **SYSTEM** service (root)

- Runs `soundsync-cod-watch.sh`, which **keeps** the adapter Class-of-Device pinned to
  `0x240414` (audio speaker) so A2DP **source** devices reliably present SoundSync as a speaker.
- Needed because on modern bluez (e.g. 5.85) `main.conf [General] Class` is ignored, and
  `bluetoothd` recomputes the adapter class (resetting the major to "Computer") on every
  BlueZ media-endpoint registration — which WirePlumber does at startup and on each device
  connect. A one-time `hciconfig` (the adapter oneshot) is therefore wiped seconds later, and
  the unprivileged user daemon has no `CAP_NET_ADMIN` to re-assert it.
- The watcher re-pins reactively via `dbus-monitor` (BlueZ `PropertiesChanged`/`InterfacesAdded`),
  writing only on drift, with a 30s backstop. `Restart=always`, `WantedBy=bluetooth.target`.
- See `research/audio-routing-nuances.md` (AUD-002 / AUD-002b) for the full root-cause trace.

---

## Maintenance: `--doctor` and `--reset`

The installer doubles as a maintenance tool (run as root, via sudo, **as the appliance user**
so it can reach that user's PipeWire session):

```sh
sudo packaging/install.sh --doctor   # diagnose + safe idempotent repair (OK/WARN/FAIL)
sudo packaging/install.sh --reset    # nuclear reset of the audio pipeline to known-good
sudo packaging/install.sh --help     # all modes + options
```

- **`--doctor`** checks binaries, libspa-bluetooth, the system + user-session services
  (bluetooth, CoD watcher, pipewire/wireplumber/pipewire-pulse, the user daemon), linger, the
  WP A2DP config, conflicting servers (pulseaudio/bluealsa), the adapter CoD + `0x110b`
  advertisement, and the capture sink. It applies safe repairs (start/enable services, rewrite
  the WP config, enable linger) and ends with the daemon's own `soundsync doctor`.
- **`--reset`** stops the pipeline, clears PipeWire/WirePlumber **user state + cache**, removes
  stale soundsync WP configs (both formats), re-applies the config, restarts bluetooth, brings
  the PipeWire stack up in order (`pipewire` → `pipewire-pulse` → `wireplumber`), re-pins the
  adapter, restarts the services, and verifies. **Pairings and installed files are preserved.**

---

## Installing via `.deb` (Debian / Ubuntu / Raspberry Pi OS)

Build the package (requires system `node`, `npm`, `cargo`, `debhelper`):

```sh
# From the repo root (NOT from packaging/):
dpkg-buildpackage -us -uc -b
sudo apt install ../soundsync_*_amd64.deb
```

Or install a pre-built `.deb`:

```sh
sudo apt install ./soundsync_*_amd64.deb
```

> **Note for this host (no system node):** Set `SOUNDSYNC_NODE_BIN=~/.local/node/bin`
> before calling `dpkg-buildpackage`, or build with `--from <prebuilt-dir>` via the
> shell installer instead.

---

## Installing via shell script (non-Debian / manual)

```sh
# Build from source (node auto-detected at ~/.local/node/bin or via SOUNDSYNC_NODE_BIN):
sudo packaging/install.sh

# Skip build; use pre-built artifacts in place (target/release/soundsync + webui/dist):
sudo packaging/install.sh --no-build

# Install from a directory of prebuilt artifacts:
sudo packaging/install.sh --from /path/to/prebuilt/
```

The installer is idempotent — safe to re-run after upgrades.

---

## Uninstalling

```sh
# Remove binary, webui, unit files (keeps WirePlumber config + per-user data):
sudo packaging/uninstall.sh

# Also remove /etc/wireplumber/wireplumber.conf.d/51-soundsync* (per-user ~/.config/soundsync/ is NOT removed):
sudo packaging/uninstall.sh --purge
```

Via `.deb`:

```sh
sudo apt remove soundsync          # remove (keeps WP config)
sudo apt purge soundsync           # also removes WP config
```

---

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `SOUNDSYNC_BIND` | `0.0.0.0:8080` | TCP address + port the HTTP server binds |
| `SOUNDSYNC_WEB_DIR` | `/usr/share/soundsync/webui` | Directory serving the SPA |
| `SOUNDSYNC_NAME` | _(empty — persisted name used)_ | Override device name at startup |
| `SOUNDSYNC_NODE_BIN` | _(unset)_ | Path to node/npm binaries for build steps |

---

## Self-check with `soundsync doctor`

After installing, run as your regular user:

```sh
soundsync doctor
```

Reports Pass / Warn / Fail for: required binaries, PipeWire + WirePlumber running,
WP A2DP config present + correct, Bluetooth adapter present, null-sink reachable,
bind port free, and webui directory exists. Exits non-zero if any check fails.

---

## Port-in-use: using a different port

If port 8080 is taken, create a systemd drop-in override:

```sh
mkdir -p ~/.config/systemd/user/soundsync.service.d
cat > ~/.config/systemd/user/soundsync.service.d/override.conf <<'EOF'
[Service]
Environment=SOUNDSYNC_BIND=0.0.0.0:8081
EOF
systemctl --user daemon-reload
systemctl --user restart soundsync
```

The installer warns if :8080 is already bound when it runs.

---

## Multi-install note

Each user can override the bind port via the drop-in above.  Only one
`soundsync-adapter.service` (root oneshot) exists per machine — it is shared
and runs once regardless of how many user instances are active.

---

## Quick start after install

1. Log out and back in (or `systemctl --user start soundsync`) to start the daemon.
2. Open `http://<hostname>:8080` in a browser from another device on the LAN.
3. Run `soundsync doctor` to verify everything is working.

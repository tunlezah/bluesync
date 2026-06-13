# SoundSync

A headless **Bluetooth A2DP sink** appliance: pair a phone (or any A2DP source), and SoundSync
captures the audio into **PipeWire**, applies a **DSP equaliser**, and makes it available via a
**WebRTC browser stream** and **multiple outputs** (local sound card / AirPlay / Chromecast) — all
controlled from a **Preact web UI**.

Targets headless Raspberry Pi OS and headless Ubuntu Server (validated on Ubuntu 26.04).
Rust daemon + Preact SPA. TLS is rustls/ring (no OpenSSL).

## Features

- Bluetooth A2DP sink (codeless "Just Works" pairing), AVRCP now-playing metadata + album art
- PipeWire capture with a glitch-free DSP EQ
- WebRTC "Listen in the browser" streaming
- Multi-output: local sound card, AirPlay (RAOP), Chromecast (CASTV2)
- Preact web UI: Now Playing, EQ, Devices, Status, output picker, theme picker
- Robust install + self-healing: a Class-of-Device watcher keeps the adapter advertising as a
  speaker; `--doctor` / `--reset` maintenance modes

## Architecture (brief)

The daemon runs as a **systemd user service** inside the user's PipeWire session. A small **root
service** (`soundsync-adapter-cod`) keeps the Bluetooth adapter's Class-of-Device pinned to an
audio speaker. WirePlumber is configured (SPA-JSON on WP 0.5+, Lua on 0.4) to expose the A2DP sink
on headless systems. See `docs/` for the full design, audio-routing nuances, and install runbook.

## Build

```sh
# Rust daemon
cargo build --release

# Web UI (Node 20+/22)
cd webui && npm ci && npm run build
```

## Install (Debian / Ubuntu / Raspberry Pi OS)

```sh
sudo packaging/install.sh                 # apt deps -> build -> install + enable
sudo packaging/install.sh --no-build      # use prebuilt artifacts in place
sudo packaging/install.sh --help          # all modes/options
```

### Maintenance

```sh
sudo packaging/install.sh --doctor        # diagnose + safe repair (run as the appliance user)
sudo packaging/install.sh --reset         # nuclear reset of the audio pipeline
soundsync doctor                          # the daemon's own self-check
```

A `.deb` can be built with `dpkg-buildpackage -us -uc -b` (see `packaging/README.md`).

## Configuration (environment)

| Variable | Default | Purpose |
|---|---|---|
| `SOUNDSYNC_BIND` | `0.0.0.0:8080` | HTTP bind address |
| `SOUNDSYNC_WEB_DIR` | `/usr/share/soundsync/webui` | SPA directory |
| `SOUNDSYNC_NAME` | _(persisted)_ | Initial Bluetooth device name |
| `SOUNDSYNC_HCI` | _(auto-detected)_ | Bluetooth adapter index |
| `SOUNDSYNC_SHUTDOWN_TIMEOUT` | `8` | Hard-exit backstop seconds (< unit `TimeoutStopSec`) |

## Status & roadmap

See **`docs/input.md`** for the current working state and what's left (notably the input-source
selector + per-device face, gated on a live mute-efficacy spike). `docs/` also holds the
implementation plan, the clean-install runbook, and the audio-routing-nuances ledger.

## License

MIT.

# Clean-install runbook — SoundSync (sc-next) on Ubuntu 26.04

> **Purpose.** This is the live capture log of getting `sc-next` to build and run on a
> brand-new, clean Ubuntu 26.04 box. EVERY package installed, file modified, and fix
> applied is recorded here so it can feed a robust, repeatable installer (validate /
> install / reinstall / troubleshoot). Started 2026-06-12.
>
> Format: each action records **what**, **why**, **command**, **result**, and whether it
> should land in the installer (`packaging/install.sh` + `debian/control`) or be a new
> doctor/reset check.

---

## 0. Verified baseline (clean box, before any change)

Captured 2026-06-12, host `applsrv`, user `mark` (uid 1000, in `sudo` group).

- **OS:** Ubuntu 26.04 LTS, x86_64, kernel 7.0.0-22-generic.
- **sudo:** NOT passwordless — needs a password (interactive). Root commands must be run by
  the human (via `! sudo …` at the prompt, or a real terminal).
- **Present already:** `git` 2.53.0, `curl` 8.18.0, `dbus-daemon`.
- **MISSING — build toolchain:** `rustc`, `cargo`, `node`, `npm`, `gcc`/`cc`, `pkg-config`,
  `make`. No `~/.local/node`.
- **MISSING — audio/BT runtime:** `pipewire`, `wireplumber`, `pactl`, `pw-cli`, `wpctl`,
  `bluetoothctl`. (Only `dbus-daemon` present.)

So this is a true bare server: no toolchain, no audio stack. Everything below is net-new.

---

## 1. Dependency analysis (from the manifests, pre-build)

### Build-time (to compile the Rust binary + the SPA)
Derived from `sc-next/Cargo.toml` and `webui/package.json`:

- **Rust stable** (`rust-toolchain.toml` → channel = stable). Install method: TBD (rustup vs apt).
- **Node + npm** for the Preact/Vite SPA (vite ^6 → needs Node ≥18, ideally 20/22). Method TBD.
- **C toolchain + pkg-config** — `build-essential`, `pkg-config`. Needed because:
  - `dbus` 0.9 and `bluer` 0.17 link **system libdbus-1** → `libdbus-1-dev`.
  - `opus` 0.3 links **system libopus** → `libopus-dev`.
- **NOT needed at build time (unlike legacy `sc`):**
  - No `protoc`/protobuf-compiler — the CASTV2 protobuf is inline `#[derive(prost::Message)]`
    with **no `build.rs`** (`src/cast/proto.rs`).
  - No pipewire/spa **dev** libraries — sc-next has no pipewire/spa Rust crate; it drives the
    audio graph by shelling out to `pactl`/`wpctl`/`pw-cli`. (Legacy `sc` linked
    `libpipewire-0.3-dev`/`libspa-0.2-dev`/`libclang-dev`; those are **not** required here.)
  - No `libmp3lame-dev` (legacy used lame; sc-next uses the `opus` crate only).

  → These reductions must be **empirically confirmed by a clean build** before trusting them.

### Runtime (to actually run + capture A2DP audio)
From `debian/control` Depends + the audio path the app drives:

- `pipewire`, `pipewire-pulse`, `pipewire-audio`, `wireplumber` — the audio graph + Pulse shim.
- `libspa-0.2-bluetooth` — **critical**: the PipeWire/SPA BlueZ A2DP plugin (no A2DP capture
  without it). Listed in legacy install, NOT in sc-next `debian/control` — gap to verify.
- `pulseaudio-utils` — `pactl`/`parec` (capture + RMS measurement path, AUD verification).
- `bluez` (+ maybe `bluez-tools`) — BlueZ daemon; `hciconfig` for `apply-adapter-config`
  (Class-of-Device). NOTE: `hciconfig` is deprecated on modern Ubuntu — verify it exists on
  26.04 or the adapter oneshot will fail.
- `ffmpeg` — used for volumedetect/RMS measurement and any transcode in the verify path.
- `avahi-daemon` + `avahi-utils` — mDNS for AirPlay/Chromecast discovery.
- `dbus` (libdbus-1-3) — already present.

---

## 1a. Install-method decisions (agreed with user, 2026-06-12)

- **Toolchain source = distro apt only.** Rust + Node come from Ubuntu 26.04's own repos
  (`apt install cargo rustc nodejs npm`) — no rustup, no NodeSource. Rationale: single
  source, no external repos, mirror-/air-gap-friendly. **Risk to validate:** the shipped
  versions must build vite ^6 and the Rust crate set; if `cargo`/`rustc` lag and a crate
  needs a newer toolchain, that gets captured here as a required fallback.
- **Privilege = temporary passwordless sudo** for this session (`/etc/sudoers.d/mark-temp`),
  revoked at the end. Lets the work drive `apt`/`systemctl` directly. NOT part of the
  installer — the real installer is run with `sudo` once by the operator.

## 2. Actions log (chronological)

### A. Build toolchain — `apt` (2026-06-12)

**What / why:** install Rust, Node, C toolchain, and the two `-dev` libs the crates link.
**Command (run once, as root):**
```sh
sudo apt-get update
sudo apt-get install -y \
    build-essential pkg-config libdbus-1-dev libopus-dev \
    cargo rustc nodejs npm \
    git curl unzip
```
**Result — versions on clean 26.04 (all sufficient, NO fallback needed):**
| tool | version | note |
|---|---|---|
| rustc | 1.93.1 | builds the whole crate set |
| cargo | 1.93.1 | |
| node | v22.22.1 | satisfies vite ^6 |
| npm | 9.2.0 | |
| gcc | 15.2.0 | build-essential |
| pkg-config | 2.5.1 | |
| make | 4.4.1 | |
| libdbus-1 dev | 1.16.2 | `pkg-config --exists dbus-1` ✓ (for `dbus`/`bluer`) |
| libopus dev | 1.6.1 | `pkg-config --exists opus` ✓ (for `opus` crate) |

**Decision confirmed:** distro apt is sufficient for the toolchain on 26.04 — the "versions
may lag" risk did NOT materialise. `cargo`/`rustc` live in `/usr/bin` (no `~/.cargo`/`~/.local/node`
PATH munging needed, unlike the old dev box).

**→ Installer impact:** the build-time apt line is **smaller** than legacy `sc/install.sh`.
Drop `libpipewire-0.3-dev libspa-0.2-dev libclang-dev libmp3lame-dev` (legacy, not needed here)
— pending confirmation by the clean build in step B.

### B. First clean build — discovered a MISSING build dep: `cmake` (2026-06-12)

**What:** `cargo build --release` + `webui` build (`npm ci && npm run build`).
**SPA build:** clean — `npm ci` (236 pkgs, 0 vuln), `vite 6.4.3 build` OK → `webui/dist`.
**Rust build — FIX CAPTURED:** the dependency tree pulls **`webrtc-srtp` → the `cmake`
crate**, whose `build.rs` shells out to the **`cmake` binary**. `build-essential` does NOT
include cmake, so a clean box fails here. Installed it:
```sh
sudo apt-get install -y cmake     # cmake 4.2.3 on 26.04
```
After that the native build proceeds (webrtc-srtp compiled).

**→ Installer impact (BUILD-DEP GAP):** `cmake` MUST be in the build-time apt list /
`debian/control` Build-Depends. It is in NEITHER legacy `sc/install.sh` NOR sc-next
`debian/control` today. **Required build dep, currently undocumented.**

**Confirmed-correct minimal build-time apt set for 26.04:**
`build-essential pkg-config cmake libdbus-1-dev libopus-dev cargo rustc nodejs npm`
(+ `git curl unzip` as fetch helpers). NOT needed: protobuf-compiler, libpipewire/spa/clang/lame-dev.

### C. Runtime audio/BT stack — `apt` (2026-06-12)

**Command:**
```sh
sudo apt-get install -y \
    pipewire pipewire-pulse pipewire-audio wireplumber libspa-0.2-bluetooth \
    pulseaudio-utils bluez bluez-tools ffmpeg avahi-daemon avahi-utils
```
**Result — all available on 26.04, versions much newer than the old dev box:**
| package | 26.04 version | old dev box | note |
|---|---|---|---|
| pipewire / -pulse / -audio | 1.6.2 | 1.0.5 | major jump |
| wireplumber | **0.5.13** | **0.4.17** | **config format change — see below** |
| libspa-0.2-bluetooth | 1.6.2 | — | the A2DP SPA plugin (critical, was a control gap) |
| bluez | 5.85 | — | `hciconfig` STILL present (adapter oneshot OK) |
| bluez-tools | 2.0 | — | |
| ffmpeg | 8.0.1 | — | |
| avahi-daemon/-utils | 0.8 | — | mDNS for AirPlay/Cast |

**Verified post-install:** `pipewire wireplumber pw-cli wpctl pactl parec bluetoothctl
hciconfig avahi-browse ffmpeg` all on PATH; **BT adapter `hci0` present**.

**WirePlumber 0.4→0.5 — NOT a problem (already handled in code):** WP 0.5 dropped Lua
config for SPA-JSON. sc-next is version-aware: `capabilities::version::config_format()`
returns `SpaJson` for `>= (0,5)`, so `apply-wireplumber-config` writes
`/etc/wireplumber/wireplumber.conf.d/51-soundsync.conf` (SPA-JSON, with the critical
`monitor.bluez.seat-monitoring = disabled` for headless). The old box's Lua path
(`bluetooth.lua.d/51-soundsync-a2dp.lua`) is NOT used on 26.04. ✓

**Minor gaps noted:**
- `rfkill` NOT installed — needed to detect/clear a soft-blocked adapter during troubleshooting.
  Candidate add to runtime deps / doctor.

### D. Bring up the PipeWire user session + WP config + linger (2026-06-12)

**What / why:** the daemon must run inside mark's PipeWire session. On a fresh headless box
that session is not auto-started and does not persist without login.

1. **Start the user audio stack** (mark's session was up via SSH; services just needed starting):
   ```sh
   systemctl --user start pipewire.socket pipewire pipewire-pulse wireplumber
   ```
   All three → `active`. (Units are preset-`enabled`, so once linger is on they autostart at boot.)
2. **Apply the WirePlumber A2DP config** (root, writes the SPA-JSON file + removes any stale Lua):
   ```sh
   sudo soundsync apply-wireplumber-config
   # wrote: /etc/wireplumber/wireplumber.conf.d/51-soundsync.conf
   # NUANCE: prints "wireplumber restarted: false" — run as root it CANNOT restart
   #         mark's *user-session* wireplumber. Operator must restart it (or reboot):
   systemctl --user restart wireplumber
   ```
3. **Enable linger** (headless appliance must run with no logged-in user):
   ```sh
   sudo loginctl enable-linger mark    # Linger=yes
   ```
   **→ Installer impact:** `loginctl enable-linger <user>` is REQUIRED for the user
   `soundsync.service` to run at boot without a login. Neither sc-next installer nor
   `debian/postinst` does this today — **GAP**. (The old dev box relied on the `auralis`
   account having linger=yes for the same reason.)

**Result — `soundsync doctor` all green (exit 0):**
`binaries ✓ · pipewire-running ✓ · wp-config ✓ · bt-adapter ✓ · web-dir ✓`

### E. Run the daemon + validate the adapter oneshot (2026-06-12)

**Dev run (as mark):**
```sh
cd /home/mark/sc/sc-next
export SOUNDSYNC_WEB_DIR=$PWD/webui/dist SOUNDSYNC_BIND=0.0.0.0:8080
setsid ./target/release/soundsync >/tmp/ss.log 2>&1 </dev/null & disown
```
**Verified up:** HTTP **200** on :8080; port bound; SPA `index.html` served; the
**`soundsync-capture` null sink created** (id 70, float32le 2ch 48000Hz, IDLE);
`module-raop-discover` loaded (AirPlay available).

**Root adapter oneshot path (`soundsync-adapter.service` → `apply-adapter-config`):**
```sh
sudo soundsync apply-adapter-config     # "adapter configured: hci0 name=SoundSync", exit 0
```
`bluetoothctl show` after → **Class `0x00240414`** (Audio/Video speaker CoD ✓), **Alias
`SoundSync`**, **Powered: yes**. So on 26.04 `hciconfig` CAN still set CoD — the
adapter-config path is functional (NOT broken by the modern bluez/hciconfig deprecation).
(Discoverable/Pairable = no until the daemon opens a pairing window on demand — expected.)

**Expected non-fatal warnings** when the daemon runs as non-root mark (CoD/name need
CAP_NET_ADMIN): `set CoD failed … Operation not permitted`. Harmless — handled by the root
oneshot above. Captured so the installer/doctor does not treat them as errors.

---

## 3. Installer gaps found (feed these into `packaging/` + `debian/`)

These are the concrete deltas the clean-box validation surfaced. None block a manual install
(done above), but a *robust* installer must encode them:

| # | Gap | Where it bites | Fix to encode |
|---|---|---|---|
| G1 | **`cmake` missing from all dep lists** | clean `cargo build` fails at `webrtc-srtp` | add `cmake` to build deps (`debian/control` Build-Depends + any apt step) |
| G2 | **sc-next `packaging/install.sh` installs NO system deps** | it assumes pipewire/bluez/etc. already present; a clean box has none | add an apt dependency-install step (build + runtime sets below) to `install.sh`, or document the prereq apt line |
| G3 | **No `loginctl enable-linger`** | user `soundsync.service` won't run headless after reboot w/o login | `enable-linger` for the target user in installer/postinst |
| G4 | **`apply-wireplumber-config` can't restart user WP when run as root** | install-time WP config doesn't take effect until user WP restart/reboot | postinst should note/trigger `systemctl --user restart wireplumber` for the target user, or instruct reboot |
| G5 | **`libspa-0.2-bluetooth` not in sc-next `debian/control`** | without it there is no A2DP SPA plugin → no capture | add to runtime `Depends` (it IS in legacy install.sh) |
| G6 | **`rfkill` not installed** | can't detect/clear a soft-blocked adapter when troubleshooting | add to runtime deps and/or a doctor check |
| G7 | **CoD set by `apply-adapter-config` (hciconfig) does NOT persist** | bluetoothd reverts the adapter to its default "Computer" class (`0x..010c`) once A2DP services register, because `/etc/bluetooth/main.conf` has `Class` commented out (`#Class = 0x000100`). The daemon then can't re-assert `0x240414` (no CAP_NET_ADMIN). **Cosmetic only** (icon shown on the phone) — history confirms capture works with the Computer CoD. | for a durable audio CoD, write `Class = 0x240414` into `/etc/bluetooth/main.conf` + restart `bluetooth` (the legacy `sc` approach), instead of / in addition to the transient hciconfig oneshot |

### Confirmed-correct apt sets for Ubuntu 26.04 (validated on this box)
```sh
# BUILD-TIME
sudo apt-get install -y build-essential pkg-config cmake \
    libdbus-1-dev libopus-dev cargo rustc nodejs npm git curl unzip
# RUNTIME
sudo apt-get install -y pipewire pipewire-pulse pipewire-audio wireplumber \
    libspa-0.2-bluetooth pulseaudio-utils bluez bluez-tools ffmpeg \
    avahi-daemon avahi-utils rfkill
```

## 4. What is validated vs. still pending a LIVE test

**Validated on clean 26.04:** toolchain install · clean Rust+SPA build · runtime stack ·
PipeWire session · WP A2DP config (0.5 SPA-JSON) · doctor all-green · daemon boots · HTTP/SPA ·
capture null sink · adapter CoD via root oneshot · AirPlay module load.

**Still needs a live/hardware test (not "install" issues, but end-to-end function):**
- Real **A2DP capture** from a phone (connect + play → `bluez_input` node → RMS on
  `soundsync-capture.monitor`).
- **WebRTC** browser "Listen" path from a real browser.
- **AirPlay / Chromecast** output to real devices on the LAN.

---

## 5. G1–G6 fixes APPLIED + hardened installer VALIDATED (2026-06-12)

Applied the captured fixes to the real installer files (sc-next), then ran the installer
end-to-end on this clean box.

### Files changed
- **`debian/control`**
  - Build-Depends += `rustc, cmake, pkg-config, libdbus-1-dev, libopus-dev` (G1 + completeness).
  - Depends += `pipewire-pulse, pipewire-audio, libspa-0.2-bluetooth, bluez-tools,
    avahi-daemon, avahi-utils, rfkill` (G5/G6 + completeness).
- **`packaging/install.sh`**
  - New `--no-deps` flag + `_install_system_deps()` (apt) with `RUNTIME_PKGS`/`BUILD_PKGS`
    arrays — installs the validated sets; `cmake` in BUILD_PKGS (G1/G2/G6).
  - After WP config: `loginctl enable-linger "$SUDO_USER"` (G3) + restart the target user's
    WirePlumber via `sudo -u … XDG_RUNTIME_DIR=… systemctl --user restart wireplumber` (G4),
    with a manual-fallback note when not run via sudo.

### Validation run — `sudo packaging/install.sh --no-build` → exit 0
- **G2** apt deps step ran; only `rfkill` was newly installed (everything else already present). ✓
- **G3** "Enabling linger for mark …" → `Linger=yes`. ✓
- **G4** "restarted mark's WirePlumber — A2DP config now active." (root-run installer DID
  reload the user-session WP — the previously-manual step is now automatic). ✓
- Installed `/usr/bin/soundsync`, `/usr/share/soundsync/webui`, both systemd units; enabled
  `soundsync-adapter.service` (system) + `soundsync.service` (user, `--global`).

### Production service verified
- `systemctl --user start soundsync` → **active (running)**, PID 18653, loaded from
  `/usr/lib/systemd/user/soundsync.service`.
- `:8080` bound by the service; **HTTP 200**; `soundsync-capture` sink present.
- `soundsync-adapter.service` → active (exited), `ExecStart … status=0/SUCCESS` (CoD set at install).
- `soundsync doctor` (installed defaults) → **all 5 green, exit 0**.

**Conclusion:** on a clean Ubuntu 26.04 box, the hardened installer takes it from bare-metal
to a running, doctor-green production service. The original `packaging/install.sh` would have
worked here ONLY because deps were pre-installed by hand and the build was pre-done; the G1–G6
fixes close those gaps so it works from a truly clean box.

### Still not run / pending
- Full `packaging/install.sh` **with** build from a clean checkout (we used `--no-build` since
  artifacts existed; the deps + build path with `cmake` is logically validated but not run as
  one command).
- `.deb` build via `dpkg-buildpackage` (needs `debhelper`; control now lists the right deps).
- Live A2DP / WebRTC / AirPlay / Chromecast hardware tests.

---

## 6. CRITICAL FIX — "won't appear as a speaker": persistent Class-of-Device (G8)

**Symptom (live):** a connected A2DP **source** device (tested with an iPhone, but this is
device-agnostic) does NOT show SoundSync as a speaker and plays audio locally; no
`bluez_card`, no MediaTransport is created.

**Root cause — fully traced, not guessed:**
1. A2DP **source** devices decide whether a peer is a speaker by its **Class-of-Device**.
   SoundSync must advertise `0x240414` (Audio/Video major, Loudspeaker minor). This is the
   long-standing **AUD-002** nuance.
2. On **bluez 5.85 (Ubuntu 26.04)** `main.conf [General] Class` is **IGNORED** — verified by
   test: even a clean major/minor value `0x000414` is not applied after a `bluetooth` restart
   with PipeWire stopped (adapter stays `0x00010c` = Computer/Laptop). (The legacy AUD-002
   note "BlueZ ignores Class=" is correct on this version.)
3. `hciconfig hci0 class 0x240414` **does** set it, but **`bluetoothd` recomputes the adapter
   class on every BlueZ media-endpoint (re)registration** — `kernel-default major (Computer) |
   service bits` — wiping the audio major. **WirePlumber registers its A2DP/HFP endpoints at
   startup AND on every device connect/card creation.** Verified: assert CoD → restart WP →
   reverts to `0x6c010c` (Computer) every time.
4. The SoundSync daemon is an unprivileged systemd **user** service → no `CAP_NET_ADMIN` → it
   logs `set CoD failed … Operation not permitted` and cannot re-assert. A boot-time **oneshot
   cannot fix it** because the revert happens *after* boot, on each endpoint registration.

**The fix (robust, device-agnostic, installed):** a small **root systemd service that re-pins
the CoD reactively** whenever bluetoothd drifts it.
- `packaging/soundsync-cod-watch.sh` → installed `/usr/lib/soundsync/soundsync-cod-watch.sh`:
  asserts `0x240414`, then `dbus-monitor`s BlueZ `PropertiesChanged` + `InterfacesAdded`
  (device connect/disconnect, profile re-registration, adapter Class change) and re-pins on
  drift (writes only when drifted), with a 30s backstop; exits on monitor EOF so systemd
  restarts it.
- `packaging/systemd/soundsync-adapter-cod.service` → `/usr/lib/systemd/system/…`: root,
  `After/Requires/PartOf=bluetooth.service`, `Restart=always`, `WantedBy=bluetooth.target`.
- `install.sh` installs both + `systemctl enable --now soundsync-adapter-cod.service`;
  `uninstall.sh` disables + removes them; `dbus-bin` (provides `dbus-monitor`) added to the
  runtime dep set (apt + `debian/control`).

**Validated live:** service active+enabled; CoD = `0x240414`; **survives WirePlumber restarts**
— journal shows it catching each transient revert (`0x00010c`/`0x6c010c`/`0x68010c`/`0x20010c`)
and re-pinning `0x240414` within the same second.

> **Why reactive, not main.conf or a oneshot:** main.conf is ignored on 5.85; a oneshot is
> wiped by the first WP endpoint registration. Reactive re-pin is the only approach that keeps
> the adapter advertising as a speaker for EVERY source device, across connects and WP churn.
> Residual: a sub-second window after each revert before re-pin — negligible for discovery,
> and devices re-read CoD on (re)connect.

**This was the brittleness.** It is now a maintained, installed, boot-enabled service — fix
G8 in the table below.

**LIVE END-TO-END A2DP CAPTURE — VALIDATED (2026-06-12):** with the CoD watcher running, two
different phones (an Android and the iPhone) both discovered SoundSync **as a speaker** and
routed audio to it. Verified the full path on this clean box:
- `bluez_card.44_4A_DB_B4_E7_0D` created; active profile `audio-gateway` (A2DP Source & HSP/HFP
  AG) — in PipeWire 1.6 this IS the correct receive-from-phone profile (the legacy continue.md
  §3 worry that "only off + audio-gateway is offered" was a misread; audio-gateway is the
  capture profile for a source device).
- Input node `bluez_input.44_4A_DB_B4_E7_0D.2` present; `pw-link` shows
  `bluez_input…:output_FL → soundsync-capture:playback_FL` (phone wired into the capture sink).
- `soundsync-capture.monitor` RMS while playing: **mean −51.9 dB, max −36.7 dB** (real signal).
The "won't appear as a speaker" problem is fully resolved and device-agnostic.

| # | Gap | Fix to encode |
|---|---|---|
| G8 | **Adapter CoD doesn't persist as a speaker** (main.conf ignored on 5.85; bluetoothd reverts it on every WP endpoint registration; user daemon lacks CAP_NET_ADMIN) → source devices won't show/route to SoundSync as a speaker | **DONE** — root `soundsync-adapter-cod.service` + `soundsync-cod-watch.sh` reactively re-pin `0x240414`; wired into install/uninstall; `dbus-bin` added to deps. Supersedes the transient `apply-adapter-config` CoD (G7) for runtime; the oneshot still sets the HCI name. |
| G9 | **webui install accumulates stale content-hashed assets** — `cp -a dist/. WEBUI_DEST/` never removes old `index-*.{js,css}`, so the dir bloats across upgrades | **DONE** — `install.sh` now `find "$WEBUI_DEST" -mindepth 1 -delete` before copying. |

## 7. UI fix — device three-dots menu clipped (2026-06-12)

**Report (user):** opening a device's `⋮` menu in the Devices tab, the dropdown pops *under*
the page / gets cropped for rows near the bottom (top row's menu shows fully; bottom row's is
clipped).

**Root cause:** `.devices-list` (the card) had `overflow: hidden` (to clip flush rows to the
card's rounded corners). The per-device menu is `position:absolute; top: calc(100% + 4px)`
(`.device-menu`, `z-index:200`); for rows near the bottom it extends past the card and was
**clipped by that `overflow: hidden`** — z-index is irrelevant to overflow clipping. No
ancestor (`#app`, `.card`) clips; this was the only one. Confirmed device rows have no
background/hover, so the corner-clip wasn't actually needed.

**Fix:** removed `overflow: hidden` from `.devices-list` (`webui/src/tabs/Devices.css`). Card
corners still rounded via `.card` border-radius; menu (`z-index:200`) now renders above
following rows, uncropped. Rebuilt the SPA (`npm run build`) and deployed to
`/usr/share/soundsync/webui`. **Refresh the browser (hard reload) to pick up the new asset.**

---

## 8. Installer is now also a maintenance tool — `--doctor` / `--reset` (2026-06-12)

Folded the capabilities of the legacy `sc/scripts/soundsync-doctor.sh` and
`soundsync-nuclear-reset.sh` into `packaging/install.sh` as switches (the legacy scripts
targeted the OLD architecture — root `soundsync.service` as `User=mark` in `/opt`, hand-written
`main.conf Class=`, a WP `MemoryDenyWriteExecute=no` override — none of which apply to the
rebuild, so the *capabilities* were re-implemented for the user-service + CoD-watcher model).

`sudo packaging/install.sh --help` now documents three modes:
- **(default) install** — unchanged install flow.
- **`--doctor`** — diagnose + safe idempotent repair. Checks binaries, libspa-bluetooth, system
  services (bluetooth, CoD watcher), user-session services (pipewire/wireplumber/pipewire-pulse,
  the user daemon), linger, the WP A2DP config (`seat-monitoring = disabled`), conflicting
  servers (pulseaudio/bluealsa), adapter CoD `0x240414` + the `0x110b` speaker advertisement, and
  the capture sink; repairs by starting/enabling services, rewriting the WP config, enabling
  linger; ends with the daemon's own `soundsync doctor`. **Validated live: 0 fail, 0 warn.**
- **`--reset`** — nuclear reset: stop pipeline → clear PipeWire/WirePlumber user state+cache →
  remove stale soundsync WP configs (both formats) → re-apply config → restart bluetooth → bring
  the PipeWire stack up in order → re-pin adapter → restart services → verify. Pairings +
  installed files preserved. **Validated live: came back fully green (web 200, 0x110b, CoD pinned).**

**Design note (run-as model):** both modes require root *and* must run via `sudo` as the
appliance user (`SUDO_USER`) — they use a `run_user()` helper (`sudo -u <user> XDG_RUNTIME_DIR=…
DBUS_SESSION_BUS_ADDRESS=… …`) for every user-session command, because the rebuild's daemon is a
systemd **user** service. Legacy facts deliberately dropped on 26.04: the `bluetooth`/`audio`
group membership (not required — verified) and the `MemoryDenyWriteExecute=no` override (basic
A2DP/SBC works with MDWE=yes; doctor only *reports* it as a latent LDAC/aptX consideration).

**Also fixed in this pass:** the installer's port-in-use check no longer false-warns on our own
running daemon (`grep -qv 'soundsync'`); `--reset` does a user `daemon-reload` before starting
the daemon.

## 9. UI feature — sticky, type-accurate device icons (2026-06-12)

**Request:** device icons should be **sticky** (keep the right glyph after disconnect, not
revert to generic), plus a **turntable** icon for the Audio-Technica Sound Burger and a
**cassette/Walkman** icon for a cassette player.

**Before:** `deviceIcon()` was keyed on connection *state* (`📱` connected / `📶` not) — it never
knew the device type, so it couldn't be type-correct *or* sticky.

**Fix (robust path, chosen by user over a name-only heuristic):** plumb BlueZ's own device
`Icon` through the backend so the type is accurate for every device and persists while paired.
- **Backend:** `BluezEvent::DeviceAdded`, `DeviceSnapshot`, and `DeviceInfo` gain an
  `icon: Option<String>`; populated from `bluer` `dev.icon()` at the three `DeviceAdded` emit
  sites (`bluer_devices.rs`); only overwritten when an event carries a value (sticky). Flows
  snapshot → `DeviceInfo` → WS → UI. **445 Rust tests pass; release builds clean; fmt clean.**
  Verified live: `/api/status` reports `icon:"phone"` for BOTH the iPhone and the custom-named
  "Rotormola" Android, **even while disconnected**.
- **Frontend (`Devices.tsx`):** `deviceIcon(device)` resolves kind by: (1) name override for
  **turntable** (`sb727`/`sound burger`/`vinyl`/…) and **cassette** (`cassette`/`walkman`/
  `tape`/…) — types BlueZ can't distinguish (it calls them `audio-card`); (2) BlueZ `icon` →
  phone/headphones/computer; (3) a looser name backstop; (4) a localStorage sticky cache. Icons:
  📱 🎧 🔊 💻 💿 (turntable) 📼 (cassette) 🎵. Connection state stays on the separate status dot.
  Typecheck clean, 27 web tests pass.

**Result:** iPhone 📱, Rotormola 📱, Sound Burger 💿, cassette 📼 — all correct and sticky.

### Final installer inventory (all under `sc-next/`, all validated on clean 26.04)
- `packaging/install.sh` — install + `--doctor` + `--reset` + `--help`; apt deps (build+runtime);
  cmake/dbus-bin/libspa-bluetooth included; linger; user-WP restart; CoD watcher install; webui
  clear-before-copy.
- `packaging/soundsync-cod-watch.sh` + `packaging/systemd/soundsync-adapter-cod.service` — the
  CoD watcher (G8).
- `packaging/uninstall.sh` — removes the watcher service+script too.
- `debian/control` — Build-Depends + Depends corrected (cmake, rustc, pkg-config, libdbus-1-dev,
  libopus-dev / libspa-0.2-bluetooth, pipewire-pulse, pipewire-audio, bluez-tools, avahi, rfkill,
  dbus-bin).
- `packaging/README.md` — documents the CoD watcher service + `--doctor`/`--reset`.




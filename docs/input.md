# input.md — SoundSync rebuild: implementation status & handoff

> Written 2026-06-13. Single source of truth for: the input-source / mute-efficacy work,
> the crash-loop regression and its fix, what is done vs left, and how to resume.
> Companion docs: `research/plans/2026-06-13-implementation-research-plan.md` (the full plan),
> `research/clean-install-2604-runbook.md` (the clean-install + installer capture),
> `research/audio-routing-nuances.md` (the audio crown-jewel ledger), `continue.md` (older handoff).

---

## 0. TL;DR — current state

The appliance is **healthy and stable**: A2DP capture, metadata, spectrum, EQ, all 3 outputs, and
the **browser WebRTC "Listen" path all work**. A regression I introduced (an 8-second restart
loop) was found and fixed; the daemon now runs indefinitely and shuts down cleanly in ~1.5s.

Two of the four planned waves are landed and deployed. The **input-source selector + per-device
face is NOT built** — it is gated on a live spike (**IN-0**) that has **not yet been run to
completion** (the crash loop interrupted it). Everything is committed-ready and the gate is green.

**Gate (all green, 2026-06-13):** Rust `cargo build` clean · **457 lib + 4 bin tests** ·
`cargo fmt --check` clean · webui `tsc` clean · **28 web tests** · `vite build` OK.

---

## 1. Mute-efficacy (IN-0) — the crown gate — STATUS: NOT YET MEASURED

**What it is.** The input-source selector must keep exactly ONE connected A2DP source audible at a
time. The design's preferred mechanism (A) is to **mute the held `bluez_input.<MAC>` source node**
(`pactl set-source-mute` / `wpctl set-mute`) so it contributes silence to the capture mix while
staying connected. IN-0 is the live de-risk that decides whether mechanism A actually works or we
must fall back to mechanism C (route held sources to a separate discard null-sink).

**Why it must be measured live (not assumed).** Two real risks (plan §8 C2):
1. Muting the source *node* may not zero its contribution to `soundsync-capture.monitor` — the
   source→default-sink **link** is what feeds the capture mix, and the topology **differs EQ-off
   vs EQ-on**. Must test BOTH.
2. With `bluez5.enable-hw-volume=true` (our WP config), a source-side mute may **round-trip over
   AVRCP/AVCTP to the phone itself** (pausing/muting the user's phone) — unacceptable.

**The exact procedure to run (when a source is actively streaming to SoundSync):**
```
# 1. find the streaming source node
NAME=$(pactl list short sources | awk '/bluez_input/{print $2; exit}')
# 2. baseline RMS on the capture monitor (track playing)
timeout 2 parec -d soundsync-capture.monitor --format=s16le --rate=48000 --channels=2 /tmp/b.raw
ffmpeg -f s16le -ar 48000 -ac 2 -i /tmp/b.raw -af volumedetect -f null - 2>&1 | grep mean_volume
# 3. mute the source, re-measure (mechanism A works IFF this is ~silence)
pactl set-source-mute "$NAME" 1 ; sleep 0.7 ; <measure again>
# 4. unmute, re-measure (signal returns)
pactl set-source-mute "$NAME" 0 ; sleep 0.7 ; <measure again>
# 5. REPEAT steps 2-4 with EQ ON (toggle EQ in the UI / via /api) — different topology
# 6. While muted, HUMAN observes the phone: did its playback pause or its volume change?
```
**Verdict needed:** A (mute, recommended) vs C (discard-sink). This gates ALL of Wave 3.

**Why it isn't done yet.** During the first attempt the daemon was crash-looping (see §2), so no
`bluez_input` was stable enough to measure, and the measurement window kept resetting. The host is
now stable, so the next session can run it cleanly the moment a source is playing. **NOTE:** when
re-running, confirm the phone's audio output is actually routed to SoundSync (a `bluez_input`
source node appears in `pactl list short sources`) — capture working (spectrum/metadata) is the
signal that it is; do NOT chase the phone if capture is already flowing.

---

## 2. The crash-loop regression — ERROR, ROOT CAUSE, FIX (resolved)

**Symptom (reported):** browser audio "broken", "nothing else working correctly", while the sink,
metadata, and visual EQ all showed. The phone/connection were FINE.

**Root cause (mine).** The Wave 0 graceful-shutdown change wrapped the **forever-running**
`run_daemon` main loop in `tokio::time::timeout(8s, run_daemon(...))` (`main.rs`). `run_daemon`
only returns after a shutdown is requested, so the 8s timer always elapsed during NORMAL
operation → `eprintln!("shutdown did not complete within 8s — forcing exit")` → `std::process::
exit(0)` → systemd `Restart=always` respawned it → a restart every ~8-10s (NRestarts hit 53).
Capture/metadata/spectrum recovered within ~1s each cycle (so they *looked* fine), but the
**WebRTC peer connection the browser holds cannot survive a daemon that dies every 8s** — that was
the "broken browser audio."

**Why it slipped the gate.** The agent's two unit tests exercised `tokio::time::timeout` *in
isolation* (which trivially passes), not how `main` actually used it. False confidence.

**Fix (`main.rs`).** The 8s backstop now starts counting **only after shutdown is requested**:
`run_daemon` is raced (via `tokio::select!`) against a `backstop` future that first
`wait_for`s the shutdown flag and only THEN `sleep(timeout)`s. During normal operation the
backstop parks forever; on shutdown the clean path returns in ~1.5s and wins the race; only a
genuinely hung shutdown trips the force-exit. Replaced the misleading tests with three that guard
the real semantics: (1) **never fires during normal operation**, (2) fires on a hung shutdown
after a request, (3) a prompt clean shutdown wins.

**Verified:** daemon stable (same PID across 20s+, `NRestarts=0`, zero forced exits); clean stop
measured at **1.49s** (well under the unit's `TimeoutStopSec=15`, no SIGKILL); browser confirmed
working by the user.

---

## 3. What is DONE + deployed + verified

### Wave 0 — foundations
- **ThreadSleeper async fix.** The null-sink backoff is now `tokio::time::sleep().await`;
  `ensure_null_sink`/`bring_up` are async; the `Sleeper` generic was dropped from the hot path and
  **`src/sys/clock.rs` was deleted** (became dead code). Tests migrated to `#[tokio::test(start_paused)]`.
- **Graceful-shutdown fix (FULL drain) — now CORRECT.** Hard-exit backstop (8s, post-request only),
  shutdown-aware `run_daemon`, `axum ... with_graceful_shutdown`, `ws_connection` observes shutdown,
  new `WebRtcController::stop_all()` (+ Fake) called on shutdown, unit `TimeoutStopSec=15` +
  `KillMode=mixed`, optional `SOUNDSYNC_SHUTDOWN_TIMEOUT` env.

### Wave 1 — independent
- **4 new color schemes** (total 7): Slate Blue, Royal Magenta, Forest Lime, Desert Red. All
  contrast-corrected to pass WCAG AA (the raw `#a855f7`/`#16a34a`/`#c0563a` failed and were
  darkened); `readAccent` widened so they persist; `.accent-swatches` got `flex-wrap` so 7 don't
  overflow on phones; `themes.test.ts` asserts 7 + no-dup-ids + accent-dim/accent2 presence.
- **Interface version 0.2.0**, surfaced: vite `__APP_VERSION__` define + a Status row
  (Backend vs Interface).
- **`config_store` persistence primitive** (`src/config_store.rs`): generic `load_json`/`save_json`
  under `~/.config/soundsync/`, reusing the `sys::fs::Fs` seam + `FakeFs`, mirroring
  `name_config.rs`. **Tested, but DORMANT — no consumers wired yet** (intended for devices.json /
  null-sink + a2dp toggles in later waves). Harmless at runtime.

### Wave 2 (partial)
- **W2.1 null-sink STARTUP sweep.** `parse_all_null_sink_module_ids` + `sweep_duplicate_null_sinks`
  (keeps the canonical module id `ensure_null_sink` returns, unloads any other
  `module-null-sink sink_name=soundsync-capture`). Wired into the startup path only. **Live note:**
  ran at the last startup without issue; it's a no-op while there is one sink.

### Earlier this session (already deployed)
- Robust **CoD watcher** (`soundsync-adapter-cod.service` + `soundsync-cod-watch.sh`) — reactively
  pins adapter Class `0x240414` so source devices see a speaker.
- **Sticky, BlueZ-icon-based device icons** (phone/headphones/speaker/computer + turntable/cassette
  name overrides incl. `kxrm`/`sb727`); the backend now sends BlueZ `icon` in `DeviceInfo`.
- **Device menu z-index/overflow fix** (`.devices-list` no longer clips the dropdown).
- **Installer is also a maintenance tool**: `sudo packaging/install.sh --doctor | --reset | --help`.

---

## 4. What is LEFT to do

| Item | Wave | Gated on | Notes |
|---|---|---|---|
| **IN-0 spike** (mute efficacy, EQ off+on, phone round-trip) | gate | a streaming source + human | §1. Decides mechanism A vs C. Blocks all of Wave 3. |
| **W2.2 a2dp-role doctor check** | 2 | — | Strengthen `doctor.rs` wp-config check (content, ConfigFormat-aware) + live `0x110b` probe (Warn not Fail). Cited `apply_config(format, fs, runner)` (in `install.rs:19`), NOT `apply.rs`. |
| **Null-sink PERIODIC reconcile** + UI interval toggle | 2 | a LIVE null-sink survivor spike | Deferred: with audio playing, load a duplicate and see which sink the live links/monitor stay on, to know the survivor rule while streaming. Uses `config_store`. |
| **Input-source selector + per-device face (IN-1..IN-5)** | 3 | IN-0 verdict | The big feature. `InputController` (mirror `OutputController`), sticky/auto-switch, AVRCP coupling **additive-only**, `devices.json` faces (supersedes the hardcoded icon name-regex). |
| **Wire `config_store` to consumers** | 2/3 | — | Currently dormant. |
| **Backend version bump** `Cargo.toml 0.0.1→0.0.2` + `debian/changelog` | 3 | when W3 adds a backend command | Per plan §6. |

**Open questions still to resolve** (plan §9): config-persistence shape is decided (one shared
primitive — done), but: shutdown timeout values (8s/15s — chosen, verified), graceful WS/WebRTC
drain (chosen: in — done), per-device "name" scope + `POST /api/input/none` in v1?, auto-switch
debounce (2s vs 3s, decide after the two-source live test), `active_input` persistence across
restarts.

---

## 5. Build order remaining (from the plan, adjusted)

1. **IN-0 spike** (human + streaming source) → verdict A/C.
2. **W2.2** a2dp doctor (independent; can be done anytime).
3. **Live null-sink survivor spike** → then the **periodic reconcile** + UI toggle.
4. **Wave 3** input-source (IN-1→IN-5), gated by IN-0; keep AVRCP coupling additive; new
   snapshot fields MUST be `Option`/`#[serde(default)]` to protect the gate + `types.ts` mirror;
   avoid the `/api/bluetooth/:addr/appearance` vs `:addr/:action` route collision.

---

## 6. Installer / robustness state

`packaging/install.sh` (default / `--no-build` / `--no-deps` / `--from` / `--doctor` / `--reset`)
installs: apt deps (build + runtime, incl. `cmake`, `dbus-bin`, `libspa-0.2-bluetooth`), the binary,
webui (clear-before-copy), the user unit, the adapter oneshot, the **CoD watcher** service+script,
the WP A2DP config, linger, and now a **user `daemon-reload` + restart** so unit changes apply to a
running instance. `debian/control` deps corrected to match. See the gap ledger G1–G9 in
`research/clean-install-2604-runbook.md` — all resolved.

**No-assumptions audit — DONE (2026-06-13).** Doctor 14/14 green; no leaked hostnames/IPs/paths;
uninstall complete. The single found assumption — hardcoded `hci0` — was **removed**: the adapter
index is now resolved from `SOUNDSYNC_HCI`, else the first `/sys/class/bluetooth/hci*`, else `hci0`,
in `src/main.rs` (`resolve_adapter()`), `packaging/install.sh` (`detect_hci()` in doctor/reset),
and `soundsync-cod-watch.sh`. (The daemon already used `control.hci_name()`.) Verified live: CoD
still pinned `0x240414`, doctor still green, no regression.

---

## 7. How to resume (next session)
1. Confirm health: `sudo packaging/install.sh --doctor` (expect all green).
2. Ask the human to connect a source and play a track; confirm a `bluez_input` source appears.
3. Run **IN-0** (§1) → record the verdict in `research/audio-routing-nuances.md` and the plan.
4. Proceed by the build order (§5). Keep the gate green after every slice; redeploy with the
   installer and verify on the host (esp. WebRTC continuity — the canary for daemon stability).

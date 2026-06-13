# SoundSync (`sc-next`) — `needsfixing.md`

> A fine-tooth-comb robustness / stability / correctness audit of the whole repository.
> **Goal:** identify what is fragile, hard-coded, assumed-but-untested, crash-prone, and what
> would fail to run (or fail to *stay* running for months, with and without audio) on a fresh
> or partially-provisioned machine. **This document contains NO code changes** — it is a work
> ledger. Every claim cites `file:line` verified against the tree at commit `dedbc68`.
>
> Produced by reading 100% of `src/**` (16 018 LOC Rust), `webui/src/**` (2 944 LOC TS), all of
> `packaging/**` + `debian/**`, and the four design docs, then cross-checking against the
> "crown-jewel" ledger `docs/audio-routing-nuances.md` (AUD-001…AUD-074, UB-1…UB-7). Note the
> ledger cites the **legacy** tree (`/home/mark/sc/sc`); a major part of the audit was verifying
> whether each hard-won nuance actually survived into this rebuild.

---

## 0. How this was checked + empirical build results

| Check | Result |
|---|---|
| `cargo build` (this box: toolchain present, no audio/BT runtime) | **FAILS** at `libdbus-sys` build script — `pkg-config` can't find `dbus-1` (no `libdbus-1-dev`). Hard `panic`/`exit 101`. Build deps **required**: `libdbus-1-dev`, `libopus-dev`, `cmake`, `pkg-config` (+ `cargo`/`rustc`/`nodejs`/`npm`). |
| `webui`: `npm ci` → `tsc --noEmit` → `vitest` → `vite build` | **PASS** — tsc clean, **28 tests pass**, build OK. BUT `npm ci` reports **5 high-severity advisories** (`esbuild`/`vite`/`vitest`, dev-only chain). |
| Rust tests | **461** `#[test]`/`#[tokio::test]` fns, all **inline** — there is **no `tests/` dir**. Could not run here (libdbus). Docs claim 457 lib + 4 bin green on the real host. |
| Runtime tooling present on this box | `pactl pw-cli wpctl bluetoothctl hciconfig ffmpeg avahi-browse` are **all MISSING**; only `dbus-monitor` present. (The app shells out to all of these at runtime.) |

**Production files with ZERO unit tests** (the real integration glue): `web/server.rs`,
`web/webrtc/webrtc_rs.rs`, `web/admin.rs`, `bluetooth/bluer_devices.rs`, `bluetooth/bluer_control.rs`,
`bluetooth/bluer_agent.rs` — i.e. the live ICE/DTLS, HTTP/WS, and `bluer`/D-Bus seams are exercised
only through fakes. This is the exact shape of the prior crash-loop regression (`docs/input.md §2`:
"two unit tests… false confidence").

---

## 1. Executive summary — the dominant themes

The pure, hot-path logic is genuinely strong: the frame/PCM contract, the broadcast bus, the
parsers, the state reducer, the AVRCP D-Bus liveness recovery, the EQ filter-chain supervision, and
the WebRTC RTP math are all careful and well-tested. **The danger is not in the algorithms — it is
in the lifecycle, the long-running connections, and the divergence from the ledger.** Seven themes
recur across every subsystem:

1. **Most long-running tasks are unsupervised, and `systemd` can't see their death.** Only 3 of
   ~10 tasks go through `supervise()`. `run_bluetooth`, `serve` (web), `run_spectrum`,
   `webrtc_command_loop`, and each per-session Opus pump are bare `tokio::spawn`. systemd
   `Restart=always` only fires on **process** exit, so a dead subsystem task = a dead feature
   until a human restarts the unit. Worse, `supervise()` restarts on *return*, **not on panic**.
2. **No timeout on any shelled-out command.** `SystemCommandRunner::run` is a blocking
   `Command::output()` with no timeout, called *synchronously inside* the reconcile `select!`. A
   wedged `pactl`/`wpctl` (a known post-suspend/post-restart failure mode) freezes reconciliation
   forever; shutdown can't preempt it; only the `exit(0)` backstop escapes — skipping all cleanup.
3. **Long-lived connections leak over months.** WebRTC (no `on_peer_connection_state_change`, no WS
   keepalive), Chromecast (no heartbeat-failure detection, no reconnect, no `JoinHandle`), the BT
   device map + per-device watchers, and per-client `parec|ffmpeg` streams all accumulate or wedge
   on abrupt disconnects / device churn.
4. **Silent-failure / "green UI, dead audio."** Several routing steps are never verified: the
   EQ→capture `pw-link` (UB-6 still open), the null-sink *reuse* path trusts module presence not
   node existence, the AirPlay loopback link, and the doctor's WP-config check is existence-only.
5. **Many ledger nuances were dropped in the rebuild** — some deliberately, none reconciled in the
   docs: AUD-007/008/009 (discovery/stale-session/name-resolution), AUD-027 (readiness poll),
   AUD-029 (tool fallbacks), AUD-058/059/060/061/062/065/066 (AirPlay/Chromecast hardening),
   AUD-043 (AAC profile), AUD-015/054/055/056 (RT scheduling + codec override in the installer).
6. **The `.deb` is a trap.** It silently omits the CoD watcher (re-opens the "won't appear as a
   speaker" G8 bug), linger (dead after reboot), and the user-WP restart. Only `install.sh` is safe.
7. **Hard-coded externals & no auth.** A public-internet STUN SPOF, an undocumented iTunes
   artwork call that phones home per track, and an unauthenticated `0.0.0.0:8080` exposing
   BT/EQ/output/admin control + uncapped stream spawns to any LAN host.

---

## 2. Master list — CRITICAL & HIGH (ranked, with location)

| # | Sev | Finding | Location | Category |
|---|-----|---------|----------|----------|
| NF-1 | CRIT | Shelled-out commands have **no timeout**; a wedged `pactl`/`wpctl` blocks reconcile forever | `sys/command.rs:28-37` | crash/hang |
| NF-2 | CRIT | **8 of ~10 long-running tasks unsupervised**; a panic kills the subsystem permanently, process stays up so systemd never restarts | `main.rs:113,123,164,186,198,248,272` vs `daemon.rs:149,164` | leak/long-term |
| NF-3 | CRIT | `run_bluetooth` spawned unsupervised; a BT-internal exit ends BT for the process lifetime | `main.rs:164-177` | crash/leak |
| NF-4 | CRIT | No recovery from a stale `bluer::Session`/D-Bus after suspend / adapter-reset / `bluetoothd` restart (AUD-011 recovery exists only for AVRCP) | `bluetooth/service.rs:56-57`, `bluer_devices.rs:147-153` | leak/long-term |
| NF-5 | CRIT | Audio-service **readiness poll (AUD-027) missing** from daemon startup → cold-boot routing failure → `exit(1)` restart loop | `daemon.rs:99-101` | regression |
| NF-6 | CRIT | Null-sink **reuse path trusts module presence, never verifies the sink node** → silent dead capture after a WP restart | `audio/pw/null_sink.rs:39-44` | crash(silent) |
| NF-7 | CRIT | WebRTC: **no `on_peer_connection_state_change`** + **no WS keepalive** → half-open clients leak PC + Opus pump forever | `web/webrtc/webrtc_rs.rs:157`, `web/server.rs:134` | leak/long-term |
| NF-8 | CRIT | Chromecast: dropped device **never recovers**, session task leaks; no heartbeat-failure detection, no reconnect, no `JoinHandle` | `cast/client.rs:163-193,270-365`, `output/controller.rs:327-338` | leak/long-term |
| NF-9 | CRIT | `.deb` does **not** install/enable the **CoD watcher** → re-introduces G8 "won't appear as a speaker" | `debian/soundsync.install`, `debian/postinst` | regression |
| NF-10 | CRIT | `.deb` never enables **linger** → user service dead after every reboot | `debian/postinst` | regression |
| NF-11 | CRIT | webui: **AUD-051 violated** — `ontrack` uses `evt.streams[0]` not `new MediaStream([evt.track])` → silent playback on every browser | `webui/src/lib/webrtc.ts:104` | regression |
| NF-12 | CRIT | webui: **AUD-067/068/069 violated** — `<audio>` not primed synchronously in the click gesture → Safari/iOS "Listen" is silently blocked | `webui/src/lib/webrtc.ts:38-48,96-105` | regression |
| NF-13 | CRIT | webui: **type drift** — backend `device_name` snapshot field + `device_name` WS message not in the TS contract; rename never shows; `Devices.tsx:300` hardcodes `'SoundSync'` in both ternary branches | `webui/src/lib/types.ts:66-90`, `store.tsx:60-88`, `Devices.tsx:300` | untested/missing |
| NF-14 | HIGH | EQ→capture **`pw-link` link never verified (UB-6)** → "EQ on" with dead audio | `audio/eq_controller.rs:128-167` | regression |
| NF-15 | HIGH | Web server `serve` unsupervised; a bind failure (port-in-use after fast restart) kills the UI for the process lifetime while audio keeps running | `main.rs:272-284` | leak/long-term |
| NF-16 | HIGH | `exit(0)` backstop **skips Drop cleanup** → BlueZ agent + discovery left registered across a restart, children unreaped | `main.rs:345`, `bluetooth/service.rs:40-42,81-89` | leak/long-term |
| NF-17 | HIGH | Doctor WP-config check is **existence-only, not content/format-aware** → false-PASS on a stale/empty/wrong config (W2.2 unbuilt) | `doctor.rs:120-143` | install/missing |
| NF-18 | HIGH | WP config-format **fallback to Lua on an unreadable `wireplumber --version`** → wrong format written on a 0.5 box, deletes the correct file → silent no-audio | `capabilities/probe.rs:13-21` | install/fragility |
| NF-19 | HIGH | Per-session Opus **pump unsupervised**; the comment claims the supervisor restarts it — it does not | `web/webrtc/pump.rs:43-53`, `webrtc_rs.rs:191` | crash/leak |
| NF-20 | HIGH | WebRTC **PT 111 / direction assumed**, never read from the negotiated answer → silent if a client picks another PT | `web/webrtc/webrtc_rs.rs:141-182` | untested |
| NF-21 | HIGH | `stop_all()` graceful drain **spawned detached, never awaited** → races process exit, usually loses (browsers get a hard drop) | `main.rs:123`, `web/webrtc/command.rs:56` | leak/correctness |
| NF-22 | HIGH | **AUD-007 absent** — discovery never auto-stopped on AudioActive (radio TDM stutter guard gone) | `bluetooth/manager.rs:118-152`, `service.rs:87` | missing/regression |
| NF-23 | HIGH | **AUD-009 absent** — no two-tier name resolution / no on-demand name fetch; devices can show as bare MACs forever | `bluetooth/bluer_devices.rs:128,181` | missing/regression |
| NF-24 | HIGH | **AUD-005 absent** — installer never adds user to `bluetooth` group nor verifies the running user-manager creds | `packaging/install.sh` | install/regression |
| NF-25 | HIGH | AirPlay routing swapped `pw-link`+suspend-sink-wake → `module-loopback`; **AUD-059/060 dropped**, link never verified, untested on real SUSPENDED RAOP | `output/controller.rs:250-269`, `loopback.rs:29` | regression/untested |
| NF-26 | HIGH | Chromecast: **no heartbeat PONG-timeout** → half-open TCP casts silence indefinitely | `cast/client.rs:270-365` | crash(silent)/leak |
| NF-27 | HIGH | **AUD-062 dropped** — no RAOP-module / `avahi-daemon` availability check → silent "no devices", indistinguishable from empty network; **AUD-066** mdns fallback gone | `output/controller.rs:89-174`, `discovery.rs:287-317` | install/regression |
| NF-28 | HIGH | `set_default_sink` has **no retry**; cold-boot failure is fatal → `exit(1)` loop | `audio/pw/routing.rs:16-48`, `daemon.rs:79,125` | fragility/regression |
| NF-29 | HIGH | **AUD-029 fallback chains mostly dropped** — no `pw-loopback` null-sink fallback, no `pw-cli/pw-dump` for BT-source/node-exists → pactl-or-nothing | `audio/pw/null_sink.rs`, `bluetooth/audio_node.rs` | missing/install |
| NF-30 | HIGH | Reconcile loop / supervised units are **unrecoverable on panic** (panic ≠ `Err`, so `supervise` stops restarting) | `daemon.rs:231-240`, `sys/supervisor.rs:81-85` | crash/leak |
| NF-31 | HIGH | `.deb` never restarts user WP → A2DP config written but not loaded (G4) | `debian/postinst` | install/regression |
| NF-32 | HIGH | `.deb` build untested; `dh_install` glob + missing CoD artifacts likely fail the build | `debian/soundsync.install`, `debian/rules` | untested/install |
| NF-33 | HIGH | `debian/rules` build relies on root's `$HOME/.local/node` — non-reproducible | `debian/rules:14-19` | fragility |
| NF-34 | HIGH | webui: **AUD-070 no reconnect jitter** → thundering-herd reconnect storm across tabs on daemon restart | `webui/src/lib/ws.ts:6-8,71-77` | regression/leak |
| NF-35 | HIGH | webui: **AUD-050 null-mid candidates not dropped** + snake_case not normalized → spurious Safari errors | `webui/src/lib/webrtc.ts:55-67` | regression |
| NF-36 | HIGH | webui: `ws.test.ts` does **not test `ws.ts`**; `webrtc.ts`/`store.tsx`/`api.ts`/all tabs untested → the AUD-051/067/070 regressions shipped past a green gate | `webui/src/lib/ws.test.ts` | untested |
| NF-37 | HIGH | No auth on `/api/*`, binds `0.0.0.0:8080`; **uncapped `/api/stream` spawns** = fork-bomb / FD exhaustion from any LAN host | `config.rs:11`, `web/server.rs:59-101`, `web/stream.rs` | security/stability |

---

## 3. Detailed findings by subsystem

> Format per finding: **What** · **Failure mode** · **Fix direction**. Severity & `file:line` as
> verified. "ROBUST" call-outs are included so good behaviour is not accidentally regressed.

### 3.1 Bluetooth — connection / pairing / adapter / discovery

**Framing:** sc-next deliberately replaced the legacy **active-inquiry** discovery with a **passive
watch** (`bluer_devices.rs:87` — "no active inquiry scan… the phone initiates"). There is no
`discover_devices()` call anywhere. Defensible for a pure sink, but it silently drops AUD-007/008/009
and their UI surface, and the docs never reconcile this.

- **[CRITICAL] NF-3 `run_bluetooth` unsupervised** — `main.rs:164-177`. It's a bare
  `tokio::spawn`; its `tokio::join!` (`service.rs:94-107`) ends the moment any of the four joined
  loops returns (e.g. `run_bt_manager` returns on `broadcast … Closed` at `manager.rs:139`; bridge/
  command loops return when a sender drops). The process stays up on its other tasks, so systemd
  never restarts → pairing/device-state/discovery dead until a human intervenes. **Fix:** wrap in
  `supervise(...)` (rebuild Session+adapter+agent+discovery on exit).
- **[CRITICAL] NF-4 No stale-session recovery** — `service.rs:56-57`, `bluer_devices.rs:147-153`.
  One `bluer::Session`+`default_adapter()` for the whole process lifetime; `start_discovery`'s loop
  just `eprintln!`s and `return`s if `adapter.events()` fails, permanently ending the watcher.
  `DbusHealth` (the AUD-011 reconnect primitive) is **only** used by AVRCP, never here. After the
  first suspend / `bluetoothd` restart on a Pi: cached proxies dead, `/api/pair` "succeeds" (it only
  `try_send`s — `web/pairing.rs:44`) but the adapter never becomes discoverable. **Fix:** supervise
  (rebuilds the Session) or add a `DbusHealth`-style reconnect.
- **[HIGH] NF-22 AUD-007 absent** — `manager.rs:118-152`, `service.rs:87`. Discovery is started once
  and never stopped on `AudioActive`; `stop_discovery` (`bluer_devices.rs:211`) is called nowhere
  outside tests, and the manager holds no `DeviceControl` to call it. The pairing window still drives
  `set_discoverable(true)` (`pairing.rs:49`) with `DiscoverableTimeout=0` — re-introduces the radio
  stutter if a device is left mid-pairing. **Fix:** stop discovery / close the pairing window on the
  `Connected→AudioActive` edge, or document that passive-watch supersedes AUD-007 and prove no-stutter.
- **[HIGH] NF-23 AUD-009 absent** — `bluer_devices.rs:128,181`. Name read exactly once per surfacing
  event (`dev.name().await`); no 250ms fast-poll, no `hcitool` Identify, no empty/MAC-shape rejection.
  The manager has *sticky* name logic (`manager.rs:54-59`) but nothing ever feeds it a later name, so
  a device that surfaces before BlueZ resolves its alias shows a bare MAC forever. (The web "Identify"
  route plays an 880 Hz **tone** — `daemon.rs:198-221` — a different feature sharing the label.)
  **Fix:** bounded `alias()` re-poll after `DeviceAdded`; reject empty/MAC-shaped names.
- **[HIGH] NF-24 AUD-005 absent** — `packaging/install.sh`. No `usermod`/`gpasswd` to add the user to
  `bluetooth`, no running-user-manager credential verification, no `SupplementaryGroups=bluetooth` on
  the unit. Classic "works after a reboot": WP's BlueZ5 monitor can't reach BlueZ until re-login.
- **[MEDIUM] User daemon's CoD/HCI-name calls EPERM every boot** — `service.rs:71-74`. The
  unprivileged user service shells `hciconfig … class/name` which needs `CAP_NET_ADMIN`; it always
  logs a failure. Dead weight (the root watcher owns this) and confusing. **Fix:** drop them from the
  user daemon; let the root services own AUD-002/003.
- **[MEDIUM] HCI name (AUD-003) set once by the boot oneshot, never re-pinned** — `adapter.rs:17`,
  `soundsync-adapter.service`. The reactive watcher re-pins only `class`, not `name`
  (`soundsync-cod-watch.sh:38`); if `bluetoothd` resets EIR on profile churn the HCI name reverts.
  (Alias *is* set live via bluer, so this is belt-and-braces only.)
- **[MEDIUM] `hciconfig`/`hcitool` hard dependency** — `adapter.rs:11-23`, `soundsync-cod-watch.sh:32,38`.
  Deprecated and not installed by default on modern distros. If absent, the *entire* AUD-002b CoD
  strategy silently no-ops (watcher swallows it with `2>/dev/null`). **Fix:** hard-depend on the
  provider, log loudly when missing, or migrate to `btmgmt`/D-Bus mgmt API.
- **[MEDIUM] Unbounded device map** — `manager.rs:30,40-69`. `BTreeMap<MAC, DeviceSnapshot>` only
  pruned on `DeviceRemoved`, which BlueZ rarely emits; every transient phone in a café accumulates a
  permanent snapshot serialized to every WS client on every change (`state.rs:229-234`). **Fix:** cap/
  prune long-Disconnected non-paired entries; send deltas.
- **[MEDIUM] Per-device watcher tasks accumulate** — `bluer_devices.rs:103-110`. One persistent
  `dev.events()` task per device ever seen (the dedup set is per-discovery-run; discovery never
  restarts), each holding a D-Bus match rule. **Fix:** reap on disconnect, or bound; supervising BT
  bounds it naturally.
- **[LOW] Optimistic `bluez_input.*` parsing (AUD-010 gate)** — `audio_node.rs:15-30`. No MAC-shape
  validation, no case normalization; `reconcile_audio` does an exact string compare (`manager.rs:77`).
  A lowercase/odd-suffix node name → device never reaches `AudioActive` despite audio flowing.
- **[LOW] Pairing-window `set_pairable`/`set_discoverable` failures swallowed** — `pairing.rs:49-51`.
  `open=true` published even if the pairable write failed → `/api/pair` looks open, phone sees nothing.
- **[LOW] `HOME`/`XDG_RUNTIME_DIR` default to `/root` flavors** — `name_config.rs:31,45`, `main.rs:197`
  — odd for a per-user service; device-name persistence silently no-ops if the user env lacks `HOME`.

**ROBUST:** the device state machine + reducer (`device.rs`, `events.rs`, panic-free total
`apply_event`), AUD-012 path encoding (`addr.rs`, tested), the CoD watcher's reactive design, the
pairing window (re-openable, auto-expiring, well tested), and `resolve_adapter()` (no hardcoded `hci0`).

### 3.2 Bluetooth — AVRCP metadata / now-playing / artwork

**Reframe:** **UB-4 is obsolete.** sc-next does **not** extract artwork from the BlueZ `Track` dict;
it was redesigned to query the **iTunes Search API** by text and hand the remote
`https://*.mzstatic.com` URL straight to the browser. So "image decode panic/OOM" and "decoded-art
memory growth" are **not applicable** — there is no server-side image pipeline.

- **[MEDIUM] Album art phones home to Apple per track** — `artwork.rs:125`, `NowPlaying.tsx:168`.
  Every track change → outbound HTTPS to `itunes.apple.com/search` with artist/album/title; the
  browser then loads the CDN URL directly, so **each web client also contacts Apple**. Undocumented;
  opt-out `SOUNDSYNC_ARTWORK=off` exists but **defaults on**. Also lossy: `entity=album&limit=1` text
  match → wrong/missing covers; offline/firewalled (the Pi target) gets nothing. **Fix:** document it,
  consider default-off for appliance privacy, or proxy/cache server-side.
- **[MEDIUM] Pre-player search loop has no `DbusHealth`** — `media.rs:446-459,143-154`. If the system
  bus goes stale *while waiting for a player to bind* (suspend before AVRCP player appears),
  `find_active_player` swallows the error (returns `None`, `:150-153`) and retries the dead connection
  every 3 s forever; the `'reconnect` paths all live in the post-bind loop. **Fix:** count
  `find_active_player` failures (distinguish "no player" from "D-Bus error") and `continue 'reconnect`.
- **[MEDIUM] iTunes JSON body read bounded only by ureq's implicit ~10 MB** — `artwork.rs:139`. A
  MITM/compromised DNS could amplify per-track allocs. **Fix:** explicit small `take(N)` cap.
- **[LOW] Hardcoded magic numbers** — `media.rs:32-37` (5s/2s/3s timeouts, D-Bus interface strings),
  `avrcp.rs:104` (`DBUS_FAILURE_THRESHOLD=5`, matches ledger), `artwork.rs` (iTunes URL, `600x600bb`
  upscale token list, 4s timeout). The `upscale_artwork_url` token swap silently no-ops if Apple
  changes its size-token format.
- **[LOW] `track_from_propmap` Duration parsing untested** — `media.rs:205-212`. `u64` fallback +
  `unwrap_or(u32::MAX)` is panic-safe, but a BlueZ build sending Duration as `i32`/`u16` silently
  drops a valid duration; only `media_info_from`/`select_player` are covered, not the dict parser.

**ROBUST:** **AUD-011 D-Bus liveness present & correct** — `DbusHealth` fires+self-resets at exactly
5 (`avrcp.rs:104-139`), every post-bind failure funnels to `continue 'reconnect`, and the old
connection is torn down via `AbortOnDrop` (`media.rs:393-399`, tested). **`select_player` is
deterministic + sticky** (`media.rs:114,131-135`) — the H4/AUD-041 metadata-flap is *not* present
(it only materializes if the unbuilt Wave-3 `active_input` tier is added). Artwork cache is O(1),
one fetch per genuine track change, all guards drop mutexes before `await` (no deadlock).

### 3.3 Audio — PipeWire graph & capture

- **[CRITICAL] NF-5 AUD-027 readiness poll missing** — `daemon.rs:99-101`. `run_daemon` goes straight
  to `detect_capture_tool`/`probe_quantum`/`ensure_null_sink` with no "poll `pactl info` up to 30s"
  gate. On a cold Pi boot pipewire-pulse may not accept commands yet; combined with NF-28 this is the
  classic "works after I restart it manually." **Fix:** bounded readiness poll (raced vs shutdown) at
  the top of `run_daemon`.
- **[CRITICAL] NF-6 Null-sink reuse trusts the module, not the node** — `null_sink.rs:39-44`.
  `find_existing` scans `pactl list modules`; on a hit it returns `{reused:true}` **without** the
  `pactl list short sinks` node-existence check that only the *create* path does (`:71-83`). After a
  WP/PipeWire restart that leaves the pulse module but drops the sink node, reconcile "heals" nothing
  and capture taps a `.monitor` that no longer exists → **permanent silence until process restart.**
  **Fix:** verify the node on the reuse path; unload the stale module and re-create if absent.
- **[HIGH] NF-28 `set_default_sink` no retry** — `routing.rs:16-48`, `daemon.rs:79,125`. One attempt
  + one `wpctl` fallback. A startup failure propagates → `main.rs:332-337` `exit(1)` → systemd loop;
  a mid-run failure isn't re-attempted until the next graph event (which may never come). **Fix:**
  fold into the AUD-027 gate or give it its own bounded retry / delayed reconcile retry.
- **[HIGH] NF-29 AUD-029 fallback chains mostly dropped** — `null_sink.rs`, `bluetooth/audio_node.rs`.
  Only set-default (`pactl→wpctl`) and capture-tool (`parec→pw-cat`) survive; the null-sink
  `pactl→pw-loopback` and the BT-source/node-exists `pactl→pw-cli→pw-dump` chains are gone. On a box
  without `pulseaudio-utils` the whole graph fails to build. (UB-7 is therefore moot only because the
  fallback was removed.) **Fix:** decide explicitly — document pactl as hard-required *or* restore the
  `pw-loopback` fallback with a correct capture target.
- **[HIGH] AUD-057 zombie-reap only via `kill_on_drop`** — `process.rs:68,80`, `supervisor.rs:81-85`.
  The supervisor cancels a unit by **dropping** the future (drops the `Child`); it never explicitly
  `kill().await`+`wait().await` the capture/event children as the ledger prescribes. Tokio's orphan
  reaper usually collects them, but on the `exit(0)` backstop path Drop is skipped entirely. **Fix:**
  explicit kill+wait on the cancel path, or document the chosen mechanism.
- **[MEDIUM] Startup sweep skipped when `module_id` is `None`** — `daemon.rs:122-124`,
  `null_sink.rs:78-82`. The (correct, startup-only) duplicate sweep only runs if `ensure_null_sink`
  returned a parseable module id; otherwise duplicates from a prior crashed run survive → AUD-026
  stutter. Also: the sweep handles only `module-null-sink`, not orphaned `soundsync`-targeting
  `module-loopback`. **Fix:** resolve the canonical id from `pactl list short modules` (or, at
  startup, unload all `soundsync-capture` null-sinks then create one fresh).
- **[MEDIUM] Capture fast-fail respawn loop** — `supervisor.rs:16-24`, `daemon.rs:149`. If the
  monitor is missing, capture respawns ~every 5 s forever (no ceiling, no surfaced "capture down").
  Steady-state with no source is correctly *quiet* (parec streams silence, doesn't exit) — the loop
  only triggers when the monitor node itself is gone (ties to NF-6).
- **[MEDIUM] Parser format-drift** — `parse.rs`, `quantum.rs:31-41`, `events.rs:19-29`. `wpctl
  status`/`pw-metadata`/`pactl` human-text parsing; all fail *soft* (None/default), so drift degrades
  gracefully — but a `wpctl` change silently breaks node-id resolution. Prefer machine-readable
  `pw-dump`/`pactl -f json` where possible (the EQ path already does).
- **[MEDIUM] `Identify` tone bypasses the spawner seam** — `daemon.rs:204-221`. Hardcoded `sh -c
  "ffmpeg | pw-cat --target=soundsync-capture"`, `2>/dev/null`-swallowed, untestable.

**ROBUST & IMPROVED:** the canonical frame contract (`frame.rs:3-12`, 48k/2ch/f32/1920/7680,
broadcast cap **256**, all asserted), **UB-1 fixed** (latency derived `max(2×quantum,50ms)` —
`quantum.rs`), AUD-034 (`.monitor` explicit, never `@DEFAULT_MONITOR@`), AUD-039 anti-cascade (slow
consumer can't stall the pump, tested), partial `parec` read recovery (clean restart, no desync),
and **capture IS restarted by `supervise()` if it dies**.

### 3.4 EQ / DSP / spectrum / Opus encode

> Note: there is **no hand-rolled biquad math** to audit — `dsp/eq.rs:99-105` emits PipeWire builtin
> `bq_*` labels; PipeWire computes coefficients. The denormal/NaN-from-biquad risk lives in
> `libpipewire-module-filter-chain`, not here.

- **[HIGH] NF-14 eq→capture link never verified (UB-6)** — `eq_controller.rs:128-167`. On enable it
  waits for the EQ *node* (`wait_for_eq_node`) and sets the route, but nothing asserts the
  `effect_output.soundsync-eq → soundsync-capture` **link** formed (no `pw-link -l` anywhere). If WP
  doesn't honor `node.target` (suspended sink, respawn race, schema drift), the UI shows "EQ on",
  default sink points at the EQ input, but output goes nowhere → **silent dead audio, green UI.**
  **Fix:** poll `pw-link -l` after the node appears; retry/fall back to the null-sink route if absent.
- **[HIGH] Spectrum task unsupervised** — `main.rs:113-117`. Bare `tokio::spawn`; a panic kills the
  visualizer permanently (it can't wedge capture — separate subscriber, AUD-072 holds — so HIGH not
  CRIT). **Fix:** `supervise()` it or make `process_frame` provably panic-free.
- **[MEDIUM] `map_to_bands` slice-index panic on a degenerate buffer** — `spectrum.rs:143-165`. The
  `clamp(1, half-1)` assumes `half ≥ 2`; `half==0` underflows usize, `half==1` panics (`min>max`).
  Not reachable at FFT_SIZE=2048, but a future FFT-size change → permanent visualizer death.
- **[MEDIUM] NaN can latch into the EMA forever** — `spectrum.rs:113-131,156-163`. No non-finite
  guard before `smoothed = α·band + (1-α)·smoothed`; one `NaN` band poisons that EMA slot for the
  analyzer's life (no reset), and serializes as JSON `null` to the FE. **Fix:** replace non-finite
  band values with 0 before the EMA / clamp the accumulator.
- **[MEDIUM] EQ-route re-assert is implicit via `pactl subscribe`** — `eq_controller.rs:142-155`,
  `daemon.rs:230-240`. The toggle only updates the `DesiredRoute` watch; the actual `set-default-sink`
  happens only when a `pactl subscribe` sink event fires reconcile. A route change does **not** wake
  the reconcile loop directly. A missed/slow/coalesced event → BT audio bypasses EQ into the null
  sink. **Fix:** call `set_default_sink` directly after `wait_for_eq_node`, or add `route_rx.changed()`
  as a reconcile select arm.
- **[MEDIUM] `generate_filter_chain_config` `assert!`s on empty bands** — `dsp/eq.rs:112-116`. A panic
  (not `Result`) inside the supervised filter-chain factory future — a panic isn't `Err`, so it kills
  the EQ supervisor task. Unreachable today (callers always pass 10 bands) but a latent foot-gun.
- **[LOW] EQ playback props omit `audio.rate`** — `dsp/eq.rs:153-159`. AUD-025 wants rate on **both**
  capture & playback; only capture has it. Usually benign; could trigger a resample on odd hosts. The
  test only checks one occurrence so it passes despite the asymmetry.
- **[LOW] Embedded filter-chain base config not verified** — `filter_chain.rs:82-97`. On RPi (no
  standalone binary) the embedded `pipewire -c filter-chain.conf` needs a base `filter-chain.conf` in
  PipeWire's search path; if absent the EQ subprocess exits immediately → supervisor restart-loop with
  no clear diagnostic.
- **[LOW] Opus encode error indistinguishable from silence** — `opus_encoder.rs:25-30`. Any
  error/zero-length → `None` → pump skips with no log; a systematic encode failure = silent WebRTC.

**ROBUST & IMPROVED:** atomic config write (`fs.rs:21-27`, tested), **the filter-chain subprocess IS
supervised + respawns** (`eq_controller.rs:245-262`) with node-id re-probe across respawns, no
zombie/fd leak across toggles (`kill_on_drop` + abort+await), AUD-024 *improved* (live `pw-cli
set-param` gain updates, no 200ms restart; `wait_for_eq_node` replaces the blind 500ms sleep),
AUD-025 inputs/outputs present & tested, AUD-045 Opus exact + tested, AUD-046/UB-3 spectrum correct
(48000 not 44100), spectrum can't wedge capture and doesn't spin on silence.

### 3.5 Web — server / WS / HTTP stream / REST

- **[HIGH] NF-15 `serve` unsupervised** — `main.rs:272-284`. A returned `Err` (most realistically
  `AddrInUse` after a fast restart racing `TIME_WAIT`) leaves the entire UI/WS/stream surface down
  for the process lifetime while audio/BT/EQ keep running — and nothing retries the bind. **Fix:**
  `supervise("web", …)` (cfg/ctx are `Clone`) or retry `TcpListener::bind` with backoff.
- **[MEDIUM] Offer handling awaits the WebRTC reply inline** — `server.rs:162-178`. While blocked on
  `rrx.await` (real ICE/DTLS setup on an attacker-controlled SDP), that WS connection doesn't observe
  shutdown or drain events; a wedged webrtc-rs hangs that one socket until the 8s backstop. Blast
  radius is one connection. **Fix:** race `rrx.await` against shutdown + a timeout.
- **[MEDIUM] Half-open WS leaks its WebRTC session + Opus pump** — `server.rs:115-202`. No WS ping/
  idle-timeout; a silently-severed TCP (Wi-Fi drop, `kill -9`) never reaches the `Stop` teardown until
  OS keepalive (~2 h). N stale pumps accrue CPU+FDs. (Pairs with NF-7.) **Fix:** periodic WS ping +
  pong deadline.
- **[MEDIUM] NF-37 No auth + uncapped streams** — `config.rs:11`, `server.rs:59-101`, `web/stream.rs`.
  `0.0.0.0:8080`, every endpoint unauthenticated incl. `/api/admin/restart`, `/api/bluetooth/:addr/
  forget`, and `GET /api/stream/audio.{aac,mp3}` which forks a `parec|ffmpeg` pair **with no
  concurrency cap** → a loop of stream GETs is a fork-bomb / FD-and-CPU exhaustion vector. **Fix:**
  cap concurrent streams (semaphore→503), consider LAN-only default bind. (CORS-absence is correct.)
- **[MEDIUM] Missing `ffmpeg`/`parec` → `200 OK` + empty body** — `web/stream.rs:65-97`. `sh -c`
  always spawns, so the `503` arm only catches "can't spawn sh"; absent tools → pipeline exits, `read`
  returns `Ok(0)` → client gets a 200 with `audio/aac` and zero bytes, stderr `/dev/null`'d. Confusing
  on a partial/old box. **Fix:** preflight `which`, or `try_wait()` before 200; stop discarding stderr.
- **[MEDIUM] AUD-043 regression — AAC encoder drops `-profile:a aac_low -cutoff 20000`** —
  `web/stream.rs:26`. Only `-c:a aac -b:a 256k -f adts`. Usually benign (ffmpeg defaults to LC) but
  the ledger pinned it for strict Chromecast/Safari decoders; the test doesn't assert it.
- **[LOW] `server.rs` has zero tests** — the disconnect-reaps-child behavior (the whole point of
  AUD-042) is asserted nowhere; only the command *string* is tested. **Fix:** integration test that
  drops the response body and asserts the child is killed.
- **[LOW] No explicit `DefaultBodyLimit`** — `server.rs:57-103`. Relies on axum's implicit 2 MB cap;
  a future framework default change silently removes it.

**ROBUST:** every REST handler uses `try_send`→503 (never blocks the HTTP thread, never panics on a
closed receiver); no shell-injection surface (`stream.rs` interpolates only the compile-time
`CAPTURE_MONITOR`; BT addr flows over a channel, never into a shell); graceful shutdown is correct
(both `with_graceful_shutdown` **and** `ws_connection` observing the shutdown watch); AUD-048 casing,
AUD-053 session keying+teardown (clean disconnects), AUD-071 dual-purpose WS all preserved + tested;
broadcast `Lagged`→continue / `Closed`→break (AUD-039/072).

### 3.6 WebRTC

- **[CRITICAL] NF-7 No PC connection-state handler** — `webrtc_rs.rs:157`. The only callback is
  `on_ice_candidate`; there is **no `on_peer_connection_state_change`** anywhere (grep: zero). Teardown
  happens only via a `Stop` from the WS loop seeing `Close`/`Err`/`None`. An abrupt close (sleep, tab
  kill, Wi-Fi drop, NAT rebind, ICE `Failed`) → the `SessionHandle`, the `RTCPeerConnection`, and its
  detached `spawn_blocking` Opus pump live **forever**, encoding into a track no one reads. Over months
  this grows without bound (tasks, PCs, UDP sockets, steady CPU). **The single highest-value WebRTC
  fix.** **Fix:** register `on_peer_connection_state_change`; on `Failed`/`Closed` send `Stop`.
- **[CRITICAL] No WS keepalive ping** — `server.rs:134` (shared with NF-7). The signalling socket has
  no ping/idle-timeout, so a half-open TCP isn't detected until OS keepalive. (Spectrum traffic
  *usually* fails a `send` and breaks the loop, but not if the event stream is ever quiet.)
- **[HIGH] NF-21 `stop_all()` detached, never awaited** — `main.rs:123`, `command.rs:56`. On shutdown
  the webrtc loop runs `stop_all()` (closes each PC via network I/O), but `main` returns the moment
  `run_daemon` finishes and never joins the loop → the runtime drops detached tasks mid-`pc.close()`.
  Browsers get a hard drop (no DTLS close-notify) — the very WebRTC-continuity canary `input.md`
  cares about. (Not a cross-restart leak; OS frees sockets.) **Fix:** retain the loop's `JoinHandle`
  and `.await` it (bounded) before exit.
- **[HIGH] NF-19 Per-session pump unsupervised** — `pump.rs:43-53`, `webrtc_rs.rs:191`. The comment
  claims "the supervisor restarts it (with a fresh encoder)" — but it's a bare `tokio::spawn`, no
  `supervise()`. A poisoned encoder mutex (`pump.rs:47` `enc.lock().unwrap()`) → `JoinError` → pump
  returns → audio silently dead for that session until reconnect, no restart, no log.
- **[HIGH] NF-20 PT 111 / direction assumed, not negotiated** — `webrtc_rs.rs:141-182`. The server
  `add_track`s Opus and answers `create_answer(None)` but never reads the negotiated Opus PT; it
  blindly stamps `OPUS_PAYLOAD_TYPE=111` on every packet (`:64`). A client that negotiates a different
  PT → browser drops every packet, "connected" PC with silent audio, nothing logged.
- **[MEDIUM] Real `WebrtcController`/`WebrtcRtpSink`/`handle_offer` untested** — `webrtc_rs.rs:1`
  (self-documents "NOT unit-tested"). Only the Fake is exercised — exactly the gate-slip shape of the
  prior crash-loop. **Fix:** an SDP round-trip test through `handle_offer`.
- **[MEDIUM] Hardcoded STUN SPOF, no TURN, no override** — `webrtc_rs.rs:34`. `stun.l.google.com:19302`
  only; an air-gapped/firewalled appliance gets host-candidate-only ICE with no fallback. **Fix:** env-
  configurable STUN/TURN; degrade gracefully to host-only with a warning.
- **[MEDIUM] `sessions` `tokio::Mutex` + serial command loop head-of-line-blocks** —
  `webrtc_rs.rs:230-238`, `command.rs:42-54`. `stop_all`/`stop`/`add_ice`/`handle_offer` avoid holding
  the lock across awaits (good), but the command loop awaits each `handle_offer` serially, so a slow
  PC creation blocks all other sessions' offers/ICE/stops. Maintenance hazard + multi-client latency.
- **[LOW] Pump holds a session on an idle bus with no frame timeout** — `pump.rs:33-67` (parks, no
  spin — good; but combined with NF-7 is how leaked sessions accumulate). RTP `u32`/`u16` wrap is
  *correct-by-design* and tested (`rtp.rs`). `new_ssrc()` UUID-truncation is safe (one track per PC).

**ROBUST:** AUD-038 no-pacer, AUD-040 lag math (tested), AUD-041 spawn_blocking encode, AUD-045/047
params, the RTP timeline (wrapping, tested) — all preserved. Clean disconnects + explicit `Stop` +
`stop_all` *do* tear down correctly; the gap is abrupt/half-open disconnects.

### 3.7 Cast (Chromecast) + Output (AirPlay / soundcard)

- **[CRITICAL] NF-8 Dropped Chromecast never recovers, leaks the session task** — `cast/client.rs:163-193`,
  `output/controller.rs:327-338`. `CastHandle` holds the `watch::Sender`; the task holds the
  `Receiver` and `return`s silently on any TLS read error/EOF/write failure. The controller keeps **no
  `JoinHandle`** and the 10s tick never checks liveness. The doc-comment "the watch channel will close
  when the task exits" is **backwards**. A Chromecast that reboots/roams shows "active" while playing
  silence, forever, until manual re-select. **Fix:** store the `JoinHandle`, check `is_finished()` on
  tick (clear `active`/republish), or reconnect inside `run_cast_session`.
- **[CRITICAL] NF-26 No heartbeat PONG-timeout** — `cast/client.rs:270-365`. PING every 5s, PONG to
  inbound PINGs, but **never tracks whether the device answers** — no failure counter (AUD-065's "3
  strikes" dropped), no read deadline. A half-open TCP wedges the session task in `select!` forever.
  **Fix:** track last-inbound time; declare lost after N×heartbeat / `timeout` the read arm.
- **[HIGH] NF-25 AirPlay uses `module-loopback`, not `pw-link`** — `controller.rs:250-269`,
  `loopback.rs:29`. AUD-058/059/060 (pw-link + `pactl suspend-sink` wake + runtime port discovery) are
  gone; SUSPENDED RAOP sinks expose zero ports, so loopback to a freshly-discovered sink may not
  produce audio, and the link is never verified (UB-6-style). Unverified mechanism swap, no integration
  test on real hardware. **Fix:** verify on hardware *or* reinstate the suspend-sink wake; assert the link.
- **[HIGH] NF-27 No RAOP/avahi availability check (AUD-062) + no mdns fallback (AUD-066)** —
  `controller.rs:89-174`, `discovery.rs:287-317`. Missing `avahi-daemon`/`avahi-utils` → silent empty
  Chromecast list with no warning; no `mdns-sd` crate in the tree, so non-avahi hosts can never
  discover casts. `doctor` checks `pw-link` (now unused) but **not** `avahi-browse` or the RAOP module.
- **[HIGH] `avahi-browse -rtp`, no `\NNN` escape decode (AUD-061 partial)** — `discovery.rs:90-139,292`.
  Names with spaces/specials survive only by luck; a `;` in a device name shifts the `splitn(10,';')`
  columns → port parse fails → **device silently disappears**. (The IPv6-before-dedup hazard *is*
  handled correctly via IPv4-first stable sort — tested.) **Fix:** decode `\NNN`.
- **[MEDIUM] `detect_local_ip` 8.8.8.8 fallback dropped (AUD-064)** — `cast/client.rs:43-71`,
  `main.rs:218-241`. Only `hostname -I`; if empty, `lan_ip=""` and casting is silently disabled (the
  improved RFC-1918 filtering is good, but the UDP-connect fallback that finds the egress IP in
  VPN/netns/Docker is gone).
- **[MEDIUM] Cast content-type/URL duplicated, not shared** — `cast/client.rs:333` (`"audio/aac"`) vs
  `web/stream.rs:17`. Consistent today; a future stream-format change won't propagate → silent mismatch.
- **[MEDIUM] Hardcoded cast constants** — heartbeat 5s, port `8009`, app `CC1AD845`, refresh 10s,
  `sender-0`/`receiver-0`, initial req_ids. Correct for current firmware, no config.
- **[LOW] All cast logging is `eprintln!`** — no cast health surfaced into `AppState`, so the UI can't
  reflect real cast state (ties into NF-8).

**ROBUST:** the protobuf framing (`proto.rs`, length-prefix/partial-frame/trailing-bytes all tested,
panic-free), the JSON builders/parsers (`messages.rs`, skip malformed), all discovery parsers (total,
garbage-input tested), the output **loopback** path (single-active teardown enforced, module ids
tracked+unloaded on switch/shutdown, failed load reverts cleanly — no thread/module pileup across
re-selections), AUD-063 HTTP-pull params, and the deliberate documented TLS-no-verify posture.

### 3.8 System / daemon / lifecycle / config / doctor / install / capabilities

- **[CRITICAL] NF-1 No command timeout** — `sys/command.rs:28-37`. `Command::output()` blocks with no
  timeout, called synchronously inside the reconcile/`ReassertRouting` `select!` arms (`daemon.rs:307,
  191`). A wedged `pactl`/`wpctl`/`pw-metadata` (real after pulse restart / D-Bus stall / suspend) is a
  blocking syscall the shutdown arm cannot preempt → reconciliation frozen forever during normal
  operation; SIGTERM can't return `run_daemon`; only the Drop-skipping `exit(0)` backstop escapes.
  **The single highest-value fix.** **Fix:** add a timeout to the runner (`wait_timeout` / tokio
  `Command` + `timeout`), or `spawn_blocking`+`timeout` the synchronous setup calls.
- **[CRITICAL] NF-2 Supervisor wired to only 2 of ~10 tasks** — `main.rs:113,123,164,186,198,248,272`
  vs `daemon.rs:149,164`. Only capture + the `pactl subscribe` event source are supervised. BT, media,
  EQ-controller, output, WebRTC, spectrum, and the web server are fire-and-forget. A panic in any →
  silently gone, process stays up, systemd never restarts. And `supervise`'s `factory()` returns
  `io::Result`, so a **panic** inside even a supervised unit aborts that task too (panic ≠ `Err`).
  **Fix:** wrap the long-lived `main.rs` tasks in `supervise(...)` (they already take a shutdown watch),
  or a top-level join that makes any unexpected task exit process-fatal so systemd restarts the unit.
- **[HIGH] NF-16 `exit(0)` skips Drop cleanup** — `main.rs:345`, `service.rs:40-42,81-89`. Whenever the
  backstop fires (the *expected* outcome of a wedged `pactl` per NF-1), the BlueZ `NoInputNoOutput`
  auto-accept agent + discovery stay registered across the restart; `kill_on_drop` children unreaped.
  **Fix:** best-effort bounded teardown before `exit(0)`, or make the clean path un-hangable (NF-1).
- **[HIGH] NF-17 Doctor WP-config check existence-only** — `doctor.rs:120-143`. Passes on any readable
  file regardless of content (fixture is literally `"# config"`); never asserts `bluez5.roles` /
  `seat-monitoring = disabled` (AUD-004). An empty/truncated/wrong-format file passes the doctor while
  WP loads nothing → pairs but no audio, green doctor. (W2.2 confirmed unbuilt.) **Fix:** ConfigFormat-
  aware content assertion using `generate(fmt)` as the oracle.
- **[HIGH] NF-18 WP format fallback to Lua on unreadable version** — `capabilities/probe.rs:13-21`. If
  `wireplumber --version` can't be read/parsed, it assumes **Lua** and `apply_config` writes the Lua
  file to a 0.5 box (silently ignored) **and deletes** the correct SPA-JSON (`apply.rs:43`). **Fix:**
  prefer SPA-JSON as the modern default on detection failure; don't delete the opposite file on a
  *fallback* (vs a parse).
- **[HIGH] NF-30 Reconcile/supervised units unrecoverable on panic** — `daemon.rs:231-240`,
  `supervisor.rs:81-85`. A panic in a supervised unit body aborts the supervised task (panic ≠ `Err`),
  so capture/events die without restart. **Fix:** `catch_unwind`/spawn-the-unit-and-treat-`JoinError`-
  as-failure; decide if a reconcile-loop panic should be process-fatal.
- **[HIGH] Shutdown-timeout ordering enforced only for the default** — `main.rs:381-388,419-423`,
  `soundsync.service:15`. `SOUNDSYNC_SHUTDOWN_TIMEOUT=30` silently inverts the invariant (systemd
  SIGKILLs at 15s before the 30s backstop). **Fix:** clamp below a known unit ceiling / derive both
  from one source / warn.
- **[MEDIUM] Doctor probes hardcoded `hci0`** — `doctor.rs:147`. Ignores `resolve_adapter()`; false
  WARN on `hci1`/`hci2` boxes — contradicts the `input.md §6` "hci0 removed" claim. **Fix:** share
  `resolve_adapter()`.
- **[MEDIUM] `REQUIRED_BINS` Fails on optional/alt tools** — `doctor.rs:54-65`. Hard-Fails on
  `hciconfig` (deprecated) and `pw-cat` (only the *fallback* capture tool). **Fix:** "at least one of
  {parec,pw-cat}"; `hciconfig` → Warn.
- **[MEDIUM] `config_store` dormant but data-lossy if wired** — `config_store.rs:36-46`. Confirmed no
  consumers (`input.md` agrees). `load_json` maps *corrupt JSON* and *absent* both to `default()` →
  the first `save_json` overwrites a corrupt file, losing user state silently. Flag before the first
  consumer. (`write_atomic` does protect torn writes.)
- **[MEDIUM] `WpVersion::parse` takes the first `X.Y` token anywhere** — `version.rs:19-29`. A stray
  `1.2` in a banner/warning misparses the version → wrong ConfigFormat (feeds NF-18). **Fix:** anchor on
  a marker.
- **[MEDIUM] `Identify` shells `sh -c "ffmpeg | pw-cat"` off-seam** — `daemon.rs:204-221`. Untestable
  hardcoded escape hatch in an otherwise disciplined seam architecture.
- **[LOW] state.rs "17 unwraps" are ALL `#[cfg(test)]`** — production state path is unwrap-free;
  `tokio::RwLock` doesn't poison; snapshot clone is O(devices+bands). **No runtime risk** (verified).

**ROBUST:** the supervisor mechanism itself (capped backoff, `saturating_mul`, biased shutdown-first,
4 tests), the crash-loop fix (`main.rs:317-347`, 3 guarding tests), atomic config writes + AUD-004/017/
018/020 in `apply_config`, the pactl/module parsers (token-boundary matching, PW-1.6 identical-name
disambiguation by `sink_name=` argument), `osrelease`/`version` robustness, the fully unit-testable
command/process fakes.

### 3.9 Packaging / install / systemd / debian

- **[CRITICAL] NF-9 `.deb` omits the CoD watcher** — `debian/soundsync.install` (only ships binary +
  webui + 2 units, not `soundsync-adapter-cod.service`/`soundsync-cod-watch.sh`), `debian/postinst`
  (never enables it). On bluez 5.85 the adapter reverts to "Computer" within seconds of WP starting →
  source devices won't show SoundSync as a speaker → audio plays locally. The project's worst,
  most-expensively-debugged bug, shipped straight back. **Fix:** add both artifacts to `.install`,
  enable in `postinst`, disable in `prerm`/`postrm`.
- **[CRITICAL] NF-10 `.deb` never enables linger** — `debian/postinst` does `--global enable` but no
  `loginctl enable-linger` (G3). Headless box with no login → user `systemd --user` never starts at
  boot → silent after every reboot. **Fix:** read a configured user (debconf/`/etc/default/soundsync`)
  and enable linger, or a loud post-install instruction.
- **[HIGH] NF-31 `.deb` never restarts user WP (G4)** — `debian/postinst`. Config written but not
  loaded → pairs, no audio (AUD-004 symptom) with green diagnostics.
- **[HIGH] NF-32 `.deb` build untested; `dh_install` likely fails** — `soundsync.install` `webui/dist/*`
  glob + the unlisted CoD artifacts (`Rules-Requires-Root: no`, compat 13). Runbook §5 admits the
  `.deb` build was never run end-to-end. **Fix:** run `dpkg-buildpackage` once; `--list-missing`.
- **[HIGH] NF-33 Build relies on root's `$HOME/.local/node`** — `debian/rules:14-19`. Non-reproducible;
  falls back to PATH `nodejs`/`npm` (never exercised) and `npm ci` reaches the network, breaking the
  stated "air-gap-friendly" goal. **Fix:** drop the `~/.local/node` branch; vendor/`--offline`.
- **[HIGH] `--reset` doesn't start `pipewire.socket`; re-pins CoD before WP up** — `install.sh:266-278`.
  On socket-activated PipeWire, starting `.service` without `.socket` can leave pactl unresponsive
  within the 10×1s poll → no `soundsync-capture`; CoD may sit wrong up to 30s.
- **[MEDIUM] User unit orders after PipeWire *start*, not *readiness*** — `soundsync.service:3-4`. No
  `pipewire.socket` in `After=`/`Wants=`; leans 100% on the app's AUD-027 poll — which is **missing**
  (NF-5). On a slow cold boot, `Restart=always`/`RestartSec=2` hammers until PW is ready.
- **[MEDIUM] No `StartLimit*` on any unit** — `soundsync.service:10-11`. A deterministic startup crash
  → infinite 2s restart storm rather than a clean `failed` state. (`input.md §2` documents NRestarts
  hitting 53.) **Fix:** `StartLimitIntervalSec`/`StartLimitBurst` (weigh vs "must always come back").
- **[MEDIUM] CoD watcher busy-restart + unfiltered firehose** — `soundsync-cod-watch.sh:49-58`,
  `soundsync-adapter-cod.service:13-14`. If `dbus-monitor`/`hciconfig` is absent or BlueZ is slow, the
  pipeline EOFs instantly → exit → restart in 2s → tight loop. And the match rules have no
  `sender=org.bluez`/path filter, so *every* system-bus `PropertiesChanged` (NetworkManager, logind, …)
  wakes the loop and forks a `hciconfig`. **Fix:** guard the binaries (fail clean), `sleep` before exit,
  narrow the match to BlueZ.
- **[MEDIUM] `_setup_node` reads root's `$HOME` under sudo** — `install.sh:324`. `/root/.local/node`,
  not the appliance user's; inconsistent with README guidance, "node not found" without `SOUNDSYNC_NODE_BIN`.
- **[MEDIUM] `apply-wireplumber-config` failure only warns (violates AUD-019)** — `install.sh:458-464`,
  `debian/postinst:8-13`. "Config written ≠ loaded"; a failed apply still reports success. **Fix:** hard
  error / verify the file content after writing.
- **[MEDIUM] AUD-015/054/055/056 dropped vs legacy** — `install.sh`, `debian/*`. No WP
  `MemoryDenyWriteExecute=no` override (LDAC/aptX silently downgrade to SBC under systemd), no RT limits
  in `limits.d`+`user.conf`+`LimitRTPRIO` (PipeWire stays SCHED_OTHER → Pi stutter), no `performance`
  governor/rtkit, no `net.core.wmem/rmem_max=2MB` (WebRTC packet drops under load). Validated for SBC/AAC
  only, on one beefy x86 box — **the RT omission is exactly the Pi-stutter the legacy fixes existed for.**
- **[MEDIUM] Whole stack validated only on one x86 Ubuntu 26.04 box** — Pi OS / ARM / WP 0.4-Lua /
  non-systemd / no-linger paths untested; multiarch RAOP-module path and the WP-0.4 individual-prop Lua
  rule (AUD-018) unexercised; `hciconfig` increasingly absent.
- **[LOW] `changelog` frozen at `0.0.1`** (no CoD-watcher/linger entry), **`uninstall.sh`/`postrm` purge
  the wrong WP-Lua path** (`…/wireplumber.conf.d/51-soundsync.lua.d` vs the real `…/bluetooth.lua.d/`),
  **uninstall doesn't stop the running user daemon or disable linger**, **CoD watcher `Documentation=`
  points at a doc the package doesn't install**.

**ROBUST:** `install.sh` (`set -euo pipefail`, idempotent, correct `run_user` XDG/DBUS export per
AUD-020, `require_target_user`, webui clear-before-copy G9); G1/G2/G5/G6 genuinely encoded (cmake,
apt deps, libspa-0.2-bluetooth, rfkill, dbus-bin); `debian/control` Build-Depends/Depends correct;
the user unit's graceful-shutdown contract (`TimeoutStopSec=15 > 8s`, `KillMode=mixed`); adapter
de-hardcoding consistent across `install.sh`/watcher/`main.rs` (only `doctor.rs` missed); the
port-in-use self-check excluding the daemon's own process.

### 3.10 WebUI (Preact SPA)

- **[CRITICAL] NF-11 AUD-051 violated — `evt.streams[0]`** — `webui/src/lib/webrtc.ts:104`. webrtc-rs
  `add_track()` doesn't associate a stream, so `evt.streams[0]` is `undefined` → `?? null` → no audio
  source → **silent playback on every browser.** **Fix:** `new MediaStream([evt.track])`.
- **[CRITICAL] NF-12 AUD-067/068/069 violated — Safari priming** — `webui/src/lib/webrtc.ts:38-48,96-105`.
  No synchronous `audioEl.play()` inside the click before the first `await` (the `<audio>` is created
  lazily in async `ontrack`); no muted-toggle unlock on play rejection. Safari/iOS autoplay blocks it
  with `NotAllowedError` → **no audio on the primary phone-listening use case.** **Fix:** create+`.play()`
  the element synchronously in `handleStart` before any await; muted-unlock fallback in `ontrack`.
- **[CRITICAL] NF-13 Type drift — `device_name` not consumed; rename never shows** —
  `webui/src/lib/types.ts:66-90`, `store.tsx:60-88`, `Devices.tsx:300`. Rust emits a `device_name`
  snapshot field + a `device_name` WS message (`state.rs:117-126`, `ws.rs:66-69,90`); the TS contract
  omits both and the store's switch silently drops it. `Devices.tsx:300` is
  `snapshot?.version ? 'SoundSync' : 'SoundSync'` — a hardcoded literal in *both* ternary branches. The
  Speaker-Name rename never appears; the pairing banner always says "SoundSync". **Fix:** add the field
  + message to TS, handle in the store, read `snapshot.device_name`.
- **[CRITICAL] AUD-073 empty-name guard not preserved (FE side)** — `store.tsx:70-72`. `bluetooth_devices`
  is wholesale-replaced (`setDevices(msg.devices)`) with no per-`addr` name-preservation merge; a blank
  name from the backend blanks the UI. (Partly mitigated server-side, but the ledger lists a FE
  obligation that's absent.) **Fix:** merge by `addr`, keep an existing non-empty name over an empty one.
- **[HIGH] NF-34 AUD-070 no reconnect jitter** — `webui/src/lib/ws.ts:6-8,71-77`. Deterministic 1s→30s
  ×1.5, no `Math.random()`. A daemon restart drops every tab's socket; identical timers → thundering-
  herd reconnect storm hitting the just-restarted daemon in lockstep. **Fix:** `+ Math.random()*1000`.
- **[HIGH] NF-35 AUD-050 null-mid candidates not dropped + snake_case not normalized** —
  `webui/src/lib/webrtc.ts:55-67`. ICE buffering (AUD-049) is correct, but candidates with both
  `sdpMid`/`sdpMLineIndex` null are passed straight to `addIceCandidate` → Safari throws → spurious
  "Listen error" toast / aborted session. **Fix:** drop null-mid candidates before applying.
- **[HIGH] NF-36 The WS/WebRTC/store/api/tabs are untested** — `ws.test.ts` doesn't import `ws.ts` (it
  tests type shapes + a *re-implemented copy* of the reducer). `webrtc.ts`/`store.tsx`/`api.ts` and all
  tabs/components have zero coverage — which is why the AUD-051/067/070 regressions shipped past the
  "28 web tests" gate. **Fix:** real `createWsClient` tests (fake timers, jitter bounds, malformed-frame
  survival, teardown) + a `webrtc.ts` test asserting ICE flush order and `new MediaStream([track])`.
- **[MEDIUM] Hardcoded STUN, no override** — `webui/src/lib/webrtc.ts:41` (matches AUD-052 but a public-
  internet dependency at connect).
- **[MEDIUM] Transient `disconnected` treated as terminal error** — `webrtc.ts:86-93`. A momentary
  Wi-Fi blip flips the button to error; and Listen doesn't observe `store.wsConnected`, so after a WS
  reconnect (which invalidates the server session per AUD-053) the UI shows stale "playing". **Fix:**
  treat `disconnected` as soft; reset/restart Listen on WS drop.
- **[MEDIUM] Structurally-valid-but-wrong WS frame crashes the render** — `ws.ts:51-58`, `store.tsx:62-67`.
  `JSON.parse` is guarded, but a snapshot missing `bluetooth_devices` → `devices.map(...)` throws (the
  type drift above is one trigger). TS `?` optionals don't protect at runtime. **Fix:** defensive
  defaults (`?? []`) when applying a snapshot.
- **[MEDIUM] `package.json` `jsdom: ^29.1.1` is suspicious** — likely not a real published major (latest
  ~24–25); could break `npm ci` on a clean checkout. **Fix:** verify the lockfile / pin to a real version.
- **[LOW] Spectrum rAF loop free-runs on hidden tabs** — `NowPlaying.tsx:31-89` (CPU/battery, bounded;
  NowPlaying is never unmounted — panels use `hidden`). **[LOW]** `AlbumArt onError` hides the `<img>`
  permanently (a later valid URL stays hidden — add `key={url}`). **[LOW]** EQ vertical slider relies on
  Firefox `orient` + CSS fallback elsewhere (untested cross-browser).

**ROBUST:** AUD-048 casing (both sides, tested), AUD-049 ICE buffering, AUD-071 single WS; themes (7
presets, no-dup-ids, `readAccent` widened so M4 resolved, dim/accent2 present, index-0 indigo-cyan
asserted); device-icon stickiness (localStorage cache + BlueZ `icon` plumbing, M6 correct); `api.ts`
graceful degradation (network-0/404/503 → "unavailable" not wedge); timer/listener cleanup on unmount
(no leak); **WSS-behind-TLS derivation is correct** (`ws.ts:40-43`); `__APP_VERSION__` surfacing correct.

---

## 4. Cross-cutting reference

### 4.1 Ledger (AUD/UB) preservation matrix

| Status | Items |
|---|---|
| **Preserved & tested** | AUD-006 (agent, improved), AUD-010 (audio-node gate), AUD-011 (AVRCP liveness), AUD-012 (path enc), AUD-021/022/026 (sink/route/dup), AUD-025 (filter fields), AUD-034 (.monitor), AUD-036 (frame), AUD-038/040/041/045/047/052/053 (WebRTC), AUD-039/072 (broadcast), AUD-042 (HTTP pipe), AUD-048/049/071 (WS/ICE), AUD-063 (cast pull), themes/icons |
| **Improved over ledger** | AUD-024 (live `pw-cli set-param`, no 200ms restart), UB-1/AUD-037 (latency derived ≥2×quantum), AUD-061 IPv6 handling (IPv4-first sort) |
| **Dead-code removed (good)** | AUD-001 (`endpoint.rs` gone), AUD-013/016 (`codecs/*` gone) |
| **DROPPED — deliberate but unreconciled** | AUD-007 (auto-stop discovery), AUD-008/009 (active scan / stale-session / name resolution / Identify), AUD-029 (tool fallbacks, partial), AUD-058/059/060 (AirPlay pw-link/wake/ports), AUD-064 (8.8.8.8 fallback), AUD-066 (mdns fallback) |
| **DROPPED — likely regressions** | AUD-005 (bluetooth group), AUD-027 (readiness poll), AUD-043 (AAC profile/cutoff), AUD-061 (`\NNN` decode), AUD-062 (RAOP/avahi availability), AUD-065 (cast heartbeat 3-strikes), AUD-015/054/055/056 (installer RT/codec/UDP tuning) |
| **Still open (as in ledger)** | **UB-6** (eq→capture link never verified — NF-14), UB-7 (moot: fallback removed) |

### 4.2 Hard-coded values worth surfacing to config / documenting

- Network: STUN `stun.l.google.com:19302` (server `webrtc_rs.rs:34` + client `webrtc.ts:41`, no
  override/TURN); cast control port `8009`, app `CC1AD845`; default bind `0.0.0.0:8080` (`config.rs:11`).
- External service: iTunes Search API + `mzstatic.com` artwork (`artwork.rs`), per-track, default-on.
- Identity/adapter: CoD `0x240414` (`SOUNDSYNC_COD` env in the watcher only), `hci0` literal in
  `doctor.rs:147` (everywhere else resolved).
- Timeouts/intervals: shutdown 8s/unit 15s, reconcile debounce 200ms, AVRCP 5s/2s/3s, cast heartbeat
  5s/refresh 10s, supervisor 100ms→5s/stable 10s, pairing window 300s, spectrum 50ms. Mostly fine;
  the shutdown one can be inverted by env (NF — `main.rs`).
- Audio: 48k/2ch/f32/1920/7680, broadcast 256, Opus 128k/4000-byte buf, FFT 2048/64 bands/α0.35/−80dB,
  EQ ±12 dB/10 bands. Centralized & asserted (good) — but `spectrum.rs:65` re-literals `48000.0`
  instead of importing `frame::SAMPLE_RATE`.

### 4.3 Environment variables

| Var | Read at | Documented in README? |
|---|---|---|
| `SOUNDSYNC_BIND` / `SOUNDSYNC_WEB_DIR` / `SOUNDSYNC_NAME` / `SOUNDSYNC_HCI` / `SOUNDSYNC_SHUTDOWN_TIMEOUT` | `config.rs`/`main.rs` | ✅ |
| **`SOUNDSYNC_ARTWORK`** (defaults **on**, phones home to Apple) | `artwork.rs:127` | ❌ undocumented |
| **`SOUNDSYNC_COD`** (CoD override) | `soundsync-cod-watch.sh:30` | ❌ undocumented |
| `SOUNDSYNC_NODE_BIN` (build-time node path) | `debian/rules`, `install.sh` | ❌ (packaging only) |
| `HOME` / `XDG_RUNTIME_DIR` (default to `/root`, `/run/user/0`) | multiple | n/a — odd for a user service |

### 4.4 Task supervision map (the crux of "stays up for months")

| Task | Spawned at | Supervised? | On panic | On systemd restart trigger? |
|---|---|---|---|---|
| capture pump | `daemon.rs:149` | ✅ `supervise()` | task aborts (panic≠Err) | only if process exits |
| `pactl subscribe` events | `daemon.rs:164` | ✅ `supervise()` | task aborts | " |
| EQ filter-chain subprocess | `eq_controller.rs:254` | ✅ `supervise()` | task aborts | " |
| `run_bluetooth` | `main.rs:164` | ❌ | dead forever | ❌ process stays up |
| web `serve` | `main.rs:272` | ❌ | dead forever | ❌ |
| `run_spectrum` | `main.rs:113` | ❌ | dead forever | ❌ |
| `webrtc_command_loop` | `main.rs:123` | ❌ | dead forever | ❌ |
| per-session Opus pump | `webrtc_rs.rs:191` | ❌ | that session silent | ❌ |
| `run_media_manager` | `main.rs:186` | self (`'reconnect` loop) | inner loop is the guard | ❌ |
| `run_eq_controller` / `run_output_controller` | `main.rs:198/248` | partial (own children) | controller task dead | ❌ |

---

## 5. Long-term stability verdict (months, with & without audio)

**Without audio (idle):** mostly stable, but three accumulators creep in any high-traffic place — the
BT device map + per-device watcher tasks (NF), leaked WebRTC sessions on abrupt browser closes (NF-7),
and (if anyone re-selects often) cast sessions. The capture `parec` correctly streams silence without
respawning, the spectrum/pumps park on `recv()` (no spin), and the SPA is leak-free as a read-only tab.

**With audio:** the happy path is solid and well-tested, but the system is **fragile to exactly the
events months of uptime guarantee** — suspend/resume, `bluetoothd`/WP/pipewire-pulse restarts, device
reboots, and lossy Wi-Fi. The first failures, in order:

1. A **wedged `pactl`/`wpctl`** freezes reconciliation with no timeout (NF-1) — the most likely "daemon
   alive, audio dead" cause.
2. A **WP/PipeWire restart** leaves a lingering null-sink module → reuse falsely succeeds → silent dead
   capture (NF-6); and the eq→capture link is never verified (NF-14).
3. **BT goes stale after the first suspend / `bluetoothd` restart** with no reconnect, and isn't
   restarted because it's unsupervised (NF-3/NF-4).
4. **WebRTC/Chromecast sessions leak/wedge** on abrupt disconnects (NF-7/NF-8/NF-26) — unbounded growth
   of tasks/PCs/sockets/CPU over months.
5. A **panic in any unsupervised task** silently kills that feature (NF-2) — systemd can't see it.

**Does it stay up / auto-restart?** The *process* auto-restarts (`Restart=always`) only when it
actually exits. The crash-loop regression is genuinely fixed. But **subsystem death is invisible to
systemd**, and a deterministic startup crash (e.g. NF-5/NF-28 on a slow cold boot) has **no
`StartLimit` backstop** → either a clean fix is needed or it storms. Net: it will run for months *as a
read-only monitor and on an undisturbed happy path*; it will **not** reliably survive months of real
adapter/PipeWire/network churn as wired today. The two highest-leverage fixes are **(1) a command
timeout** and **(2) supervising the `main.rs` tasks (or making any task exit process-fatal)**.

---

## 6. Fresh-machine & old/partial-machine install

**Fresh machine (via `install.sh`, x86 Ubuntu 26.04):** high success — bare-metal → doctor-green was
exercised. Caveats: the **build hard-fails without `libdbus-1-dev` + `libopus-dev` + `cmake` +
`pkg-config`** (empirically confirmed here); the *full clean build+install in one command* and the
`.deb` build were never run end-to-end (runbook §5).

**Old / partial machine — biggest risks (ranked):**
1. **`hciconfig`/`hcitool` absent** (deprecated) → the entire CoD-pin strategy silently no-ops, "won't
   appear as a speaker," no clear error (both the oneshot and the watcher swallow it).
2. **`.deb` install** → missing CoD watcher (NF-9), linger (NF-10), user-WP restart (NF-31): reproduces
   the two hardest solved bugs. **Treat `.deb` as unsupported until fixed.**
3. **WirePlumber 0.4 (Lua)** and **ARM/Pi OS** paths untested; the unreadable-`--version`→Lua fallback
   (NF-18) can write the wrong format on a 0.5 box.
4. **Missing `pulseaudio-utils`** (no `pactl`/`parec`) → graph fails to build (AUD-029 fallbacks gone).
5. **Missing `avahi-daemon`/`avahi-utils`** → silent empty cast/AirPlay lists, no mdns fallback (NF-27).
6. **No RT tuning** (AUD-055/056 dropped) → PipeWire SCHED_OTHER on a Pi → stutter at low CPU.
7. **`npm ci`** flags 5 high-severity dev advisories; `jsdom ^29` may not resolve on a clean checkout.

**No-systemd / no-linger hosts:** no story at all — the whole model assumes systemd user services + linger.

---

## 7. Feature completeness vs the design docs

- **Claimed working & verified present:** A2DP capture path, AVRCP metadata, spectrum, 10-band EQ
  (improved live-gain path), WebRTC "Listen" (backend), 3 outputs (with the AirPlay caveat NF-25),
  Preact UI (4 tabs + pickers), 7 themes, CoD watcher, doctor/`--reset` installer modes, graceful
  shutdown — all present.
- **Claimed working but effectively broken/regressed:** browser "Listen" on **Safari/iOS** (NF-11/12 —
  likely silent); the **Speaker-Name rename** (NF-13 — never surfaces); **album art** is a lossy iTunes
  guess (UB-4 redesigned), not AVRCP cover art, and frequently wrong/missing/privacy-leaking; **AirPlay
  to a SUSPENDED RAOP sink** unverified (NF-25); **Chromecast reconnection** absent (NF-8).
- **Explicitly NOT built (per `input.md`, confirmed in code):** the input-source selector + per-device
  face (Wave 3, gated on the un-run IN-0 spike); the null-sink **periodic** reconcile; **W2.2** a2dp/
  wp-config **content** doctor check (NF-17); `config_store` consumers (dormant). The backstop/graceful-
  shutdown work (Wave 0) and themes/version (Wave 1) are landed.
- **Doc drift:** README documents 5 env vars (misses `SOUNDSYNC_ARTWORK`/`SOUNDSYNC_COD`); `lib.rs`
  references `research/audio-routing-nuances.md` (now `docs/…`); `input.md §6` claims `hci0` fully
  removed (still in `doctor.rs:147`); `debian/changelog` frozen at `0.0.1`.

---

## 8. Suggested remediation order (highest leverage first)

1. **NF-1** add a command timeout (unblocks the #1 months-long hang).
2. **NF-2/3/15/19/30** supervise the `main.rs` tasks (or make any unexpected task exit process-fatal);
   convert supervised-unit panics to restartable errors.
3. **NF-7 + WS keepalive** register `on_peer_connection_state_change` + a WS ping (stops the WebRTC leak).
4. **NF-6 + NF-5 + NF-28** verify the null-sink node on reuse; add the AUD-027 readiness poll; retry
   `set_default_sink` (closes the cold-boot loop + WP-restart silent death).
5. **NF-9/10/31** fix the `.deb` (CoD watcher + linger + user-WP restart) or mark it unsupported.
6. **NF-11/12/13** the three webui CRITICALs (Safari audio + rename) — small, high user-visible impact.
7. **NF-8/26** Chromecast liveness/reconnect.
8. **NF-14** verify the eq→capture link (UB-6).
9. **NF-17/18** doctor content-check + the WP-format-fallback safety.
10. Add `StartLimit*`, narrow the CoD-watcher match, document `SOUNDSYNC_ARTWORK`, and add real webui +
    `server.rs`/`webrtc_rs.rs` integration tests (NF-36) so these don't silently regress again.

---

## 9. What is genuinely robust (do not regress)

The frame/PCM contract and broadcast bus; the parsers (token-boundary matching, soft-fail); the BT
state-machine reducer; AVRCP D-Bus liveness recovery (AUD-011) + deterministic sticky `select_player`;
the EQ filter-chain supervision + live-gain path + atomic config writes; the WebRTC RTP math (no pacer,
lag advance, wrap) and clean-disconnect teardown; the output loopback single-active discipline; the
protobuf/JSON/discovery parsers (total, garbage-tested); `install.sh`'s `run_user`/idempotency/G1-G2/G5/
G6/G9; the graceful-shutdown contract and the crash-loop fix; the supervisor *mechanism*; the SPA's
graceful degradation, theme system, device-icon stickiness, and WSS derivation. The engineering is
careful — the gaps are lifecycle wiring, long-lived-connection liveness, the ledger divergences, and
the `.deb`.

# tobefixed.md — survivors of a skeptical teardown of needsfixing.md

This document is the filtered output of an adversarial, file:line-grounded re-review of `needsfixing.md`, an audit written without access to the running appliance. Ten subsystem reviewers re-opened every cited location in `/home/mark/sc/sc-next`, checked each claim against the rebuild's actual architecture (passive A2DP sink, user systemd service, reactive CoD watcher) and against the verified LIVE FACTS, and defaulted to DISMISS. Of the ~75 findings examined, **20 survived as CONFIRMED (KEEP)**, **6 require LIVE VERIFICATION**, and the remainder were **refuted** as wrong-premise misreads, intentional architectural changes, or claims contradicted by the running build. The method: a finding was kept only if, after reading the real code, it could not be refuted and was not already settled by live reality; weak KEEPs were downgraded toward skepticism.

---

## A. CONFIRMED — to be fixed (KEEP)

### Critical
*(none — no finding survived at this severity; every "[CRITICAL]" silent-playback / Safari-broken claim was empirically refuted by the live build.)*

### High

**NF-9 — .deb omits the CoD watcher**
`debian/soundsync.install:1-4`, `debian/postinst:24-28` (vs `packaging/install.sh:445-453,499`)
Survives: the `.deb` ships neither `soundsync-adapter-cod.service` nor `soundsync-cod-watch.sh`, and `postinst` never enables a watcher — so a `.deb` install loses the reactive guard against bluez 5.85 recomputing CoD to "Computer" on every WP endpoint re-registration (the expensively-debugged AUD-002/G8 bug; live facts cover only the install.sh box, not the .deb path).
Fix (audit correct): add both artifacts to `debian/soundsync.install`, enable the unit in `postinst`, disable/clean in `prerm`/`postrm`. Severity is HIGH not CRITICAL because no `.deb` artifact is currently produced/deployed — latent until someone installs via `.deb`.

**NF-10 — .deb never enables linger**
`debian/postinst:28` (vs `packaging/install.sh:475-477`)
Survives: `postinst` does `systemctl --global enable` but never `loginctl enable-linger`, so on a headless box the user systemd manager (and thus PipeWire + `soundsync.service`) never starts at boot.
Fix (audit only partial): enable linger in the maintainer script, but the audit's fix understates that `postinst` can't reliably know the appliance user — the right route is debconf (or `/etc/default/soundsync`), not a "loud post-install note," which is too weak for a headless appliance.

### Medium

**NF-6 — Null-sink reuse path trusts the module, never verifies the node**
`null_sink.rs:18-24,39-44` (reuse path skips the `pactl list short sinks` node check the create path does at `:71-83`)
Survives: if the pulse module record survives but the sink node is dropped (e.g. WirePlumber restart that tears the node but leaves the module registered), `ensure_null_sink` short-circuits `{reused:true}` forever and capture taps a non-existent `soundsync-capture.monitor` → silent; the reconcile loop re-runs `bring_up` but hits the same blind reuse, so it cannot self-heal.
Fix (audit correct): on the reuse path, verify the node via `pactl list short sinks`; if absent, unload the stale module and fall through to create. Medium not critical — needs a specific WP-restart-without-module-teardown sequence.

**NF-15 — Web server `serve` unsupervised; bind failure kills UI for process lifetime**
`main.rs:272-284`, `server.rs:209-218`
Survives: `serve` is a bare spawn; on `Err` (only `TcpListener::bind`, line 212) the closure just `eprintln!`s and the task ends with no retry — an AddrInUse-after-fast-restart race permanently loses the control UI.
Fix: audit's `supervise("web", ...)` is type-correct (`ServerConfig`/`WebContext` are `Clone`, `serve()` matches the bound, supervise breaks on shutdown). A `SO_REUSEADDR` listener + bounded bind-retry is an equally valid, simpler fix for the AddrInUse case. Downgraded HIGH→MEDIUM: only the UI is lost, only after operator-induced rapid restart.

**NF-7 — No `on_peer_connection_state_change` + no WS keepalive → half-open clients leak PC + Opus pump**
`webrtc_rs.rs:157` (only `on_ice_candidate` registered), teardown is WS-driven only at `server.rs:191-201`
Survives: an abrupt half-open client (Wi-Fi drop, sleep, NAT rebind, ICE Failed) leaves the WS loop parked; on a quiet/idle bus the spectrum stream produces no sends (`spectrum.rs:37-52`), so nothing trips the WS, and the PC + detached `spawn_blocking` Opus pump (`webrtc_rs.rs:191`) survive until OS TCP keepalive reaps them (minutes). The passive-sink design makes the quiet-bus window common. Not refuted by live facts (those prove only clean disconnect).
Fix (audit correct): register `on_peer_connection_state_change`, send `Stop` on Failed/Closed/Disconnected; add WS ping/idle-timeout as secondary defense. MEDIUM not CRITICAL — bounded by keepalive, one PC+pump per leak. **NF-7 subsumes "WebRTC: no-WS-keepalive" (kept, LOW, folded in).**

**WEB: half-open-WS-leak** *(folds into NF-7)*
`server.rs:115-202` (no WS ping send, no pong/idle deadline)
Survives: independently warranted as the WS-side half of NF-7 — a silently-severed TCP leaves `socket.recv()` (line 159) parked, accruing PCs+FDs+CPU across repeated half-opens. Fix: periodic WS ping + pong deadline → break → existing `Stop` teardown. Implement together with NF-7.

**NF-37 — No auth + binds 0.0.0.0:8080 + uncapped /api/stream spawns**
`config.rs:11`, `server.rs:59-101`, `stream.rs:65-105`
Survives (the stream-cap half only): every GET `/api/stream/audio.{aac,mp3}` unconditionally spawns a `parec|ffmpeg` pair via `sh -c` with no concurrency cap — a loop of stream GETs is a real FD+CPU exhaustion vector from any LAN host.
Fix (audit partial): add a semaphore → `503` on the stream endpoint (the high-value, correct mitigation). The bind/auth halves are **design choices for a LAN appliance**, not defects: `0.0.0.0` is deliberate (LAN control), binding loopback would break the documented use case, and auth is out of scope. KEEP only the concurrency cap; downgraded HIGH→MEDIUM (trusted-LAN threat model).

**NF-1 — Shelled-out commands have no timeout; a wedged pactl/wpctl blocks reconcile**
`command.rs:30` (`std::process::Command::output()`, synchronous, no timeout), reached via `daemon.rs:78-79,191,307`
Survives: a `select!` cannot cancel a blocking syscall already executing on the runtime thread, so a truly wedged child freezes that reconcile/admin path until it returns.
Fix (audit correct): `wait_timeout`, or `spawn_blocking`+timeout, or `tokio::process`+timeout. MEDIUM not critical — the audit **overstates** the consequence: SIGTERM is NOT lost (the backstop still `exit(0)`s after 8s and systemd restarts), so it is a self-healed latent hang, not "frozen forever."

**NF-2 — Supervisor wired to only 2 of ~10 tasks; a panic kills the subsystem, process stays up**
`daemon.rs:142-151,158-166` are the only `supervise()` sites; `main.rs:113,123,164,186,198,248,272` are bare spawns
Survives: a panic in any unsupervised subsystem silently removes it while the process + `Restart=` stay satisfied, so systemd never restarts.
Fix (audit partial — important correction): wrapping in `supervise()` does NOT make panics recoverable (panic ≠ Err; see NF-30). The correct robust choice is the audit's *alternative*: a top-level join that makes any unexpected task exit **process-fatal** (so systemd restarts). MEDIUM — only bites on an actual panic, none observed live.

**NF-8 — Dropped Chromecast never recovers; UI shows active while silent**
`client.rs:206-366`, `controller.rs:188-189,271-338`
Survives: `run_cast_session` `return`s on EOF/read-error/any write failure; the controller stores only `Option<CastHandle>` (no JoinHandle) and the refresh tick never checks cast liveness, explicitly keeping `active` when the device leaves discovery — so a rebooted/roamed Chromecast leaves `active=Some(dev)` published forever while the task has exited.
Fix (audit partial): the audit's "leaks the session task" is **FALSE** (the task fully returns; only one dead `watch::Sender` lingers). Minimal correct fix: store the JoinHandle + check `is_finished()` on tick, then clear `active`/republish. The doc-comment at `client.rs:184-186` is backwards and should be corrected. Downgraded to MEDIUM (cosmetic-state-only, single-output appliance, re-select is one click).

**NF-13 — device_name type drift; rename never shows in UI**
`webui/src/lib/types.ts:66-90` (omits `device_name`), `lib/store.tsx:60-88` (no case), `tabs/Devices.tsx:300` (`... ? 'SoundSync' : 'SoundSync'`), `Status.tsx:69` (hardcoded initial, never seeded)
Survives: the backend has the feature end-to-end (`state.rs:98,205,211-216`; `ws.rs:67-69,90-92`; `server.rs:82`) and the BlueZ alias *does* change (the phone sees it), but the TS contract drops `device_name`, so the pairing banner always says "SoundSync" and the rename field resets on reload.
Fix (audit correct): add the field + message to TS, handle in the store, seed from `snapshot.device_name`. MEDIUM — core name works; only UI display is stale.

### Low

**BT: per-device watcher tasks accumulate** — `bluer_devices.rs:168-176,107-110`
Survives: `spawn_device_watcher` runs for *every* `DeviceAdded` regardless of paired/connected; the `watched` dedup is per-discovery-run and discovery never restarts, so in a busy RF environment every BLE advertiser gets a permanent `dev.events()` task + D-Bus match rule, never reaped — genuine unbounded growth on a long-lived appliance. Fix (audit partial): "reap on disconnect" is right in spirit; simplest correct fix is to **only spawn the watcher for paired devices** (or drop it from the JoinSet on `Connected(false)` for unpaired).

**AVRCP: pre-player-search-no-DbusHealth** — `media.rs:446-459,148-153`
Survives: the pre-bind wait loop's `find_active_player` returns `None` identically for "D-Bus error" and "no player yet," then retries the *same* connection forever; no `DbusHealth` counter exists pre-bind (all recovery is in the post-bind loop). If the bus dies before any player binds, it never rebuilds. Fix (audit correct): have `find_active_player` distinguish D-Bus error from "no player," count errors against `DbusHealth`, `continue 'reconnect` on threshold. LOW — narrow window, self-heals when a player reappears.

**NF-26 — No heartbeat PONG-timeout; half-open TCP wedges cast session** — `client.rs:270-365,290`
Survives: PING is sent every 5s and inbound PINGs answered, but inbound PONG/last-activity is never tracked and the read arm has no deadline (AUD-065's "3 strikes" dropped) — a true half-open leaves `active` published until the kernel TCP retransmit timeout. Fix (audit correct): track last-inbound-frame time and declare lost after N×heartbeat, or wrap the read in `tokio::time::timeout`. LOW — overlaps NF-8; fixing NF-8's liveness check largely subsumes it.

**WEB: offer-awaited-inline** — `server.rs:169`
Survives: `if let Ok(Ok(answer)) = rrx.await` runs inside the `socket.recv()` select arm, so that one connection observes neither shutdown nor broadcast events until the oneshot resolves. Blast radius = one connection, bounded by the process backstop. Fix (audit correct): race `rrx.await` against shutdown + a timeout. LOW.

**WEB: ffmpeg-missing-200-empty** — `stream.rs:67-105`
Survives: `sh -c` spawns whenever `/bin/sh` exists, so the `503` arm only catches sh-spawn failure; missing `parec`/`ffmpeg` yields `200 OK` + `audio/aac` header + empty body, and stderr is `Stdio::null()` so the operator gets no diagnostic. Fix (audit correct): preflight `which`/`try_wait()` before emitting 200, and stop discarding stderr. LOW — only bites a partial/broken box.

**NF-16 — exit(0) backstop skips Drop (BlueZ agent + discovery left registered)** — `main.rs:339-345`, `service.rs:40-42,56,81`
Survives: the backstop `std::process::exit(0)` skips all Drop, leaving the auto-accept agent + discovery registered and children unreaped across a restart. Fix (audit correct), but BACKSTOP-only (reached only if the clean path fails within 8s, which live facts say doesn't happen); the deeper fix is NF-1. LOW.

**NF-17 — Doctor WP-config check is existence-only, not content-aware** — `doctor.rs:120-142`
Survives: the path is computed format-correctly, but `read_to_string` passes on any `Ok(_)` regardless of contents (test fixture is literally `# config`), so a truncated/wrong-content file at the right path yields a green check while WP loads nothing. Fix (audit correct): assert contents match `generate(fmt)` as oracle (markers at `config.rs:17,22,33`). LOW — latent false-PASS only on a corrupted deploy.

**NF-30 — Supervised units unrecoverable on panic (panic ≠ Err)** — `supervisor.rs:55-57,81-85`, `daemon.rs:142,158`
Survives: `supervise` restarts on `Ok`/`Err` completion, but a panic unwinds the supervise future inside `tokio::spawn` → `JoinError`, so capture/events die permanently on panic. Fix (audit correct): spawn the unit and treat `JoinError` as a failed run, or `catch_unwind`. LOW — root cause of NF-2's panic gap; fix together.

**Shutdown-timeout not clamped below unit TimeoutStopSec** — `main.rs:381-388`, `soundsync.service:15`
Survives: `resolve_shutdown_timeout` accepts any `u64>0` unbounded; `SOUNDSYNC_SHUTDOWN_TIMEOUT=30` makes the app backstop fire *after* systemd SIGKILLs at 15s, defeating the backstop. Fix (audit correct): clamp below a known ceiling (or warn). LOW — operator-misconfiguration only; default (8) is safe.

**Doctor probes hardcoded hci0** — `doctor.rs:147` (vs `main.rs:353-372` `resolve_adapter()`)
Survives: doctor warns "no hci0 adapter" on an hci1/hci2-only box where the daemon works fine. Fix (audit correct): share `resolve_adapter()`. LOW — cosmetic Warn, no runtime impact.

**REQUIRED_BINS fails on optional/alt tools** — `doctor.rs:54-65,91-97`
Survives: `pw-cat` AND `parec` both mandatory though `detect_capture_tool` needs only one (`daemon.rs:55-69`); `hciconfig` mandatory though deprecated — a box with `parec` but not `pw-cat` (or modern bluez without `hciconfig`) gets a false Fail. Fix (audit correct): "at least one of {parec,pw-cat}", `hciconfig`→Warn. LOW.

**NF-31 — .deb never restarts user WP (config written but not loaded)** — `debian/postinst:8` (vs `packaging/install.sh:479-486`)
Survives: `postinst` writes `/etc/wireplumber/...` as root but never restarts the target user's WP, so a running session won't load the new A2DP config until restart/reboot. Fix (audit partial): restart user WP from `postinst` — but, like linger, `postinst` can't reliably know the target user/UID without debconf. MEDIUM→ kept; "green diagnostics, no audio" window exists only if WP is already running and never restarted (a reboot fixes it).

**NF-34 — No WS reconnect jitter → thundering-herd across tabs** — `webui/src/lib/ws.ts:6-8,71-77`
Survives: deterministic 1s→30s×1.5 with no `Math.random()` jitter (AUD-070 required up-to-1s jitter); a daemon restart reconnects all open tabs on identical timers. Fix (audit correct): `+ Math.random()*1000` in the delay. LOW — single-user appliance.

**NF-36 — web tests don't test ws.ts; webrtc.ts/store.tsx/api.ts/tabs untested** — `webui/src/lib/ws.test.ts` (re-implements the reducer at `:95-116`, never imports `ws.ts`)
Survives: `createWsClient`, reconnect/backoff, jitter bounds, malformed-frame survival, teardown are all unexercised — the gap that let NF-13/NF-34 ship past a green gate. Fix (audit correct): real `createWsClient` tests with fake timers + jitter bounds + teardown. LOW — coverage/process gap.

**AVRCP: album-art-phones-home** — `media.rs:259-327`, `artwork.rs:73,127,138`, `NowPlaying.tsx:167-176`, `README.md:13`
Survives (disclosure gap only): per track, exactly one fetch hits `itunes.apple.com`, and each web client then fetches the mzstatic CDN URL; opt-out `SOUNDSYNC_ARTWORK=off` exists but defaults on and is documented only in a code comment, while README advertises "album art" without disclosing the Apple outbound lookup or the env var. Not a stability issue (offline degrades gracefully to a gradient). Fix: **DOCUMENT the behavior + env var in README** (the audit's default-off / server-side proxy are optional design choices, not required). LOW.

**CAST-LOG: all cast logging is eprintln!, no health in AppState** *(bundle with NF-8)* — `client.rs:216-345`, `controller.rs:192-197,318-321`
Survives: `OutputState{active,available}` has no cast-session-health field and the session has no handle back to `AppStateHandle` — this is the **root enabler** of NF-8's "active while silent." Fix: implement as part of NF-8 (have the session report liveness/errors into AppState). LOW — alone it is logging hygiene.

**LOW bundle: changelog / uninstall paths / docs** — `changelog:1`, `uninstall.sh:16,64-90`, `postrm:6`, `soundsync-adapter-cod.service:3`
Survives (all four verified): (2) the **wrong WP-Lua purge path** is a genuine bug — `uninstall.sh`/`postrm` target `.../wireplumber.conf.d/51-soundsync.lua.d` but the binary writes `.../bluetooth.lua.d/51-soundsync-a2dp.lua` (`config.rs:48-49`, `apply.rs:94,114`), so purge leaves the real Lua config behind on a WP 0.4 box; (3) uninstall never stops the live user instance nor `loginctl disable-linger`; (1) stale changelog; (4) dangling `Documentation=` doc never installed. Fix: correct the Lua purge path and add a live-stop to uninstall (highest value); the rest are cosmetic. LOW.

**LOW: AlbumArt onError hides img permanently** — `NowPlaying.tsx:160-178`
Survives: the single `<img>` has no `key`; `onError` sets inline `display:none`, and Preact reuses the DOM node when `url` changes, so a later valid `artwork_url` stays invisible. Fix (audit correct): `key={url}` forces a fresh `<img>` per URL. LOW — cosmetic.

**MEDIUM: disconnected treated as terminal + no wsConnected observation** *(downgraded to LOW)* — `webrtc.ts:86-93`
Survives: `connectionState 'disconnected'` (self-recoverable) is mapped to `'error'`, and Listen never subscribes to `store.wsConnected`, so a WS drop+reconnect tears down the server-side Listen session while the browser may briefly show stale "playing." Fix (audit partial): treating `'disconnected'` as soft is correct, but the cleaner remedy is to **reset/restart Listen when `store.wsConnected` goes false**. LOW — needs a mid-Listen blip; single-user.

---

## B. NEEDS LIVE VERIFICATION (LIVE-CHECK)

> **Live-check results — run on the appliance 2026-06-13 (4 of 6 resolved):**
> - **NF-18 → REFUTED.** `wireplumber --version` prints `0.5.13` (parseable `MAJOR.MINOR`) → `config_format` selects SpaJson; the Lua fallback never fires, and the on-disk config is SPA-JSON. Latent *only* on a box where the version string is unreadable. Minor hardening nit: make the fallback default to **SpaJson** (the modern default), not Lua. → see Dismissed.
> - **NF-24 → REFUTED.** `mark` is not in the `bluetooth` group, yet `busctl … GetAll org.bluez.Adapter1` succeeds — polkit/default policy grants access. No installer group-add needed. → see Dismissed.
> - **NF-32 → FOLDED into NF-9.** Confirmed `debian/soundsync.install` lists only `soundsync.service` + `soundsync-adapter.service` (no CoD watcher). The audit's "build fails" is wrong — it builds but silently omits the files. The real, kept issue is **NF-9** (High).
> - **AUD-015/054/055/056 → CONFIRMED latent (Pi-only); treat as KEEP (Medium).** WirePlumber here runs `SCHED_OTHER`/prio 0 (not real-time) and the legacy RT-limits (`limits.d` + `LimitRTPRIO`) are absent. Harmless on this x86 box (no stutter observed), but a plausible glitch source on a constrained Pi under load. Fix: ship RT limits for Pi targets; final confirmation still needs a Pi under audio load.
>
> **Still open (need a deliberate/disruptive or browser test):**
> - **NF-4** (below) — needs `sudo systemctl restart bluetooth` + a phone reconnect (disruptive + hardware). Code-side there is no `bluer::Session` re-establish on `adapter.events()` failure, so a real recovery gap is plausible — run the test to confirm severity.
> - **EQ slider `orient`** (below) — needs a visual check in Safari/Chrome on the appliance (cosmetic LOW).

**NF-4 — No recovery from stale bluer::Session/D-Bus after bluetoothd restart / adapter reset** (`service.rs:56-57`, `bluer_devices.rs:147-153`)
Test: open the web UI device list, then `sudo systemctl restart bluetooth`; wait 5s; reconnect a phone and press Pair.
Confirms-bug: no new connect/disconnect events appear, adapter never becomes discoverable until `soundsync.service` restarts, and `journalctl --user -u soundsync` shows `adapter.events() failed` then silence.
Refutes: device events resume and pairing works post-restart.

**NF-24 — Installer never adds user to bluetooth group / no running-creds check** (`install.sh`, `soundsync.service`)
Test: on a fresh target, `id <user> | grep -o bluetooth`; then as that user run `busctl --system call org.bluez /org/bluez org.freedesktop.DBus.Properties GetAll org.bluez.Adapter1`.
Confirms-bug: `AccessDenied`.
Refutes (matches live appliance): the call succeeds, proving polkit/default policy grants access without an explicit group add.

**NF-18 — WP config-format fallback to Lua deletes the correct SPA-JSON** (`probe.rs:13-21`, `apply.rs:18-23,43`)
Test: on the deployed appliance run `wireplumber --version` (read-only).
Confirms-bug: stdout AND stderr lack a parseable `MAJOR.MINOR` (so the fallback would trigger on re-apply).
Refutes: a `0.5.x`+ token that `WpVersion::parse` picks up → SpaJson, so the fallback never fires here.

**NF-32 — .deb build untested; likely silently drops CoD artifacts (not "build fails")** (`debian/rules:23`)
Test: from repo root with node on PATH, `dpkg-buildpackage -us -uc -b` (or `dh_install --list-missing`).
Confirms (NF-9): build SUCCEEDS and `--list-missing` reports `soundsync-adapter-cod.service` and `soundsync-cod-watch.sh` as not-installed.
Refutes the audit's "build fails" prediction (which is wrong; unlisted files are silently not packaged).

**AUD-015/054/055/056 — RT limits / governor / net buffers dropped vs legacy (Pi-only)** (absent from `packaging/`, `debian/`)
Test: on a **Pi** target under audio load, `systemctl --user show wireplumber -p LimitRTPRIO` and `chrt -p $(pgrep -n wireplumber)`.
Confirms-bug: `RTPRIO=0` / `SCHED_OTHER` with audible stutter → add `limits.d` + `LimitRTPRIO=`.
Refutes: WP already RR/FIFO via system rtkit. (Note: `MemoryDenyWriteExecute` is already a documented INFO note at `install.sh:206-208`, only relevant for LDAC/aptX.) MEDIUM, Pi-only.

**LOW: EQ vertical slider relies on Firefox `orient`** (`EQ` tab CSS)
Test: open the EQ tab in Safari and Chrome on the running appliance; visually confirm sliders render vertically and adjust band gain.
Confirms-bug: renders horizontal (purely cosmetic even then, since gain still adjusts).
Refutes: vertical sliders render and respond. Not demonstrable from source alone.

---

## C. DISMISSED — refuted (with reasons)

| id | title | why dismissed |
|---|---|---|
| NF-3 | run_bluetooth spawned unsupervised | Trigger scenarios wrong: broadcast Sender lives in run_bluetooth's own frame (`service.rs:60`), can't close while alive; only narrow hole is shared with NF-4. Live: BT stable. |
| NF-22 | AUD-007 absent (TDM stutter guard) | Rebuild does NO active inquiry (`bluer_devices.rs:87-89` passive); no TDM contention exists. Intentionally moot (case b). |
| NF-23 | AUD-009 absent (two-tier name resolution) | "Read once" is false: name re-read on Connected (`bluer_devices.rs:37-42`), kept sticky (`manager.rs:54-59`); only paired/connected surfaced. Sub-second cosmetic flicker, not stuck MAC. |
| BT: CoD/HCI EPERM every boot | Accurate but best-effort + logged; root `soundsync-adapter.service` + CoD watcher own CoD/name from root. Cosmetic log noise. |
| BT: HCI name re-pin | Friendly name is the BlueZ Alias, set live via bluer (`bluer_control.rs:53-58`); source firmware reads Alias, not raw HCI name. |
| BT: hciconfig hard dependency | Refuted: `bluez`/`bluez-tools` in RUNTIME_PKGS (`install.sh:344-350`), doctor fails loudly if missing (`install.sh:148-152`). Declared + verified. |
| BT: unbounded device map | Premise FALSE: manager only inserts on paired\|\|connected events (`bluer_devices.rs:124,180`); transient phones never enter the map. |
| BT: optimistic bluez_input parsing | Case-mismatch fear unfounded — both sides uppercase (`bluer_devices.rs:127` `Address::to_string()`). AudioActive works live. |
| BT: pairing-window failures swallowed | D-Bus writes don't fail on a healthy adapter (live: pairing works); cosmetic divergence only under NF-4's degraded condition. |
| BT: HOME/XDG default to /root | Systemd user service always exports HOME/XDG; /root fallback only on bare-root-with-stripped-env, not the appliance path. |
| UB-4 reframe | No server-side image pipeline; artwork is a remote URL string the browser loads (`artwork.rs:125-162`). N/A by design. |
| AVRCP: iTunes-body-bound | `into_string()` already ~10MB-capped in ureq 2.x; one-shot per track, 4s timeout, drops immediately. MITM defeats more anyway. |
| AVRCP: magic-numbers | Constants are named (`media.rs:32-37`); upscale no-op is by-design and tested. Style nit. |
| AVRCP: track_from_propmap untested | Panic-safe; BlueZ spec mandates uint32 Duration, `as_u64` fallback covers widths; UI tolerates missing duration. |
| ROBUST claims (AUD-011/select_player/cache) | Verified correct; included for completeness, no action. |
| NF-5 | AUD-027 readiness poll missing | `ensure_null_sink` already retries `load-module` 10× (~35s, AUD-028) as a bounded gate before `set_default_sink`; cold-boot failure premise wrong. |
| NF-28 | set_default_sink no retry | Runs only after `ensure_null_sink` succeeded; mid-run failures retried on next graph event via dual pactl→wpctl path. |
| NF-29 | AUD-029 fallback chains dropped | `pactl` is a hard dependency (`debian/control:30`); the audit's own fix option A (document as required) is already satisfied. |
| AUD-057 zombie-reap via kill_on_drop | At process exit the kernel reaps everything to PID 1; no long-lived parent to accumulate zombies. kill_on_drop adequate. |
| AUD: startup-sweep skipped when module None | module_id is Some on both reuse and create paths; in the duplicate-pileup case `find_existing` matches → sweep runs. |
| AUD: capture-fast-fail-respawn | Misreads backoff: RestartPolicy ramps 100ms→5s cap, not flat 5s; capped retry is correct recovery behavior. |
| AUD: parser-format-drift | All parsers fail soft (defaults/None) and are token-aware + unit-tested; forward-compat preference, not a present bug. |
| AUD: Identify-tone bypasses spawner | Deliberate fire-and-forget diagnostic, kill_on_drop reaps, not in audio data path. Testability nit. |
| NF-14 | EQ→capture pw-link never verified | Architecturally wrong: WP forms the link from `node.target` (AUD-025); the audit's fix would DISABLE EQ. EQ works live. |
| spectrum-task-unsupervised | `process_frame` provably panic-free at FFT_SIZE=2048; missing supervisor moot. Audit concedes "can't wedge capture." |
| map_to_bands-panic | `half` hard-derived from compile-time FFT_SIZE=1024; no runtime path produces half<2. Audit admits unreachable. |
| NaN-EMA-latch | `map_to_bands` can't produce NaN for finite PCM; trigger input doesn't arise in this pipeline. |
| eq-route-reassert-implicit | EQ sink appearing emits a GraphEvent that wakes reconcile AFTER `route_tx.send`; debounce coalescing guarantees a post-update reconcile. EQ toggles correctly live. |
| assert-empty-bands | `gains_to_bands`→`default_bands()` always yields 10 entries; never called with empty slice. Audit concedes unreachable. |
| audio.rate-omitted | Filter-chain runs at capture-side 48k; playback side also 48k, no resample. Benign cosmetic; tidy-up only. |
| filter-chain-base-unverified | Wrong citation (`filter_chain.rs:82-97` is tool detection); EQ config is self-contained in the generated conf. Live: EQ works. |
| opus-error==silence | Encoder built once with valid params, fed canonical 20ms frames; doesn't spuriously fail. Live: Safari Listen works. |
| AUD-043 | AAC drops -profile/-cutoff | ffmpeg `aac` defaults to LC (no-op restatement); -cutoff cosmetic. AAC plays live incl. Safari. |
| WEB: server-no-tests | Reaping structurally guaranteed by `kill_on_drop(true)` (`stream.rs:71`). Test-coverage gap, not a defect. |
| WEB: no-DefaultBodyLimit | Relies on axum's 2MB default; tiny JSON bodies; speculative future-proofing. |
| NF-21 | stop_all drain detached | Audit concedes "not a cross-restart leak; OS frees sockets." Only a missing DTLS close-notify; browser reconnects. |
| NF-19 | Per-session Opus pump unsupervised | Encoder per-session, freshly constructed; no shared poison; only the comment is misleading. Live: audio works. |
| NF-20 | PT 111 hardcoded | Identical to stable legacy (AUD-047); browsers DO negotiate 111. Empirically refuted (Safari works). |
| WebRTC: real-controller-untested | Test-coverage gap; real controller works end-to-end live. CI can't exercise real ICE/DTLS. |
| WebRTC: STUN-SPOF | LAN appliance: host/srflx candidates suffice; STUN unreachable degrades to host-only. TURN irrelevant for LAN. |
| WebRTC: sessions-mutex-HOL | Lock NOT held across awaits (PC cloned out before network await); serial loop a non-issue at 1-2 listeners. |
| WebRTC: pump-no-frame-timeout | Self-dismissing; only load-bearing concern already captured under NF-7. |
| NF-25 | AirPlay module-loopback not pw-link | Deliberate architecture: module-loopback negotiates links + resumes sink itself; AUD-059/060 were pw-link-specific. AirPlay works live. |
| NF-27 | No RAOP/avahi check + no mdns fallback | avahi is a declared hard dependency; mdns dual-path intentionally dropped. Only nit: drop dead `pw-link` from REQUIRED_BINS. Live: Cast discovery works. |
| AVAHI-ESCAPE: ;-in-name | Premise FALSE: `avahi-browse -p` escapes `;` as `\NNN`, column count intact, device parses fine. Effect is cosmetic display only, not device loss. |
| DETECT-IP: 8.8.8.8 fallback dropped | Failure is NOT silent (logs "LAN IP unknown ... cannot cast"); UDP fallback only matters in containers this appliance isn't. |
| CAST-CT-DUP | "audio/aac" duplicated | Consistent today; pure DRY nit, Chromecast tolerates content-type loosely. |
| CAST-CONSTS | hardcoded cast constants | CASTV2 protocol constants / universal app id, fixed by Google; port falls back to avahi-advertised. Not tunables. |
| install.sh --reset no pipewire.socket | Refuted: starts WP services explicitly then polls `pactl info` 10×1s before proceeding; CoD watcher self-corrects. |
| soundsync.service orders after PW start | After=/Wants= service not socket — but Restart=always converges; live stable. Tuning nit. |
| no StartLimit* on units | Deliberate: unattended appliance WANTS infinite restart, not a terminal failed state. |
| cod watcher busy-restart/firehose | EOF vs timeout distinguished (`cod-watch.sh:53-56`); deps are hard Depends; assert_cod only writes on drift. Efficiency nit. |
| _setup_node reads root $HOME | Convenience fallback only; SOUNDSYNC_NODE_BIN is primary, fails loudly if node absent. |
| apply-wireplumber-config only warns | INTENTIONAL: a maintainer script must not abort the dpkg transaction; doctor catches a missing config. |
| NF-33 | rules relies on root ~/.local/node | Misread: SOUNDSYNC_NODE_BIN→~/.local/node(gated)→PATH; audit's fix would BREAK this host's documented build. |
| validated only on one x86 box | Process/meta note; WP-0.4/0.5 split + adapter de-hardcode handled in code. Residual covered by RT-limits + .deb LIVE-CHECKs. |
| config_store dormant but data-lossy | Zero consumers (dead code); `write_atomic` already protects torn writes. Can't bite. |
| WpVersion::parse first X.Y token | Real `wireplumber --version` emits version as first token; no realistic trigger. Feeds NF-18 only. |
| Identify shells sh -c off-seam | Non-blocking, kill_on_drop reaps, best-effort; works live. Testability/style nit (duplicate). |
| state.rs 17 unwraps | All `cfg(test)`; tokio::RwLock doesn't poison; production path unwrap-free. Non-finding. |
| NF-11 | ontrack uses evt.streams[0] | Premise FALSE: track built with stream id (`webrtc_rs.rs:148-150`); browser gets a real stream. Live: Safari works. |
| NF-12 | <audio> not primed in click gesture | Empirically false: human plays in Safari; autoplay+playsinline + user-gesture session accepted. |
| AUD-073 (inline CRITICAL) | empty-name FE merge | Architectural mismatch: rebuild sends full snapshot with resolved names; no name-less audio-active message exists. |
| NF-35 | null-mid candidates + snake_case | Both FALSE: serde renames to sdpMid/sdpMLineIndex (`ws.rs:21-26`); webrtc-rs only fires on real candidates. Safari works. |
| MEDIUM: hardcoded STUN (webui) | LAN host/srflx candidates suffice; Listen works live. Configurability is an enhancement. |
| MEDIUM: malformed WS frame crashes render | Premise FALSE: `bluetooth_devices` is non-Option, always serialized; device_name omission doesn't remove it. |
| MEDIUM: jsdom ^29.1.1 suspicious | FALSE: 29.1.1 is real, installed, lockfile-pinned; `npm ci` resolves it. Stale knowledge. |
| LOW: spectrum rAF free-runs | Browsers throttle rAF on hidden tabs; cleaned up on unmount. Negligible. |

---

## D. Summary

**Counts by verdict:** ~75 findings reviewed → **20 KEEP**, **6 LIVE-CHECK**, **~49 DISMISS**.

**KEEP by severity:** Critical 0 · High 2 (NF-9, NF-10) · Medium 8 (NF-6, NF-15, NF-7, NF-37*, NF-1, NF-2, NF-8, NF-13, NF-31) · Low 11+ (per-device watchers, pre-bind DbusHealth, NF-26, offer-inline, ffmpeg-200, NF-16/17/30, shutdown clamp, doctor-hci0, REQUIRED_BINS, NF-34/36, album-art-disclosure, CAST-LOG, uninstall-bundle, AlbumArt key, ws-disconnect). *NF-37 keeps only the stream-cap half.

**LIVE-CHECK by severity:** Medium 1 (RT limits, Pi-only) · Low 5 (NF-4, NF-24, NF-18, NF-32, EQ slider).

**Top themes that were REAL:**
1. **Connection-liveness gaps** — the strongest cluster: half-open WebRTC/WS clients (NF-7), dead Chromecast sessions (NF-8/NF-26/CAST-LOG) and no WS keepalive all leave "active/playing" published while silent, curable by a shared liveness mechanism.
2. **.deb packaging parity** — the `.deb` path silently lacks the CoD watcher (NF-9), linger (NF-10), and a user-WP restart (NF-31) that `install.sh` has; only latent because no `.deb` is deployed yet.
3. **Supervision / panic-recovery** — bare spawns + panic≠Err mean a panicking subsystem dies invisibly while systemd stays satisfied (NF-2/NF-30), and a wedged child can block reconcile (NF-1).
4. **Blocking-syscall & node-verification edges** in the audio graph (NF-1, NF-6).
5. **Frontend type-drift** — the rename feature works server-side but never reaches the UI (NF-13), and the web tests that should have caught it test a re-implemented copy (NF-36).

**Top FALSE-POSITIVES (the audit's worst misreads):**
- Every "**silent playback / Safari blocked**" claim (NF-11, NF-12, NF-35) — refuted by the running Safari build and by code the audit misattributed to the legacy tree.
- "**Legacy nuance absent = regression**" applied blindly to deliberate architecture changes (NF-14 pw-link, NF-22 active-inquiry, NF-25 AirPlay loopback, NF-27 mdns) — several of the audit's "fixes" would have *broken* working features (notably NF-14, which would disable EQ).
- "**Unbounded growth**" claims that the code already bounds (BT device map, capture backoff).
- Stale tooling knowledge (jsdom 29 "not real," `;`-in-avahi-name "shifts columns") — both factually wrong against the lockfile and against `avahi-browse -p` escaping.
- Unreachable panics gated behind compile-time constants (map_to_bands, NaN-EMA, assert-empty-bands) dressed up as live risks.
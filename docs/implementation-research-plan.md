# SoundSync (`sc-next`) — Consolidated Implementation & Research Plan

**Date:** 2026-06-13
**Author:** Integration architect (research phase — PLAN ONLY, no code)
**Target tree:** `/home/mark/sc/sc-next` (Rust `soundsync` crate + Preact `webui`)
**Scope:** Synthesise 7 per-workstream plans + their adversarial risk reviews into one
build order, with cross-cutting sequencing, installer/packaging rollup, a single interface
version, the 3 color schemes, a deduped "what may break" ledger, and the open questions the
human must decide before any code is written.

> **Grounding note (read first).** Every per-workstream plan cites `sc-next` file:line.
> I re-read the **actual `sc-next` source** to verify the load-bearing claims. The plans are
> internally consistent with `sc-next`'s layout (`src/daemon.rs`, `src/output/controller.rs`,
> `src/config.rs`, `src/web/output.rs`, `webui/src/theme/themes.ts`). **The legacy `./sc`
> reference is a *different, older* architecture (versions 2.9.11, `src/state/config.rs`, no
> `src/daemon.rs`)** and is NOT what these plans touch — do not cross-reference `./sc` line
> numbers. Confirmed facts that change the plans appear inline below, tagged **[VERIFIED]**.

---

## 1. Executive summary

Seven workstreams are proposed for `sc-next`. They split cleanly into **four low-risk,
independent items** (graceful-shutdown hang fix, ThreadSleeper async fix, 3 color schemes,
interface version bump) and **three higher-risk audio/state items** (input-source selector,
duplicate null-sink cleanup, a2dp_sink-role doctor check). The single most dangerous and
highest-value workstream is **input-source selection**, which is gated by a live hardware
spike (**IN-0**) that decides whether the entire enforcement mechanism is `pactl
set-source-mute` (mechanism A, the human's recommended design) or a heavier discard-null-sink
routing scheme (mechanism C). No input-source code should land until IN-0 is answered on the
real host in **both EQ-off and EQ-on** sink topologies.

The two foundational async fixes (shutdown, ThreadSleeper) both touch the same blocking site
(`ensure_null_sink`'s retry/backoff in `audio/pw/null_sink.rs`) and the same `run_daemon`
signature, so **they must be done together, first**, before any workstream that adds a
periodic daemon task (null-sink sweep, a2dp re-assert backstop). Doing them first also
de-risks every later daemon change because it establishes the cancel-safe pattern.

Three of the plans (null-sink-cleanup, a2dp-doctor, version-bump) contain **factually wrong
premises** about `sc-next` that I corrected against the source (no persisted config struct;
wrong `apply_config` signature; the BT-manager states watch is not surfaced to `main.rs`).
Those corrections are folded into the per-workstream sections and materially change scope.

**Recommendation:** proceed with the four low-risk items in parallel immediately; run IN-0
and the null-sink "real duplicate state" spike on the host before committing to input-source
or sweep code; treat config persistence (which does **not** exist yet) as a shared
prerequisite that several workstreams silently depend on.

---

## 1a. Decisions locked with the human (2026-06-13)

These resolve the gating open questions; the rest of the doc stands as written, amended by these.

1. **Config persistence = ONE shared primitive.** Introduce `src/config_store.rs` (load/save JSON
   under `~/.config/soundsync/`, modelled on `bluetooth/name_config.rs`). All three workstreams
   (input-source `devices.json` / per-device faces, null-sink reconcile interval, a2dp re-assert
   interval) reuse it. This resolves **W0.5 / C3 / OQ#2**. → the periodic halves of W2.1/W2.2 are
   back **in scope** (not deferred for lack of persistence), subject to their own risk gates.
2. **Color schemes = FOUR new** (total 7, on top of the existing blue/orange/green): **Slate Blue,
   Royal Magenta, Forest Lime, Desert Red.** Crimson Rose is dropped in favour of **Desert Red**
   (muted terracotta) to avoid the `--color-danger` collision, and **Slate Blue** is added.
   *(Interpretation of the human's note "Add slate blue royal magenta, forest lime, desert red" —
   to be confirmed.)* Starting palettes (contrast-verified during W1.1):
   - Slate Blue (`slate-blue`): accent `#5468a8`, hover `#43528a`, dim `rgba(84,104,168,0.15)`,
     accent2 `#7c93cf`, gradient `135deg #5468a8→#7c93cf`.
   - Royal Magenta (`royal-magenta`): as §7 (verify light-mode AA on `#a855f7`).
   - Forest Lime (`forest-lime`): as §7.
   - Desert Red (`desert-red`): accent `#c0563a`, hover `#a3472e`, dim `rgba(192,86,58,0.15)`,
     accent2 `#e08a6f`, gradient `135deg #c0563a→#e08a6f` — deliberately muted/terracotta, clear
     of the bright danger red `#ef4444/#dc2626`.
   `themes.test.ts` length assertion becomes **7**; add `flex-wrap: wrap` to `.accent-swatches`.
3. **Graceful shutdown = FULL drain (not just the backstop).** Ship the hard-exit backstop +
   de-block + unit `TimeoutStopSec`/`KillMode` AND the graceful path: `axum ... with_graceful_shutdown`,
   `ws_connection` observes shutdown, and a **new `WebRtcController` enumerate/`stop_all`** method
   (+ Fake). Resolves **OQ#5** (in). Touches `web/server.rs`, `web/webrtc/command.rs`.
4. **Interface version = surfaced.** Bump `webui/package.json` → `0.2.0` AND add a vite `define`
   (`__APP_VERSION__`, with `resolveJsonModule`/`JSON.stringify`/`declare const` guards) + an
   "Interface" row in the Status tab (Backend vs Interface). Resolves **OQ#6** (Option B).

**Still gated on host spikes (run WITH the human, no code yet):** **IN-0** (mute efficacy, EQ-off
& EQ-on, phone round-trip) and **W0.4** (null-sink duplicate naming + survivor-by-name). These are
decision gates, not human preferences — they run at the start of their waves.

**Remaining open questions deferred to their wave:** OQ#4 shutdown timeout values (proposed 8s app
/ 15s unit), OQ#8 per-device "name" scope + `POST /api/input/none` in v1?, OQ#9 keep/trim the
`--doctor` bash checks, OQ#10 auto-switch debounce + `active_input` persistence.

---

## 2. Recommended BUILD ORDER (with dependencies)

### Wave 0 — Foundations & spikes (do first; gate everything else)
These unblock or de-risk later waves. The async fixes are sequencing prerequisites; the
spikes are decision gates that produce no code.

| # | Item | Why first | Blocks |
|---|------|-----------|--------|
| **W0.1** | **ThreadSleeper async fix** (`ensure_null_sink` backoff → `tokio::time::sleep`) | Cheapest real change; establishes cancel-safe `bring_up`; removes the `Sleeper` borrow that the shutdown fix also fights. | Shutdown fix, null-sink sweep, a2dp backstop |
| **W0.2** | **Graceful-shutdown hang fix** (main.rs hard-exit backstop + de-block `bring_up` + unit `TimeoutStopSec`) | Same blocking site as W0.1; shares `run_daemon` signature churn — do back-to-back to touch `daemon.rs`/`main.rs` once. | Any new daemon periodic task |
| **W0.3 (SPIKE)** | **IN-0 source-mute efficacy** on the host, EQ-off **and** EQ-on | The linchpin for input-source. Decides mechanism A vs C. No code. | Input-source workstream |
| **W0.4 (SPIKE)** | **Null-sink "real duplicate state"** on the host (`pactl load-module` twice; observe naming + which module owns the bare `soundsync-capture` node + where live links attach) | Survivor selection is unsound without this. No code. | Null-sink sweep |
| **W0.5 (DECISION)** | **Config-persistence foundation** — does `sc-next` get a structured config file, or stay env-only? **[VERIFIED: `config.rs` is env-resolvers only — `resolve_bind/web_dir/name`, no struct, no file.]** | Three workstreams (input devices.json, null-sink reconcile flag, a2dp re-assert interval) assume persistence that does not exist. Decide the pattern once. | Input-source, null-sink periodic, a2dp backstop |

### Wave 1 — Independent, low-risk, parallelisable (no shared audio state)
Safe to start immediately and in parallel; minimal cross-file contention.

- **W1.1 — 3 color schemes** (webui only; data-driven). Depends on nothing.
- **W1.2 — Interface version bump** (webui `package.json`, optional surfacing). Should land
  **with** the color-scheme `themes.test.ts` change to avoid a stale length assertion.

### Wave 2 — Daemon audio reliability (after Wave 0 async fixes)
- **W2.1 — Duplicate null-sink cleanup (startup sweep only first).** The unconditional
  startup sweep is the core fix and is the small, safe slice. The **optional periodic
  reconcile** is deferred behind W0.5 (config) and re-evaluated after W0.4.
- **W2.2 — a2dp_sink-role doctor check (one-shot doctor only first).** Strengthen the
  `wp-config` check from exists→content + add the live `0x110b` probe. The **optional
  daemon re-assert backstop** is deferred behind W0.5.

### Wave 3 — Input-source selector + per-device face (after IN-0)
- **W3 — gated by W0.3 (IN-0).** Slices IN-1…IN-5 as in the per-workstream section. This is
  the long pole and the largest cross-cutting churn (state.rs, ws.rs, media.rs, server.rs,
  main.rs, webui store/types/tabs).

**Independence summary:** W1.1, W1.2, W0.1, W0.2 are mutually independent and safe to do
first. W2.1/W2.2 depend on W0.1/W0.2 (daemon async correctness) and W0.5 (for their *optional*
periodic halves). W3 depends on W0.3 and shares the most files with everything else, so it
goes **last** to absorb churn rather than create it.

---

## 3. Per-workstream detail

### W0.1 — ThreadSleeper async fix  (effort: **S**)
**Approach.** The only executor-blocking sleep on an async path is the backoff in
`ensure_null_sink` (`src/audio/pw/null_sink.rs`), reached via `bring_up` from `run_daemon`,
`reconcile_loop`, and the `ReassertRouting` arm. **[VERIFIED: `bring_up<R,S:Sleeper>` at
`daemon.rs:74`; `run_daemon<R,Sp,Sl>` at `daemon.rs:88` with `Sl: Sleeper` at :100;
`reconcile_loop<R,Sl>` at :235.]** Replace the synchronous `sleeper.sleep(...)` with
`tokio::time::sleep(...).await`; make `ensure_null_sink`/`bring_up` async; drop the `Sleeper`
generic from this hot path.
**Decomposition:** (a) async `ensure_null_sink`, drop `Sleeper` param; (b) async `bring_up`;
(c) thread `.await` through the 3 call sites; (d) drop `Sl` generic from `run_daemon`/
`reconcile_loop`; (e) update `main.rs` call site; (f) migrate the 4 `null_sink` tests + the
`bring_up`/`run_daemon` tests to `#[tokio::test(start_paused)]`.
**Files:** `audio/pw/null_sink.rs`, `daemon.rs`, `main.rs`, (decide) `sys/clock.rs`,
`sys/testing.rs`. **New:** none. **UI:** none. **Config:** none.
**Decision folded in (from risk review):** do **NOT** take the `spawn_blocking` route — the
fakes are `RefCell`-based and `!Send`, so wrapping `pactl` calls forces a Mutex rewrite of
shared test infra. Async-sleep avoids that entirely. **Open caveat:** verify the `&R`-held-
across-await inside `tokio::join!` compiles **without** forcing `R: Sync` (compile spike).

### W0.2 — Graceful-shutdown hang fix  (effort: **M**)
**Approach.** Root cause: `run_daemon` awaits `tokio::join!(reconcile_loop, daemon_cmd)`
**[VERIFIED: `tokio::join!` at `daemon.rs:202`]**, both of which call synchronous `bring_up`
→ blocking `pactl`/`thread::sleep` with no shutdown check, so SIGTERM can't return.
**Decomposition (ranked):** (1) **main.rs hard backstop** — wrap `run_daemon` in
`tokio::time::timeout(~8s)`; on elapse `eprintln!` + `std::process::exit(0)`. This alone kills
every hang. (2) De-block `bring_up` under shutdown (largely **subsumed by W0.1** once backoff
is async — re-scope to "select bring_up against shutdown"). (3) Graceful web-server shutdown
via `axum::serve(...).with_graceful_shutdown(...)` — **only if** `ws_connection` is also made
to observe `shutdown` (else graceful drain hangs on an idle WS client). (4) Shutdown-aware
`webrtc_command_loop`. (5) Unit `TimeoutStopSec=15` + `KillMode=mixed`.
**Files:** `main.rs`, `daemon.rs`, `web/server.rs`, `web/webrtc/command.rs`,
`packaging/systemd/soundsync.service`. **New:** none. **UI:** none. **Config:** none (timeout
is a constant; optional `SOUNDSYNC_SHUTDOWN_TIMEOUT` env to match existing `SOUNDSYNC_*`).
**Corrections folded in:** `exit(0)` skips Drop-based cleanup (BlueZ agent unregister, discovery
teardown, `kill_on_drop` reaping) — tune the 8s/15s ordering so the **clean** path wins in
practice; `TimeoutStopSec` must be strictly **> the app backstop** or systemd SIGKILLs first.
The "iterate active webrtc sessions and close" step needs a **new trait method** on
`WebRtcController` (no enumerate/stop_all today) + Fake update — bigger than "add a `select!`".
**Recommendation:** ship items (1)+(2)+(5) as the core; treat (3)+(4) graceful-drain as
optional polish (the backstop makes them non-load-bearing for the hang).

### W1.1 — 3 color schemes  (effort: **S**)
**Approach.** Fully data-driven from `ACCENT_PRESETS` (`webui/src/theme/themes.ts`). Append 3
preset objects (each with all 5 vars), extend the `AccentPreset` union **and** the `readAccent`
validator. **[VERIFIED: union at `ThemeContext.tsx:5`; `readAccent` hardcodes the 3 ids at
`:13`; `themes.test.ts:38` title "has 3 presets" + `:39` `toHaveLength(3)`.]**
**Files:** `webui/src/theme/themes.ts`, `webui/src/theme/ThemeContext.tsx`,
`webui/src/theme/themes.test.ts`. **New:** none.
**CSS correction folded in (risk review confirmed):** **[VERIFIED: `.accent-swatches`
(`Status.css:103`) is `display:flex; gap:8px` with NO `flex-wrap`; the `flex-wrap:wrap` at
`:208` is on `.admin-actions`, a different element.]** Six 28px swatches in a `space-between`
`.theme-row` **will overflow on narrow/phone viewports** — **add `flex-wrap: wrap` to
`.accent-swatches`.** So there *is* a one-line CSS change, contradicting the plan's "no CSS
change required." **Test additions (make mandatory, not optional):** bump `toHaveLength` to 6
**and** the "has 3 presets" title; add a no-duplicate-ids assertion and assert
`--color-accent-dim`/`--color-accent2` presence (the generic loop only checks 3 vars, but
`applyTheme` hard-depends on `--color-accent-dim` to build `--gradient-bg` — a missing dim var
silently breaks the page gradient with no failing test).

### W1.2 — Interface version bump  (effort: **S**, **M** if surfaced)
**[VERIFIED in `sc-next`: `Cargo.toml` version `0.0.1`; `webui/package.json` version `0.1.0`;
`state.rs:9` `VERSION = env!("CARGO_PKG_VERSION")` — the Status tab shows the **backend**
version, not the webui one. They are already decoupled.]**
**Recommendation:** bump `webui/package.json` **0.1.0 → 0.2.0** (semver MINOR — additive UI
features). See §6 for the single-number justification. The webui version is **never injected
into the bundle today** (no vite `define`), so a manifest-only bump is invisible to users.
**Files:** `webui/package.json` (always); optionally `webui/vite.config.ts` + `Status.tsx` to
surface it; `Cargo.toml` + `debian/changelog` + `packaging/README.md` **only if** the
input-source selector adds a backend command (it does — see W3 — so a backend bump is
warranted when W3 lands).
**Corrections folded in:** Option B (vite `define`) has a real footgun — needs
`resolveJsonModule`, JSON.stringify'd value, and a `declare const`, or `tsc`/`vite build`
break; and importing `ACCENT_PRESETS` back into `ThemeContext.tsx` for a `readAccent` refactor
would create a **circular import** (`themes.ts` imports types from `ThemeContext.tsx`). Do
**not** edit the `ws.test.ts` `version: '0.1.0'` mocks — they are shape-only.

### W2.1 — Duplicate null-sink cleanup  (effort: **M**; startup slice **S**)
**Approach.** Add `parse_all_null_sink_module_ids` (the existing parser returns only the first
match), a `sweep_duplicate_null_sinks` reusing the existing `unload_module`, and call it once
on startup after `bring_up`. Optional periodic reconcile deferred.
**Files:** `audio/pw/parse.rs`, `audio/pw/null_sink.rs`, `audio/pw/routing.rs` (reuse
`unload_module`), `daemon.rs`; **periodic half also:** `config.rs` (see W0.5), `state.rs`,
`web/admin.rs`, `web/server.rs`, `webui/src/lib/api.ts`, `webui/src/tabs/Status.tsx`,
`webui/src/lib/types.ts`. **New:** none required.
**MAJOR correction folded in (risk review — plan-invalidating):** PipeWire/pulse gives only
**one** sink the literal node name `soundsync-capture`; duplicates load as
`soundsync-capture.2`, `.3`. **Every consumer binds BY NAME** (loopback `source=…monitor`, EQ
`node.target`, capture `NullSinkMonitor`, BlueZ via `set-default-sink`). Therefore the plan's
"keep the LOWEST module id" survivor rule is **actively dangerous** — the survivor MUST be
selected by **node name** (the module whose sink is the unsuffixed `soundsync-capture`),
resolved via `wpctl status` / `pactl list short sinks`, **not** by module id or by the
`sink_name=` argument (all duplicates carry the same argument). **This is the W0.4 spike.**
Also: folding the sweep into `bring_up` risks an **event storm** (unload → sink-remove event →
debounced reconcile → `bring_up` → sweep). **Recommendation: startup-sweep-only, NOT in
`bring_up`/reconcile**, until W0.4 proves the survivor-by-name selection and the event
behaviour.
**Config/UI:** the periodic-reconcile toggle's persistence depends on W0.5 — **the plan's
"existing config file mechanism" does not exist.**

### W2.2 — a2dp_sink-role doctor check  (effort: **M**; doctor slice **S**)
**Approach.** Strengthen `doctor.rs` `wp-config` check from exists→content (assert the
load-bearing `bluez5.roles = [ a2dp_sink … ]` line and `seat-monitoring = disabled`), and add
a new live `a2dp-sink-role` check probing the adapter's `0x110b` UUID (read-only `bluetoothctl
show` / `busctl --system`, no CAP_NET_ADMIN). Reuse `generate(fmt)` as the oracle. Re-assert =
the existing config-write+restart path.
**Files:** `doctor.rs`, `wireplumber/config.rs` (expose a const for the role substring),
`packaging/install.sh` (optional trim of redundant bash checks); **backstop also:** `config.rs`
(W0.5), `daemon.rs`, `web/server.rs`. **New:** optional `wireplumber/role_check.rs`.
**Corrections folded in:**
- **[VERIFIED]** The re-assert helper the plan names is wrong: it's `apply_config(format, fs,
  runner)` at `wireplumber/apply.rs:33` (ConfigFormat is the **first positional arg**), and
  the `(&fs,&runner)` wrapper is `apply_wireplumber_config` in **`install.rs:19`**, not
  `apply.rs`. Code against the cited signature won't compile.
- **[VERIFIED]** Format-awareness is mandatory: `generate(Lua)` emits a narrow
  `[ a2dp_sink ]` string and `seat-monitoring` exists **only** in the SPA-JSON body. The
  content check must branch on the **same** detected `ConfigFormat` used to pick the path, or
  it false-Fails correct files when `wireplumber --version` isn't readable (falls back to Lua).
- The `0x110b` check must be **Warn, not Fail**, when the bus/adapter is unreachable (mirror
  `bt-adapter`'s Warn). And `0x110b` absence has **CoD/bluetoothd root causes the WP re-assert
  cannot fix** — so a backstop must trigger on **file-content drift**, never on the `0x110b`
  signal (else restart-loop). Tightening the check also **breaks shared doctor test fixtures**
  (`all_ok_fs`'s literal `# config` body) — update them in lockstep.
**Config/UI:** the optional periodic backstop + status badge depend on W0.5 and add a new
snapshot field + SystemEvent + webui plumbing (more than "add a boolean").

### W3 — Input-source selector + per-device face  (effort: **L**; gated by IN-0)
**Approach.** Mirror the proven `OutputController` pattern: a new `InputController` task with
an mpsc command channel that keeps exactly one connected A2DP source audible (mechanism A:
`pactl set-source-mute` on the held `bluez_input.<MAC>` source nodes; mechanism C: discard-
sink routing — chosen by IN-0), with sticky/auto-switch policy, AVRCP metadata coupling to the
active source, and per-device "faces" persisted in `devices.json`.
**Slices:** IN-0 (spike, W0.3) → IN-1 (state model + `devices.json` store + node-id resolver +
`GET /api/inputs` + controller stub) → IN-2 (real enforcement) → IN-3 (auto-switch + debounce)
→ IN-4 (metadata coupling + appearance API) → IN-5 (webui: source pill+sheet, placeholder art,
appearance picker, Status toggle).
**Files (high-churn — see §4):** `state.rs`, `bluetooth/media.rs` (`select_player` @110,
`run_media_manager` @405), `bluetooth/manager.rs`/`service.rs` (states watch), `web/server.rs`,
`web/ws.rs`, `main.rs`, `webui` `types.ts`/`store.tsx`/`api.ts`/`NowPlaying.tsx`/`Devices.tsx`/
`Status.tsx`, `doctor.rs`, `lib.rs`. **New:** `src/input/{mod,controller,node_resolve,
devices_config}.rs`, `src/web/input.rs`, `webui SourcePicker.tsx`+css, `PlaceholderArt.tsx`.
**Corrections folded in:**
- **[VERIFIED — the states-watch seam does NOT exist as claimed]** `snap_tx/snap_rx` is
  created at `service.rs:91` and **fully consumed by `run_device_bridge` inside the same
  `tokio::join!` at `service.rs:96-97`**; it is never surfaced to `main.rs`. Giving the
  `InputController` the audio-active set requires **surgery to `run_bluetooth`'s signature/
  join! topology** (a second receiver or a fan-out) — not the "main.rs wiring mirror" the plan
  implies. Otherwise the controller needs its **own** `pactl` prober, which races the
  manager's existing 2s prober (**[VERIFIED: `PROBE_INTERVAL = 2s` at `service.rs:36`]**).
- **[VERIFIED — route collision]** `/api/bluetooth/:addr/appearance` would be swallowed by the
  existing wildcard `"/api/bluetooth/:addr/:action"` (`server.rs:81`) and route into
  `device_command`. Use a disambiguated path or register order carefully; all `/api/*` routes
  must precede `.fallback_service(spa)` (`server.rs:97`).
- **AVRCP coupling is validated, fragile code** (`select_player` @110, 6 tests @693-746). The
  `active_input` tier must be **additive** and only *influence* selection (e.g. tie-break /
  apply when the current binding lacks metadata) — an unconditional top tier risks AVRCP
  reconnect storms (`set_media(None)` → Now Playing blanks on every switch) and re-introduces
  the AUD-041 flap. The two authorities (audibility vs AVRCP binding) can disagree → permanent
  "live · no track info."
- Held-but-muted sources keep decoding A2DP → real **BT bandwidth/CPU contention** that can
  stutter the active stream. Verify two simultaneous A2DP links on the target adapter.
- Devices.tsx localStorage→`devices.json` migration must preserve the existing name-regex
  defaults or users' turntable/cassette icons regress to generic on first load.

---

## 4. Cross-cutting concerns (shared files & sequencing)

These files are edited by multiple workstreams; uncoordinated edits = merge churn and compile
breaks. **Sequencing principle:** land the small independent items (W1.x, W0.1/W0.2) first so
they touch shared files once; do W3 last to absorb the remaining churn.

- **`src/state.rs` — `AppState` / `AppStateSnapshot` / `Default` / `snapshot()`.** Touched by
  W2.1 (reconcile fields), W2.2 (a2dp_sink_ok), W3 (`InputState`). A new **non-Optional**
  snapshot field forces edits to every by-field construction site **and tests** (and the
  webui `types.ts` mirror, or `tsc` fails). **Rule:** any new snapshot field must be `Option`
  or `#[serde(default)]` and added by **one** owner per wave; W3 (largest) sequences last.
- **`src/daemon.rs` — `DaemonCommand` enum + exhaustive match + `run_daemon` signature.**
  **[VERIFIED: `DaemonCommand` has only `ReassertRouting` + `Identify`; no wildcard arm.]**
  W2.1's `SetReconcile` and W0.1/W0.2's signature changes collide here. **Rule:** do W0.1+W0.2
  as one back-to-back daemon edit, THEN W2.1 adds its enum variant on top.
- **`src/config.rs` — env-resolvers only (NO struct/file). [VERIFIED]** W3 (`devices.json` +
  auto-switch), W2.1 (reconcile flag), W2.2 (re-assert interval) all assume persistence that
  doesn't exist. **Resolve W0.5 first** and let one workstream introduce the persistence
  primitive (mirroring `bluetooth/name_config.rs`, the only existing persisted-state pattern),
  which the others reuse. Do **not** mix file I/O into `config.rs` (it's a pure-resolver
  contract).
- **`src/web/server.rs` — `WebContext` + router.** W2.1 (`/api/admin/reconcile`), W3
  (`/api/inputs`, `/api/input`, settings, appearance), W2.2 (status field). All `/api/*`
  routes before `fallback_service`; watch the `:addr/:action` wildcard collision (W3).
- **`src/web/ws.rs` — `WsOutMessage` + `event_to_ws_message`.** W3 adds `InputState`; W2.x may
  add status. The match maps each `SystemEvent`; adding a `SystemEvent` variant forces an arm
  here.
- **`src/doctor.rs` — shared test fixtures (`all_ok_fs`, `all_ok_runner`).** W2.2 changes the
  `wp-config` check semantics, forcing fixture edits that any other doctor-touching change
  depends on. Single owner.
- **webui `store.tsx` / `types.ts` / `Status.tsx` / `NowPlaying.tsx` / `Devices.tsx`.** W1.1
  (themes → Status), W1.2 (version row → Status), W2.x (admin/badge → Status), W3 (input slice
  + tabs). `Status.tsx` is the hottest file. **Rule:** assign `Status.tsx` a single integrator
  per wave; append rows rather than restructuring.

---

## 5. Installer & packaging impact rollup

| Workstream | install.sh | debian/ | systemd units | apt deps | root/CoD |
|---|---|---|---|---|---|
| W0.1 ThreadSleeper | none | none | none | none | none |
| W0.2 Shutdown | **must add `systemctl --user daemon-reload` + restart for the live instance** (see below) | rebuild only (same unit shipped) | **edit `packaging/systemd/soundsync.service`: `TimeoutStopSec=15`, `KillMode=mixed`** | none | none |
| W1.1 Color schemes | none | none | none | none | none |
| W1.2 Version bump | none (webui-only) | **stanza only if Cargo bumps**; `packaging/README.md` deb filename | tracks `debian/changelog`, not Cargo | none | none |
| W2.1 Null-sink sweep | none (startup sweep); periodic via user config file | none | none | none | none (user-level pactl) |
| W2.2 a2dp doctor | optional trim of redundant bash role/0x110b checks | none | none | none (`bluetoothctl`/`busctl` already deps) | none (user-session re-assert) |
| W3 Input-source | none (devices.json under `~/.config/soundsync`, lazy-created) | `postrm` already preserves the dir | none (USER daemon work) | none (`pactl`/`wpctl` already used) | none (no CAP_NET_ADMIN) |

**[VERIFIED — shutdown installer gap is real]** `install.sh` default path runs system-level
`systemctl daemon-reload` (:487) + `systemctl --global enable` (:490) but **does NOT** run
`systemctl --user daemon-reload`/restart for a *running* user instance — only the `--reset`
path does (:265-266). So **W0.2's `TimeoutStopSec`/`KillMode` edits silently no-op until reboot
or manual reload.** Plan W0.2 to add a `run_user systemctl --user daemon-reload && restart`
step to the default install flow (root reaches the user manager via `run_user`/`XDG_RUNTIME_DIR`
as the script already does elsewhere).

**No new units, no new root services, no new apt deps, no linger changes across all seven.**
The CoD watcher (root) is explicitly NOT extended — W2.2's WP role re-assert is correctly a
user-session operation, not a root watcher.

---

## 6. Interface version recommendation

**Bump `webui/package.json` 0.1.0 → 0.2.0 (semver MINOR). Single number for this round.**

Justification: this round ships **additive, backward-compatible** UI features (input-source
selector, per-device face, 3 color schemes) with **no breaking change to the WS/API contract**
(`webui/src/lib/types.ts` snapshot shape is extended only with `Option`/defaulted fields). That
is precisely MINOR. It is not 1.0.0 — the product is pre-1.0 (both manifests are 0.x) with no
stability commitment. Keep backend (`Cargo.toml`, drives the Status tab + CLI `version`) and
interface (webui) versions **independent** — they already are, by design. **Caveat to flag to
the human:** the bump is invisible unless surfaced (no vite `define` today). Recommended:
either accept Option A (manifest-only, lowest risk) **or** add the vite `define` + a Status
"Interface" row, but only with the `resolveJsonModule`/`JSON.stringify`/`declare const`
guardrails. **If W3 lands a backend command, also bump `Cargo.toml` 0.0.1 → 0.0.2 + a
`debian/changelog` stanza** so the backend change is reflected.

---

## 7. Proposed COLOR SCHEMES (names + palettes)

Each adds only the 5 accent vars; the shared dark/light neutral palettes apply automatically.

1. **Royal Magenta** (`royal-magenta`) — violet→pink.
   `--color-accent #a855f7`, `--color-accent-hover #9333ea`,
   `--color-accent-dim rgba(168,85,247,0.15)`, `--color-accent2 #ec4899`,
   `--gradient-accent linear-gradient(135deg,#a855f7 0%,#ec4899 100%)`.
   ⚠ Lightest accent — **verify ≥4.5:1 on light bg `#f1f2f6`** wherever text renders in
   `--color-accent` (not just hover); `#a855f7` on light is ~2.9:1 and likely fails AA for
   body text. Spot-check before locking.

2. **Forest Lime** (`forest-lime`) — green→lime (distinct from `minimal-teal`).
   `--color-accent #16a34a`, `--color-accent-hover #15803d`,
   `--color-accent-dim rgba(22,163,74,0.15)`, `--color-accent2 #84cc16`,
   `--gradient-accent linear-gradient(135deg,#16a34a 0%,#84cc16 100%)`.
   `#16a34a` chosen over brighter `#22c55e` to stay ≥4.5:1 on light.

3. **Crimson Rose** (`crimson-rose`) — warm red/rose.
   `--color-accent #e11d48`, `--color-accent-hover #be123c`,
   `--color-accent-dim rgba(225,29,72,0.15)`, `--color-accent2 #fb7185`,
   `--gradient-accent linear-gradient(135deg,#e11d48 0%,#fb7185 100%)`.
   ⚠ **Danger-collision risk:** near `--color-danger` (`#ef4444/#dc2626`); with crimson-rose
   active, `.theme-seg__btn--active` becomes red-on-white, visually mimicking a destructive
   control. **Open question for the human:** accept, or swap the third scheme for a cooler/
   neutral hue (slate-blue or copper) to maximise separation from semantic tokens.

`indigo-cyan` must remain index 0 (asserted by `themes.test.ts`).

---

## 8. WHAT THIS MAY BREAK (consolidated, deduped, ranked by severity)

### CRITICAL — wrong design premise; would ship a broken feature
- **C1. Null-sink survivor selection.** ~~Duplicates are named `soundsync-capture.2/.3`~~
  **[SPIKE W0.4 RESULT — 2026-06-13, run live on the host, plan assumption CORRECTED]:** on
  **PipeWire 1.6** a duplicate `module-null-sink sink_name=soundsync-capture` does **NOT** get a
  `.2` suffix — **both sinks carry the identical literal node name `soundsync-capture`** (only
  the sink/node *id* and the owning *module id* differ; e.g. ids 58 vs 18594, modules
  536870917 vs 536870918). The `.2` suffixing was the old pulse behaviour and does not occur on
  native PW 1.6. **Therefore survivor-by-name is ALSO ambiguous** (both are unsuffixed) and the
  sweep MUST disambiguate by **module**: the daemon tracks the module id it loaded and unloads
  every *other* `module-null-sink` whose args contain `sink_name=soundsync-capture`.
  - **Startup sweep (no live capture yet):** simplest safe rule = unload ALL existing
    `soundsync-capture` null-sink modules, then let `ensure_null_sink` create the one canonical
    sink. No live links to disrupt at startup.
  - **Periodic reconcile (capture live):** must keep the module whose monitor currently feeds
    the live links (resolve via `pw-link -l`), never pick by name. Defer until proven.
  This refines W2.1's `parse_all_null_sink_module_ids`/`sweep_duplicate_null_sinks` design.
- **C2. IN-0 mute mechanism may be dead on arrival.** `set-source-mute` mutes the source/its
  monitor, **not necessarily** the source→default-sink playback link that feeds the capture
  mix; topology differs **EQ-off vs EQ-on**. With `bluez5.enable-hw-volume=true`, source mute
  may **round-trip over AVRCP to the user's phone** and/or be reverted by WirePlumber policy.
  Must test both EQ states + phone-side effect. **Gate: W0.3.** Fallback = mechanism C.
- **C3. Config persistence assumed but absent.** `config.rs` is env-only. Three workstreams'
  persistence claims have nothing to write into; coding them as planned silently invents a new
  subsystem (or fails). **Gate: W0.5.**

### HIGH — real regressions / non-trivial scope blowups
- **H1. `run_daemon`/fakes Send+Sync wall.** If any later step needs `spawn_blocking` for
  `bring_up`/`pactl`, the `RefCell`-based `!Send` fakes (`sys/testing.rs`) force a Mutex
  rewrite of **shared** test infra. Mitigation: async-sleep (W0.1) instead of `spawn_blocking`.
- **H2. `exit(0)` shutdown backstop skips Drop cleanup** (BlueZ agent unregister, discovery
  teardown, `kill_on_drop` reaping) — can leave a registered agent / discovery on across a
  restart. Tune timeouts so the clean path wins; only the timeout path exits hard.
- **H3. BT-manager states-watch seam doesn't exist.** `snap_rx` is consumed inside
  `service.rs`'s `tokio::join!`; surfacing it to the `InputController` needs `run_bluetooth`
  surgery, or a second `pactl` prober racing the 2s manager prober.
- **H4. AVRCP metadata coupling can thrash / blank Now Playing.** Unconditional `active_input`
  top tier in `select_player` forces reconnect storms and re-introduces the AUD-041 flap. Keep
  it additive/tie-break only.
- **H5. Reconcile loop fights the null-sink sweep (event storm).** Sweep-in-`bring_up` →
  sink-remove event → debounced reconcile → `bring_up` → sweep. Mitigation: startup-only sweep
  until W0.4 says otherwise.
- **H6. a2dp re-assert restart-loop + mid-stream dropout.** WP restart = ~200ms dropout and
  itself emits graph events that re-trigger reconcile. Trigger only on **file-content drift**,
  default OFF, only when no active stream (and the daemon currently exposes **no** stream-active
  signal — needs one).
- **H7. State/snapshot/type drift breaks the gate.** New non-Optional snapshot fields break
  by-field tests + the `types.ts` mirror (`tsc`). Several workstreams touch the same struct.
- **H8. `apply_config` signature wrong in the a2dp plan** — `apply_config(format, fs, runner)`,
  wrapper in `install.rs`. Code as cited won't compile.
- **H9. Installer no-ops the unit change.** Default `install.sh` never `--user daemon-reload`s
  a live instance → `TimeoutStopSec`/`KillMode` silent until reboot.

### MEDIUM — usability / contrast / churn
- **M1. Color-swatch overflow on phones** — `.accent-swatches` lacks `flex-wrap`; 6 swatches
  overflow. Add `flex-wrap:wrap`.
- **M2. Royal-magenta light-mode contrast** ~2.9:1 where text uses `--color-accent` → AA fail.
- **M3. Crimson-rose ≈ danger color** → active segmented button reads as destructive.
- **M4. `readAccent` not widened** → persisted new accent silently reverts to indigo-cyan on
  reload (type passes; runtime regresses).
- **M5. Version bump is a cosmetic no-op** unless surfaced; stakeholders may think 0.2.0
  shipped while the UI still shows the backend version.
- **M6. Devices.tsx icon regression** on localStorage→`devices.json` migration (lost name-regex
  defaults).
- **M7. axum route collision** `/api/bluetooth/:addr/appearance` vs `:addr/:action`.
- **M8. Doctor `0x110b` false-Fail** on headless/no-adapter boxes if not Warn; doctor content
  check must be **ConfigFormat-aware** or it false-Fails correct Lua/SPA files.
- **M9. Auto-switch debounce vs 2s probe interval** — a chime shorter than one probe tick can
  be missed; latency = probe + debounce. Tune live.
- **M10. Four writers to PipeWire default-sink/link topology** (daemon reconcile, EQ route,
  output loopback, + new InputController under mechanism C) with no lock → races.

---

## 9. OPEN QUESTIONS the human must decide before coding

1. **IN-0 verdict (mechanism A vs C).** Does `pactl set-source-mute` on a `bluez_input` source
   silence its contribution to `soundsync-capture.monitor` in **both** EQ-off and EQ-on, and
   **without** round-tripping mute to the phone over AVRCP/hw-volume? **A = mute (recommended
   design); C = discard-sink routing (heavier).** *Gates all of W3.*
2. **Config persistence pattern.** Introduce a structured config file (and where —
   `~/.config/soundsync/`, mirroring `name_config.rs`), or keep env-only and drop the three
   persisted toggles from v1? *Gates the periodic halves of W2.1/W2.2 and W3's `devices.json`.*
   **Choice:** (a) one shared persistence primitive now; (b) per-file like `name_config`;
   (c) env-only, defer toggles.
3. **Null-sink: startup-only or also periodic?** Given the file is near-static and the
   event-storm risk, is the unconditional startup sweep sufficient for v1? *Recommended: yes;
   defer periodic.*
4. **Shutdown timeouts.** App backstop **8s**, `TimeoutStopSec` **15s**, `KillMode=mixed` —
   acceptable? (Must keep `TimeoutStopSec` > app backstop.) And: env-configurable
   (`SOUNDSYNC_SHUTDOWN_TIMEOUT`) or fixed constant?
5. **Graceful WS/WebRTC drain — in or out?** Ship it (needs `ws_connection` shutdown-awareness
   + a new `WebRtcController` enumerate/stop_all method), or rely solely on the de-block +
   hard-exit backstop + unit timeout (simpler; skips `server.rs`/`command.rs` changes)?
6. **Interface version surfacing.** Manifest-only 0.2.0 (Option A) or vite-`define` + Status
   "Interface" row (Option B)? And bump `Cargo.toml` 0.0.1→0.0.2 when W3's backend command
   lands?
7. **Third color scheme.** Keep crimson-rose (accept danger-color proximity) or swap for a
   cooler/neutral hue (slate-blue / copper)?
8. **Per-device appearance scope.** Is setting a per-device "name" here display-only metadata
   distinct from the BlueZ alias/`device-name` file (design says yes), and is `POST
   /api/input/none` (mute everything) in v1 or deferred?
9. **a2dp doctor: keep redundant bash checks** in `install.sh --doctor` (belt-and-suspenders)
   or trim once the Rust doctor covers role drift + `0x110b`? And: does the doctor's strict
   content check tolerate **any** admin customisation of the role line, or report it?
10. **Auto-switch debounce duration** (2s vs 3s) — pick after the IN-0/two-source live test;
    relatedly, does `active_input` persist across daemon restarts or re-derive deterministically
    (design implies session-sticky)?

---

*End of consolidated plan. This is research output only — no `sc-next` code is to be written
until the human approves the build order and resolves the gating open questions (especially
IN-0, config persistence, and the null-sink survivor spike).*

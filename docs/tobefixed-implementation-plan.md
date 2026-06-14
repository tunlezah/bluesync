# tobefixed.md — Consolidated Implementation Plan (integration architect)

**Date:** 2026-06-14
**Scope:** RESEARCH/DESIGN ONLY. This document is a precise plan to fix the CONFIRMED
issues in `tobefixed.md` section A. No file has been modified by producing it.
**Targets:** `/home/mark/sc/sc-next` (Rust `soundsync` crate + Preact `webui` + `debian/` packaging).
**Authority:** `tobefixed.md` corrections OVERRIDE the raw `/tmp/needsfixing.md` audit and the
raw cluster fix-designs wherever they conflict. Every cited line below was re-read in the live tree.

---

## 1. Executive summary

Eleven CONFIRMED findings are addressed across four code areas: **.deb packaging parity**
(NF-9, NF-10, NF-31; NF-33 mention-only), **daemon resilience** (NF-1 command timeout, NF-2
task-exit supervision, NF-6 null-sink node verification), **web/WebRTC lifecycle**
(NF-7 half-open teardown, NF-37 stream concurrency cap, NF-15 web bind-retry), and **Chromecast
recovery** (NF-8 dead-session detection, NF-26 inbound deadline). One pure-frontend type-drift fix
(NF-13 device_name) rounds it out.

The governing constraints are: (a) the daemon is a USER service with no `CAP_NET_ADMIN`; (b) the
fixed 8s-restart-loop regression must NOT be reintroduced — any new "process-fatal on task exit"
logic MUST treat a normal-shutdown exit as expected; (c) the test seams (`CommandRunner`/`Fs`/
`Spawner` fakes are `RefCell`-based and `!Send`) must be preserved — no signature may become
async-trait or `Send`-bounded; (d) maintainer scripts run under `set -e`, so every new
`systemctl`/`loginctl`/`runuser`/`debconf` call MUST end in `|| true` or sit inside a `set +e`
fence.

The riskiest item is **NF-2** (it touches `main.rs` and could reintroduce the restart loop via a
shutdown TOCTOU); it is sequenced LAST. The packaging items and NF-13 touch zero Rust runtime code
and are the safest. The plan folds in the adversarial reviewers' corrections, the most important of
which are: drop `RTCPeerConnectionState::Closed` from the NF-7 callback match (it self-echoes on
every teardown); fix NF-6's reuse-verify Err arm so a command timeout keeps-reuse instead of
aborting bring-up; drop NF-10's `disable-linger` on purge (linger is a shared per-user property);
and resolve NF-8's `CastHealth::Live` (it is unreachable with the proposed wiring).

---

## 2. Build order with dependencies

Order is chosen to put the lowest-risk, independent, worktree-isolatable items first, and to
minimise churn on the five shared files (`main.rs`, `web/server.rs`, `state.rs`, `web/ws.rs`,
`daemon.rs`). Two files are the real collision points: **`main.rs`** (NF-2, NF-7, NF-37, NF-15) and
**`web/server.rs`** (NF-7, NF-37, NF-15). `state.rs`/`web/ws.rs` are touched only by NF-8 (additive)
and `daemon.rs` is touched by no item.

| # | Item | Touches | Parallel-safe? | Rationale for position |
|---|------|---------|----------------|------------------------|
| 1 | **NF-9** (.deb CoD watcher) | debian/* | YES (worktree A, packaging-only) | Zero Rust/web code; pure file shipping + unit enable. |
| 2 | **NF-13** (webui device_name) | webui/* | YES (worktree B, webui-only) | Frontend-only; backend already complete end-to-end. |
| 3 | **NF-1** (command timeout) | `sys/command.rs`, `Cargo.toml` | YES (worktree C) | Real-impl-only; trait + fakes untouched. Must land before NF-6 (changes failure shape). |
| 4 | **NF-6** (null-sink verify) | `audio/pw/null_sink.rs` | sequential AFTER NF-1 | Depends on NF-1's `TimedOut` error semantics. Isolated file otherwise. |
| 5 | **NF-10** (.deb linger) | debian/postinst, control, postrm, changelog | sequential AFTER NF-9 (same postinst) | Introduces debconf `$SS_USER`; NF-31 depends on it. |
| 6 | **NF-31** (.deb user WP restart) | debian/postinst | sequential AFTER NF-10 (reuses `$SS_USER`) | Rides NF-10's user resolution; same postinst hunk. |
| 7 | **NF-8** (cast dead-session detect) | `cast/client.rs`, `output/controller.rs`, `state.rs`, `web/ws.rs`, webui/* | mostly isolated; coordinate `state.rs`/`ws.rs` with NF-13 webui | Self-contained subsystem; no `main.rs`. |
| 8 | **NF-26** (cast inbound deadline) | `cast/client.rs` | sequential WITH/AFTER NF-8 | Prompt return only observed by NF-8's tick. Same file. |
| 9 | **NF-7** (WebRTC half-open teardown) | `webrtc_rs.rs`, `web/server.rs`, `main.rs` | sequential (owns main.rs channel reorder) | Highest-value web item; touches the shared `main.rs`/`server.rs`. |
| 10 | **NF-37** (stream cap) | `web/stream.rs`, `web/server.rs`, `main.rs` | sequential AFTER NF-7 | Adds `WebContext.stream_limit`; must land after NF-7's channel reorder. |
| 11 | **NF-15** (web bind-retry) | `web/server.rs`, `main.rs` | sequential AFTER NF-37 | Smallest; serve() body, structurally leaves the spawn intact. |
| — | **NF-2** (task-exit supervision) | `main.rs` | **LAST, sequential** | Riskiest; touches every spawn site. Resolve `main.rs` once, after all NF-7/37/15 edits land. |
| — | NF-33 | none | n/a | Mention-only; no change. |

**Parallelism guidance:** Items 1, 2, 3 can be developed concurrently in three worktrees (packaging,
webui, `sys/command.rs`). After NF-1 merges, NF-6 proceeds. The packaging chain (NF-9→NF-10→NF-31)
is one serialised postinst edit pass. The web/main.rs chain (NF-7→NF-37→NF-15→NF-2) MUST be
serialised because all four converge on `main.rs` and the first three on `server.rs`; do them as one
coherent edit pass on those two files. NF-8/NF-26 (cast) are isolated from the main.rs chain and can
run in parallel with it, EXCEPT they share `state.rs`/`web/ws.rs`/`webui` with NF-13 — sequence
those small additive edits to avoid literal-construction merge conflicts.

---

## 3. Per-item designs

### NF-9 — .deb omits the CoD watcher (HIGH)

**Chosen approach.** Ship the two proven `install.sh` artifacts through `dh_install` and enable/
disable the unit in the maintainer scripts, matching the exact paths the unit's hardcoded
`ExecStart=/usr/lib/soundsync/soundsync-cod-watch.sh` requires (verified
`packaging/systemd/soundsync-adapter-cod.service:12`, `WantedBy=bluetooth.target:17`,
`PartOf=bluetooth.service:7`). Source files exist:
`packaging/soundsync-cod-watch.sh` (repo root of `packaging/`, NOT `packaging/systemd/`) and
`packaging/systemd/soundsync-adapter-cod.service`.

**Exact changes.**
- `debian/soundsync.install` (currently 4 lines): append
  `packaging/systemd/soundsync-adapter-cod.service   usr/lib/systemd/system/`
  and `packaging/soundsync-cod-watch.sh   usr/lib/soundsync/`.
- `debian/postinst` configure case, AFTER the existing adapter block (lines 18–25), BEFORE the
  global enable (line 28): add a CoD enable block that mirrors the adapter idiom exactly —
  `deb-systemd-helper enable soundsync-adapter-cod.service || true` (guarded by `command -v
  deb-systemd-helper`) then, guarded by `systemctl is-system-running --quiet 2>/dev/null`,
  `systemctl enable --now soundsync-adapter-cod.service || true`. The existing `daemon-reload`
  (line 16) covers the new unit.
- `debian/prerm` remove/upgrade/deconfigure/failed-upgrade case (mirror lines 11–19): add a CoD
  disable block — `if systemctl is-enabled soundsync-adapter-cod.service >/dev/null 2>&1; then
  systemctl disable --now soundsync-adapter-cod.service || true; else systemctl stop
  soundsync-adapter-cod.service 2>/dev/null || true; fi` then `deb-systemd-helper disable ... ||
  true`.
- `debian/changelog`: add an entry.
- `postrm` needs NO file-removal change: `dh_install`-tracked files are auto-removed by dpkg on
  remove/purge.
- **EXECUTABLE BIT (must verify):** the source script is currently `0664`, NOT `0755`. `dh_fixperms`
  does NOT add an execute bit to a file under `usr/lib` that lacks one. The unit's `ExecStart` will
  fail at runtime if the staged file is not executable. **Mitigation (adopt, do not leave to
  chance):** add `override_dh_fixperms:` in `debian/rules` calling `dh_fixperms` then
  `chmod 0755 debian/soundsync/usr/lib/soundsync/soundsync-cod-watch.sh`. (This corrects the
  cluster design, which assumed the source was already 0755 — it is not.)

**dh_installsystemd interaction (document, do not add a third path).** `debian/rules` uses bare
`dh $@`, so `dh_installsystemd` auto-discovers units staged under `usr/lib/systemd/system/` and
generates enable/start snippets at `#DEBHELPER#` (postinst:43). This already happens for
`soundsync-adapter.service` and the hand-written block is tolerated as idempotent duplication. The
CoD unit inherits the same tolerated duplication. Do NOT add a `debian/soundsync.soundsync-adapter-
cod.service` symlink-target or any third enable path.

**What could break + guard.**
- *set -e abort:* a bare failing `systemctl` aborts the dpkg transaction. Guard: every new call ends
  in `|| true`, copying the adapter idiom verbatim.
- *chroot build:* enabling in a build chroot. Guard: existing `is-system-running` check (postinst:23)
  + `deb-systemd-helper` (chroot-safe). Strictly safer than `install.sh:162`, which is unguarded.
- *non-executable script:* see executable-bit mitigation above.

**Test plan (packaging-only; Rust/web gates untouched).** `SOUNDSYNC_NODE_BIN=~/.local/node/bin
dpkg-buildpackage -us -uc -b`; then `dpkg-deb -c ../soundsync_*.deb | grep -E
'soundsync-adapter-cod.service|soundsync-cod-watch.sh'` must list both at the matching paths with the
script as `-rwxr-xr-x`; `dh_install --list-missing` must report neither as not-installed; `lintian`
clean.

**Live verification.** `sudo apt install ./soundsync_*.deb`; `systemctl is-enabled
soundsync-adapter-cod.service` => enabled; `is-active` => active; after a WirePlumber endpoint
re-registration confirm `hciconfig <hci> class` stays `0x240414` (AUD-002/G8 guard). `apt remove` =>
disabled+stopped; `apt purge` => both files gone.

**Effort:** small.

---

### NF-13 — device_name type drift; rename never shows in UI (MEDIUM)

**Premise CONFIRMED.** Backend is complete end-to-end: `state.rs:98,109,205,211–217`,
`ws.rs` maps `DeviceNameChanged` → `WsOutMessage::DeviceName{name}` (wire tag `device_name`, field
`name`, proven by the passing Rust test). The break is purely the TS contract dropping the field +
message and two hardcoded `'SoundSync'` literals. Frontend-only; no backend change.

**Exact changes (verbatim wire tag/field from the Rust serde test, not guessed).**
- `webui/src/lib/types.ts`: add OPTIONAL `device_name?: string;` to `AppStateSnapshot` (optional to
  match existing `eq?`/`output?` and avoid breaking the two `ws.test.ts` snapshot fixtures at tsc
  time); add union variant `| { type: 'device_name'; name: string }` to `WsServerMessage`.
- `webui/src/lib/store.tsx`: add `deviceName: string` to `AppStore`; `useState('SoundSync')`; in the
  `state_snapshot` case add a guarded `if (msg.data.device_name) setDeviceName(msg.data.device_name)`;
  add `case 'device_name': setDeviceName(msg.name); break;`; expose `deviceName` in the returned
  store object.
- `webui/src/tabs/Devices.tsx:300`: replace the dead
  `store.snapshot?.version ? 'SoundSync' : 'SoundSync'` ternary with `store.deviceName`.
- `webui/src/tabs/Status.tsx` `RenameSection`: add `const store = useStore();`, seed
  `useState(store.deviceName)`, and add `useEffect(() => setName(store.deviceName),
  [store.deviceName])` so a rename from another tab / reconnect re-seeds the field.
- `webui/src/lib/ws.test.ts`: extend the local reducer + `MockState` with `deviceName` and add a
  discrimination test + a transition test (SEE cross-cutting: order vs NF-36 — but NF-36 is NOT in
  this plan's scope, so the reducer extension is safe here).

**What could break + guard.**
- *tsc fails on existing fixtures* if `device_name` were required → make it optional (the load-bearing
  guard).
- *empty-name truthiness:* `if (msg.data.device_name)` treats `''` as absent and keeps `'SoundSync'`.
  Acceptable because `parse_name` allows only trimmed non-empty and startup `set_device_name(initial)`
  resolves a real default. Make this a conscious choice.
- *useStore outside provider:* `RenameSection` is rendered by `StatusTab`, which already calls
  `useStore()` inside the provider — no throw risk.

**Test plan.** webui: add the three TS tests; run `tsc --noEmit`, `vitest run`, `vite build` (expect
clean + new tests green). Backend unchanged → `cargo build/test/fmt` is a no-op pass.

**Live verification.** Rename in Status → input updates with no reload; a second tab reads the new
name on connect; the pairing banner reads `Discoverable as "<new name>"`; reload persists; the phone
shows the new alias. Optionally watch the WS frame `{"type":"device_name","name":"…"}` in devtools.

**Effort:** small.

---

### NF-1 — shelled-out commands have no timeout (MEDIUM)

**Chosen approach.** Add `wait-timeout` INSIDE `SystemCommandRunner::run` only (verified
`sys/command.rs:28–37` uses `std::process::Command::output()`, synchronous). This is the ONLY option
that fits the seam: `CommandRunner::run` is `fn run(&self, …) -> io::Result<CommandOutput>` —
synchronous, `&self`, not `Send`-bounded; the `FakeCommandRunner` stores `RefCell` and is `!Send`.
An async/tokio::process runner would force an async-trait and break the fake (forbidden);
`spawn_blocking`+timeout would need `Arc<R: Send+Sync>` rippling through `run_daemon`/`bring_up`/
`reconcile_loop` (all borrow `&R`) and would still leak a blocking thread on a wedged child.
`wait_timeout` kills+reaps at the syscall, returns an `io::Error` callers already treat as a command
failure, and leaves the trait + all call sites + the fake untouched.

**Exact changes.**
- `Cargo.toml`: add `wait-timeout = "0.2"` (pure-Rust, no transitive deps — verify before adding for
  the air-gapped build).
- `sys/command.rs`: add `const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);` (> the per-call
  null-sink backoff cap of 5s; < the 15s unit `TimeoutStopSec`). Rewrite `run` to `spawn()` with
  piped stdout/stderr, `child.wait_timeout(COMMAND_TIMEOUT)`: on `Some(status)` drain the pipes and
  build `CommandOutput` with `status.code().unwrap_or(-1)` (identical to today); on `None`
  `child.kill(); child.wait();` and return `Err(io::Error::new(ErrorKind::TimedOut, …))`. On the
  timeout path, kill BEFORE reading to avoid a pipe-buffer deadlock. Outputs are small
  (pactl/wpctl/which/hostname), so read-after-wait on the success path is safe.

**What could break + guard.**
- *pipe-buffer deadlock:* mitigated by small outputs + kill-before-read on timeout.
- *new dep on the air-gap surface:* `wait-timeout` is tiny/pure-Rust; verify vendorable.
- *timeout too tight for cold-boot pactl:* 10s is well above expected; per-COMMAND not per-loop, so
  the ~35s 10-attempt null-sink loop is unaffected.
- *fake unaffected:* `FakeCommandRunner` never calls real `run`; all ~457 tests keep compiling.

**Test plan.** In `sys/command.rs` add real-binary tests using `std::time::Instant` (NOT
`#[tokio::test(start_paused)]` — `wait_timeout` blocks an OS thread, invisible to virtual time):
(1) `run("sh", ["-c","exit 0"])` → status 0; (2) `run("sh", ["-c","printf hello"]).stdout == "hello"`
(pipe drain); (3) `run("sh", ["-c","sleep 30"])` → `Err` kind `TimedOut` in well under 30s, child
reaped (no zombie).

**Live verification.** `journalctl --user -u soundsync -f`; shadow `pactl` on PATH with a
`sleep 60` stub, trigger `/api/admin/restart`; confirm a `timed out` log within ~10s and the daemon
continues reconciling rather than hanging until the backstop.

**Effort:** small.

---

### NF-6 — null-sink reuse path never verifies the node (MEDIUM)

**Chosen approach.** Add the create path's `pactl list short sinks` node check (verified
`null_sink.rs:71–74`) to the reuse short-circuit (`null_sink.rs:39–44`). If the node is present →
reuse as today; if `success && !contains` → the module is stale: `let _ = unload_module(runner, id)`
(best-effort; import already at `null_sink.rs:3`) and fall through to the existing create loop
(46–95); if the verify command **fails or errors (incl. NF-1 `TimedOut`)** → cannot verify, KEEP
reuse.

**CRITICAL correction (from the risk review).** The raw cluster snippet used `.map_err(…)?` on the
reuse-verify call — that would PROPAGATE a command Err (including NF-1's new `TimedOut`) and ABORT
bring_up, contradicting the "keep reuse on cannot-verify" intent and destroying a live sink on a
transient pactl stall. The Err arm MUST return `Ok(NullSink { module_id: Some(id), reused: true })`,
NOT `?`. Concretely: `match runner.run("pactl", &["list","short","sinks"]) { Ok(s) if s.success()
&& s.stdout.contains(sink_name) => return Ok(reused), Ok(s) if s.success() => { eprintln!(…stale…);
let _ = unload_module(runner, id); /* fall through */ } _ => return Ok(reused) /* keep reuse on
error/non-success */ }`.

**What could break + guard.**
- *extra pactl call per reuse* (hot reconcile path): one bool check; bounded, already paid on create.
- *transient pactl error needlessly unloading a working sink:* only `success && !contains` triggers
  unload; all error/non-success paths keep reuse.
- *existing test `reuses_existing_null_sink`* programs only `list modules`; post-change the
  unprogrammed `short sinks` returns 127 (non-success) → keep-reuse branch → test still passes but no
  longer exercises the present-node path. Update it to program `short sinks` containing the name.

**Test plan (respect the fake's program+args-contains, first-match-wins, no call-ordering).**
(1) update `reuses_existing_null_sink` to also program `short sinks` => contains `soundsync-capture`,
asserting no `load-module`. (2) `reuse_with_missing_node_unloads_and_recreates`: `list modules` =>
id 7; `short sinks` => does NOT contain name. Because the fake returns the SAME response for the
post-create verify (identical args), assert on SIDE EFFECTS (an `unload-module(7)` call and a
`load-module` call were issued) and accept the function returns the "did not appear" `Err` in that
fake setup — do NOT assert `reused:false/Ok`. (3) `reuse_with_pactl_sinks_error_keeps_reuse`:
`short sinks` => status != 0 → assert `reused:true`, no unload/load. Run under
`#[tokio::test(start_paused = true)]` like siblings.

**Live verification.** Steady state: `pactl list short sinks | grep soundsync-capture` present.
Simulate: `systemctl --user restart wireplumber`, confirm module lingers while node is gone; trigger
a reconcile; watch for the "present but node missing; unloading stale module and recreating" log;
confirm the node returns and WebRTC Listen has sound.

**Effort:** small.

---

### NF-10 — .deb never enables linger (HIGH)

**Chosen mechanism.** debconf prompt with an auto-detected default, persisted (idempotent across
upgrades). A system postinst runs as root with NO `SUDO_USER` (unlike `install.sh:93`), so the
appliance user must come from somewhere. Of the candidates a bare `/etc/default` var is "too weak for
a headless appliance" (tobefixed.md:22 rejects the loud-note-only path); auto-detect alone mis-guesses
on 0/2+-user boxes. The synthesis: ship `debian/templates` + `debian/config` that PRE-FILL the prompt
by auto-detecting the sole human user (UID in [1000,65534), valid shell, existing home), so the
common single-user appliance is zero-touch; multi/zero-user boxes are prompted (or preseeded). postinst
reads the answer and runs `loginctl enable-linger`.

**Exact changes.**
- `debian/control`: add `debconf` to `Depends` (after line 38).
- `debian/templates` (new): `soundsync/appliance_user` (Type: string) + a `soundsync/no_user`
  (Type: note) for the none-detected case.
- `debian/config` (new, `#!/bin/sh`, sources `/usr/share/debconf/confmodule`): compute the unique
  [1000,65534) account with a valid login shell + existing home from `getent passwd`; if exactly one,
  `db_set soundsync/appliance_user <user>`; `db_input high soundsync/appliance_user || true; db_go ||
  true`.
- `debian/postinst` configure case: source confmodule at top; `db_get soundsync/appliance_user ||
  true; SS_USER="$RET"`; if non-empty and `id "$SS_USER"` resolves and != root, `loginctl
  enable-linger "$SS_USER" || true` (guarded by `command -v loginctl`) + a confirmation echo; if
  empty, print a loud, copy-pasteable remediation (`loginctl enable-linger <user>` +
  `dpkg-reconfigure soundsync`). Keep the existing `systemctl --global enable soundsync.service`
  (line 28).
- `debian/postrm` purge case: `db_purge` (source confmodule; `db_purge` MUST be the last debconf
  call). **DROP the `disable-linger` on purge** (SEE correction).
- `debian/changelog`: entry.

**Corrections folded in.**
1. **Variable naming:** use `SS_USER` / `SS_UID`, NOT `USER`/`UID`. `$USER` is an environment var
   (clobbering it is fragile); `UID` is read-only in bash (scripts are `#!/bin/sh`/dash today where
   it is fine, but a future shebang change would break it). NF-31 reuses `SS_USER`/`SS_UID`.
2. **set -e + debconf:** `db_get`/`db_input`/`db_go`/`db_purge` return NONZERO as normal control flow
   — EVERY call MUST end in `|| true` (or fence with `set +e`/`set -e`). This is the single biggest
   correctness risk in the cluster.
3. **DROP disable-linger on purge.** Linger is a SHARED per-user property not owned by soundsync;
   `install.sh`/`uninstall.sh` never disable it. Revoking it on purge could break OTHER user services
   the account relies on. This is a behaviour divergence from the proven installer and the design's
   main over-reach — drop it. (`db_purge` to clear the stored answer is fine; just do not call
   `loginctl disable-linger`.)

**What could break + guard.**
- *auto-detect picks wrong user on multi-user box:* the detect only fills the default; `db_input
  high` still prompts interactively.
- *loginctl in a chroot/container:* `command -v loginctl` + `|| true`.
- *noninteractive (`apt -y`) + 2+ users + no preseed:* `db_input high` is suppressed → linger silently
  unset. ACKNOWLEDGED residual; bounded (single-user appliance is dominant, the loud postinst message
  covers the empty case). NF-10 is PARTIALLY closed in this case — acceptable per the threat model.

**Test plan.** `lintian` clean on debconf usage. Single-user: `DEBIAN_FRONTEND=noninteractive apt
install ./soundsync_*.deb` → `loginctl show-user <user> | grep Linger=yes`. Multi-user interactive:
prompt appears with detected default; pick a user; linger set for it only. Preseed:
`echo 'soundsync soundsync/appliance_user string mark' | debconf-set-selections` before install →
no prompt, linger set. Rust/web gates unchanged.

**Live verification.** Reboot the appliance with NO interactive login; `loginctl user-status <user>`
shows the user manager + soundsync running purely from linger. `apt purge` → stored answer cleared;
linger NOT revoked (per the correction).

**Effort:** medium.

---

### NF-31 — .deb writes WP A2DP config but never restarts the user's WirePlumber (MEDIUM)

**Chosen approach.** Ride NF-10's resolved `$SS_USER`. Mirror `install.sh:479–486`: as the target
user, with `XDG_RUNTIME_DIR=/run/user/<uid>` and `DBUS_SESSION_BUS_ADDRESS`, run `systemctl --user
restart wireplumber` ONLY if `/run/user/<uid>` exists (a live session). If no live session, do
nothing — WirePlumber loads the freshly-written `/etc` config on its next start, so the gap is
self-correcting on reboot (tobefixed.md:106). Use `runuser` (util-linux, no new dep), not `sudo`.

**Exact changes.** `debian/postinst` configure case, AFTER the NF-10 linger block and after the
existing `apply-wireplumber-config` (line 8): `SS_UID=$(id -u "$SS_USER" 2>/dev/null)`; if
`$SS_USER` non-empty AND `[ -d "/run/user/$SS_UID" ]`, run
`runuser -u "$SS_USER" -- env XDG_RUNTIME_DIR="/run/user/$SS_UID"
DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$SS_UID/bus" systemctl --user restart wireplumber
2>/dev/null && echo "soundsync: restarted $SS_USER WirePlumber" || echo "soundsync: restart
$SS_USER WirePlumber (or reboot) to load the A2DP config."`. The trailing `|| echo` makes the
compound return 0, so `set -e` cannot abort. When no live session, print the same reboot hint.

**What could break + guard.**
- *restart drops audio for a live session at install time:* identical to `install.sh:480`, an
  operator-initiated maintenance action.
- *quoting of env list:* copy `install.sh:104–105` verbatim (proven live).
- *abort risk:* the `&& echo || echo` compound always returns 0; guarded.

**Test plan.** Live session present: `journalctl --user -M <user>@ -u wireplumber` shows a restart;
`soundsync doctor` confirms WP config loaded. No session: postinst prints the reboot hint and does
NOT error. Packaging-only.

**Live verification.** After `.deb` install on the live appliance with an active session, pair a
phone and play — audio routes through the A2DP sink WITHOUT a manual `systemctl --user restart
wireplumber`, proving G4 is closed on the .deb path.

**Effort:** small.

---

### NF-8 — dropped Chromecast never recovers; UI shows active while silent (MEDIUM)

**Premise correction (from tobefixed.md:63).** The audit's "leaks the session task" is FALSE —
`run_cast_session` fully `return`s on every error path (verified `client.rs:187–189` discards the
JoinHandle; `start_cast` returns only `CastHandle`). What lingers is the stale `watch::Sender` inside
`CastHandle` held by the controller + the stale `active` published state. Minimal correct fix:
store the JoinHandle, poll `is_finished()` on the refresh tick, clear `active`, surface health. NOT
auto-reconnect (re-select is one click; auto-reconnect risks a retry loop against a rebooting/roamed
device).

**Exact changes.**
- `cast/client.rs`: add `pub struct CastSession { pub handle: CastHandle, pub task:
  tokio::task::JoinHandle<()> }` with `is_finished(&self)` and `stop(&self)`. Change `start_cast(…) ->
  CastSession` capturing `let task = tokio::spawn(run_cast_session(…))`. Fix the backwards
  doc-comment at `client.rs:184–186` (the controller detects exit by polling the JoinHandle; the
  watch does NOT close on task exit).
- `output/controller.rs`: replace `cast_handle: Option<CastHandle>` (line 189) with `cast_session:
  Option<CastSession>` + a local `let mut cast_health: Option<CastHealth> = None;`. On the Chromecast
  Select success arm (≈311–312) store the session + set `cast_health = Some(CastHealth::Connecting)`;
  error arms set `cast_health = None`. On the refresh tick (327–338), BEFORE republishing:
  `if let Some(s) = &cast_session { if s.is_finished() { eprintln!("…cast session ended…");
  cast_session = None; active = None; cast_health = Some(CastHealth::Lost); } }`. `teardown` takes
  `&mut Option<CastSession>`; body `if let Some(s) = cast_session.take() { s.stop(); }` (drop also
  drops the JoinHandle — do NOT await; keep teardown non-blocking, matching existing style). Thread
  `cast_health.clone()` into ALL SIX `set_output(OutputState { … })` sites (verified lines 193, 223,
  243, 264, 318, 334).
- `state.rs`: add `#[derive(Debug, Clone, PartialEq, Eq, Serialize)] #[serde(rename_all =
  "snake_case")] pub enum CastHealth { Connecting, Lost }` (SEE correction: drop `Live`). Add to
  `OutputState` (verified derives `Eq`, line 53): `#[serde(skip_serializing_if = "Option::is_none",
  default)] pub cast_health: Option<CastHealth>`. `Option<CastHealth>` is `Eq` so the `Eq`/`Default`
  derives stay valid.
- `web/ws.rs`: NO logic change (`OutputState` is cloned wholesale into `WsOutMessage::OutputState`);
  add/extend a serialization test only.
- `webui/src/lib/types.ts`: add `cast_health?: 'connecting' | 'lost' | null` to `OutputState`.
  `store.tsx` merges `output` wholesale on `output_state`, so no reducer change is required;
  optionally render a "reconnect"/"silent" hint.

**Corrections folded in.**
1. **DROP `CastHealth::Live`.** It is unreachable with this wiring — `CastSession` carries no
   `AppStateHandle`, so the MEDIA_STATUS path (`client.rs:344–346`) cannot transition Connecting→Live.
   Ship a two-state `Connecting`/`Lost` enum that matches the actual wiring. (If Live is ever wanted,
   it needs a separate liveness channel from `run_cast_session` to the controller — out of scope.)
   Update test_plan/live_verification to NOT assert Connecting→Live.
2. **Deterministic test (controller tests are current-thread `#[tokio::test]`, the doc comment
   claiming multi-threaded is wrong).** `tokio::time::pause` works on current-thread, but a real
   unreachable-connect's `is_finished()` depends on wall-clock OS I/O, NOT virtual time — advancing
   virtual time does not guarantee the cast task finished first. Design the test around a
   deterministically-finished JoinHandle (e.g. a seam to construct a `CastSession` whose task is
   already complete), not a real connect racing the tick.

**What could break + guard.**
- *every `OutputState {…}` literal must add the field:* grep confirms 6 controller sites + the
  state.rs test at ~465; `ws.rs` tests use `::default()` (safe). `Option<_>` + serde `default` +
  `skip_serializing_if` keeps Default/serde/wire backward-compatible.
- *`start_cast` return-type change:* only caller is `controller.rs` (grep-verified; client tests
  exercise `server_lan_ip`/`stream_url`/`insecure_tls_config`, not `start_cast`).
- *10s detection latency:* acceptable for cosmetic state; re-select before the tick `take()`s the
  dead session and starts fresh.
- *does NOT fire on shutdown:* the shutdown arm is biased-first and breaks the loop before the tick.

**Test plan.** (1) `cast/client.rs` `start_cast_returns_finished_handle_after_connect_fails` (or the
deterministic-finished-handle seam per the correction) → `is_finished()` becomes true. (2)
`output/controller.rs` `dead_cast_session_clears_active_on_tick` → after the session is finished,
advance past `REFRESH_INTERVAL` → `active.is_none()`, `cast_health == Some(Lost)`. (3) `state.rs`
serialize test + confirm existing `output_state_serializes_with_active_null` still passes (field
omitted via `skip_serializing_if`). (4) `ws.rs` extend `maps_output_state_event`. (5) webui vitest:
`output_state` with `cast_health:'lost'` merges and clears the active device. Gate: cargo + tsc +
vitest + vite.

**Live verification.** Select a Chromecast, confirm audio + health Connecting. Power-cycle / pull the
device; within ~10s the log shows "cast session ended — clearing active output", the UI clears the
active output, re-selecting starts fresh. Normal `systemctl --user stop` exits cleanly with no
spurious "cast session ended".

**Effort:** medium.

---

### NF-26 — no inbound deadline; half-open TCP cast silences for minutes (LOW)

**Chosen approach.** Track last-inbound-frame time and declare the link dead on a heartbeat tick with
no recent inbound activity, so `run_cast_session` RETURNS — which NF-8's tick then observes. Any
inbound frame (PING/MEDIA_STATUS/RECEIVER_STATUS) proves liveness, so tracking last-inbound is
strictly more robust than PONG-only.

**Exact changes (`cast/client.rs` `run_cast_session`).** add module const
`CAST_INBOUND_DEADLINE: Duration = Duration::from_secs(15)` (3× the 5s heartbeat — the AUD-065
"three strikes"). Before the loop: `let mut last_inbound = tokio::time::Instant::now();`. On the read
arm `Ok(n)` with `n>0`: `last_inbound = tokio::time::Instant::now();`. In the heartbeat tick arm,
AFTER sending PING: `if last_inbound.elapsed() > CAST_INBOUND_DEADLINE { eprintln!("…declaring link
lost"); return; }`. No change to the biased-first shutdown arm. Use `tokio::time::Instant` (not std)
for test determinism.

**What could break + guard.**
- *quiet-but-alive Chromecast falsely reaped:* 15s/3 cycles; live devices send heartbeat + periodic
  MEDIA_STATUS, so inbound stays fresh. The const is the single tuning knob.
- *ordering vs NF-8:* the prompt return is inert without NF-8's `is_finished()` tick — land WITH/AFTER
  NF-8.

**Test plan + caveat.** Extract a pure helper `fn link_lost(last_inbound, now, deadline) -> bool`
and unit-test it (lost when elapsed>deadline; alive at/under). **Caveat (from the risk review):** the
NF-8 unreachable-address test returns at the TCP-connect stage and NEVER enters the read/heartbeat
loop, so it does NOT exercise the `last_inbound` wiring. Document that the wiring is verified LIVE,
or extract the loop body / drive against a local TCP stub. The helper test covers only the arithmetic.

**Live verification.** Cast, then hard-drop the device's network (block its IP at the router) to
create a half-open TCP. Confirm the log "no inbound activity … declaring link lost" within ~15s, then
(via NF-8) the active output clears within the following refresh tick (~25–30s total) — versus minutes
today.

**Effort:** small.

---

### NF-7 — half-open WebRTC clients leak the PeerConnection + Opus pump (MEDIUM)

**Chosen approach.** Two defenses funnelling into the EXISTING idempotent
`WebRtcCommand::Stop` teardown (`webrtc_rs.rs:222–228` removes-before-acting, second Stop no-ops).
**Defense 1 (primary):** register `on_peer_connection_state_change` beside the existing
`on_ice_candidate` (`webrtc_rs.rs:157–171`). On the relevant state, send `WebRtcCommand::Stop {
session }` on a cloned `mpsc::Sender<WebRtcCommand>` — routing through the single command loop
(which owns the controller and is lock-clean) rather than calling `self.stop()` from a `'static`
callback that cannot borrow `&self`. This requires `WebrtcController` to hold a `webrtc_tx` clone.
**Defense 2 (secondary, optional):** in `ws_connection` (`server.rs:115–202`) add a
`tokio::time::interval` (15s) ping + a last-seen deadline (35s); refresh last-seen on every
`Some(Ok(_))` (covers Text/Binary/Pong); break (existing path sends Stop) when stale.

**Corrections folded in (critical).**
1. **Match `Failed` ONLY — drop `Closed` (and `Disconnected`).** `stop()` always calls `pc.close()`,
   which fires the state-change callback with `Closed`. Matching `Closed` would self-echo an extra
   `WebRtcCommand::Stop` on EVERY teardown (normal WS disconnect AND shutdown `stop_all`) — a
   guaranteed feedback echo, not a benign double-Stop. Dropping `Disconnected` avoids tearing down on
   a transient ICE blip the browser would recover. `Failed` covers the dominant ICE-Failed/sleep/
   NAT-rebind leak cases. (This INVERTS the raw design's "keep Failed|Closed" suggestion.)
2. **Callback shape:** `FnMut(RTCPeerConnectionState) -> Pin<Box<dyn Future<Output=()> + Send +
   'static>>`, identical to `on_ice_candidate`. The callback is `'static` → it captures a cloned
   `Sender<WebRtcCommand>`, not `&self` (the raw design is correct here).
3. **Message::Ping/Pong grounding was fabricated** in the raw design ("ws.rs:577,585" — `ws.rs` is
   ~330 lines with no Ping/Pong). Re-verify against `axum::extract::ws::Message` (already imported
   `server.rs:15`) before relying on it. **The WS-ping half is OPTIONAL defense-in-depth** — once
   `on_peer_connection_state_change(Failed)` reaps the leak, the WS ping only helps the rare
   TCP-half-open-but-ICE-nominally-connected case, at the cost of a new arm in the hot
   `ws_connection` loop. RECOMMENDATION: ship PC-state(Failed) first; treat WS-ping as a follow-on.

**Exact changes.**
- `webrtc_rs.rs`: `use crate::web::webrtc::command::WebRtcCommand;` +
  `use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;`. Add field
  `webrtc_tx: tokio::sync::mpsc::Sender<WebRtcCommand>` to `WebrtcController` (struct ≈86–93) + param
  to `new()` (≈96–102). After the `on_ice_candidate` block, register
  `pc.on_peer_connection_state_change(Box::new(move |state| { let tx = st_tx.clone(); Box::pin(async
  move { if matches!(state, RTCPeerConnectionState::Failed) { let _ = tx.send(WebRtcCommand::Stop {
  session }).await; } }) }))`.
- `main.rs`: REORDER so `(webrtc_tx, webrtc_rx)` (currently line 122) is created BEFORE the controller
  (line 121), then `WebrtcController::new(pcm_bus.clone(), state.clone(), webrtc_tx.clone())`. The
  same `webrtc_tx` is already moved into `WebContext` at line 261, so no new lifetime risk; the
  command loop already breaks on channel close.
- `server.rs` (optional WS-ping): before the loop add an interval + `last_seen`; add the select arm
  and the last-seen refresh.

**What could break + guard.**
- *self-echo on teardown:* eliminated by matching `Failed` only (correction 1).
- *transient Disconnected tearing a recoverable session:* eliminated by dropping `Disconnected`.
- *double-Stop (PC + WS):* `stop()` is idempotent (remove-before-close); second Stop no-ops.
- *channel reorder leaving a dangling clone:* the controller holds the same `webrtc_tx` `main` keeps
  via `WebContext`; the command loop lives as long as the controller.

**Test plan.** `webrtc_rs.rs` has no inline tests (integration glue); rely on `cargo build`
type-check of the callback (matches `on_ice_candidate` shape) + live verification. Extend `command.rs`
`offer_replies`/a `FakeWebRtcController` test to assert a Stop following a Stop for the same session
is a no-op (idempotency). If the WS-ping half ships, factor the idle decision into a testable helper.

**Live verification.** Safari Listen → audio. (a) Force-quit the tab / disable Wi-Fi mid-listen →
within the ICE-Failed timeout watch `journalctl` for the Stop, confirm via `ss -tunp | grep
soundsync` the PC is gone. (b) (if WS-ping) idle the bus, `kill -STOP` the browser to suppress Pong →
WS reaped at ~35s. (c) normal clean disconnect tears down exactly once (no double-Stop error).

**Effort:** medium (PC-state half); small WS-ping follow-on.

---

### NF-37 — uncapped /api/stream parec|ffmpeg spawns (MEDIUM, stream-cap half only)

**Chosen approach.** Process-wide `Arc<tokio::sync::Semaphore>` with a small fixed permit count (4)
guarding both stream handlers. On request `try_acquire_owned()`; on failure return 503 (matching the
existing idiom `stream.rs:75,79`). On success move the `OwnedSemaphorePermit` into the unfold state
tuple alongside the `child` (`stream.rs:85–86`) so it is held for the body lifetime and released on
drop (client disconnect), exactly mirroring `kill_on_drop`. **Do NOT change the `0.0.0.0` bind or add
auth** (intentional LAN-appliance design per tobefixed.md:48). The separate LOW "ffmpeg-missing-200-
empty" is a DIFFERENT finding — do not conflate.

**Exact changes.**
- `stream.rs`: `stream_aac`/`stream_mp3` take `State(ctx): State<WebContext>` (verified `status()` at
  `server.rs:105` already uses `State<WebContext>`; router `.with_state(ctx)` at `server.rs:102`).
  Pass `ctx.stream_limit.clone()` into `stream_response`. **Factor the permit check into a tiny helper
  returning `Result<OwnedSemaphorePermit, Response>`** so the 503 path is unit-testable WITHOUT
  spawning `sh`. Acquire BEFORE `Command::spawn` (before line 67). Thread the permit through the
  unfold seed tuple, dropping it only on the stream-end branch (line 90). Add `const
  STREAM_MAX_CONCURRENT: usize = 4;`.
- `server.rs` `WebContext` (≈36–53): add `pub stream_limit: std::sync::Arc<tokio::sync::Semaphore>`.
- `main.rs` `WebContext` build (≈259–269): add `stream_limit:
  std::sync::Arc::new(tokio::sync::Semaphore::new(STREAM_MAX_CONCURRENT))`.

**What could break + guard.**
- *handler signature change:* `WebContext` is already the axum state (verified) — `State<WebContext>`
  injection is the established pattern; routes unchanged.
- *permit leak shrinking capacity:* hold it in the SAME unfold state that holds `child` (proven to
  live exactly as long as the body); `OwnedSemaphorePermit` releases on Drop.
- *cap too low for legitimate use:* web Listen uses WebRTC (not /api/stream); /api/stream consumers
  are HTTP-fallback browsers + one Chromecast pull = one permit. 4 is comfortably above realistic use,
  below FD exhaustion; named const is tunable.

**Test plan.** Inline `stream.rs` tests: acquire all N permits then assert `try_acquire_owned` errs
(pure semaphore, no spawn); assert the helper maps exhaustion to 503. Existing `build_stream_command`
tests untouched. (`try_acquire_owned` needs `Arc<Semaphore>` — `WebContext.stream_limit` must be
`Arc`.)

**Live verification.** Fire >4 concurrent `curl localhost:8080/api/stream/audio.aac >/dev/null &`;
the 5th returns 503; `pgrep -c ffmpeg` never exceeds 4; kill the curls → ffmpeg count drops to 0;
single-client Listen/cast still works.

**Effort:** low.

---

### NF-15 — web `serve` unsupervised; AddrInUse-after-fast-restart permanently loses the UI (MEDIUM)

**Chosen approach.** Make the bind step resilient with a bounded bind-retry loop around the EXISTING
`tokio::net::TcpListener::bind`, raced against `wait_for_shutdown` so SIGTERM mid-retry returns
promptly. Prefer this over `supervise("web", …)` (it fixes the actual failure mode without wrapping a
normally-non-returning task) and over adding `socket2`.

**Corrections folded in.**
1. **Do NOT add `socket2`.** It is transitive-only (0 hits in `Cargo.toml`, confirmed). Use the
   dependency-free bounded retry. On Linux, `SO_REUSEADDR` on a LISTENING socket does NOT fix the
   dominant AddrInUse case (a prior LISTENER still held by an exiting instance) — **retry is the
   load-bearing fix, not REUSEADDR.**
2. **Race every retry sleep against `wait_for_shutdown(&mut shutdown)`** (verified `supervisor.rs:41`,
   `pub(crate)`, already imported `server.rs:11`; `serve` already clones `ctx.shutdown` at
   `server.rs:210`). Keep attempts ≤ 5, total worst-case < 3s, well under the 8s backstop.

**Exact changes.** `server.rs` `serve()` (209–218): replace the single `bind` with a
`bind_with_retry(config.bind, &mut shutdown)` helper that loops ≤ 5 times: `tokio::net::TcpListener::
bind(addr)`; on `AddrInUse` `tokio::select! { _ = sleep(500ms) => continue, _ =
wait_for_shutdown(&mut shutdown) => return the error }`; on other errors or after N attempts return
the `io::Error`. `serve()` still returns `Ok(())` on graceful shutdown (the `with_graceful_shutdown`
future at 214–216 is preserved). `main.rs` serve spawn (272–284) is structurally unchanged — the
retry happens inside `serve` before the error escapes, so the existing actionable AddrInUse
`eprintln!` still fires after retries exhaust.

**What could break + guard.**
- *retry delaying SIGTERM:* mitigated by racing each sleep against `wait_for_shutdown`.
- *port held by a different long-lived process:* retry delays the same eventual failure; the
  actionable AddrInUse log still fires (acceptable).
- *NO restart-loop regression:* retry is bounded and only around bind; `serve` is still fire-and-
  forget returning `Ok(())` on shutdown — it adds neither a process-fatal path nor anything armed
  before shutdown.

**Test plan.** Inline `server.rs` test (file currently has zero inline tests — adding `#[cfg(test)]`
is new but fine): bind an ephemeral `127.0.0.1:0`, then assert `bind_with_retry` to the same addr
errors after N attempts (drive with `start_paused` to fast-forward the 500ms sleeps), and that
signalling shutdown mid-retry returns promptly; a free port binds on the first attempt.

**Live verification.** Stop soundsync, immediately start a dummy `nc -l 8080` for ~1s then start
soundsync (provoke AddrInUse); confirm soundsync wins the bind within the retry window and the UI
loads (`curl localhost:8080/api/status` → 200); SIGTERM during the retry window exits within the
backstop.

**Effort:** low.

---

### NF-2 — unexpected critical-task exit must be process-fatal WITHOUT firing on shutdown (MEDIUM)

**Chosen approach (overrides the audit's `supervise()`-wrapping).** `supervise()` does NOT make
panics recoverable (panic ≠ Err; the supervise future aborts inside its own spawn → `JoinError`,
NF-30). The correct fix: retain the seven critical spawn JoinHandles (`main.rs:113, 123, 164, 186,
198, 248, 272`), race them with `futures::future::select_all` (futures 0.3 already a dep — no new
crate), and on the FIRST completion read the shutdown watch: if `!requested` → log the task name and
`std::process::exit(1)` (systemd `Restart=always` restarts everything, recovering BOTH panics and
clean returns); if `requested` → treat as expected (do nothing; the clean path + backstop own
teardown). EXCLUDE the SIGTERM-handler spawn (`main.rs:287`) from the fatal set (it legitimately ends
after sending shutdown). Add this as a THIRD arm of the final `tokio::select!` (331) beside
`daemon_fut` and `backstop`, leaving the existing two arms' Ok/Err handling (332–346) unchanged.

**Corrections folded in (critical — this is where the 8s-loop regression can re-enter).**
1. **Shutdown TOCTOU.** Two tasks return on a CHANNEL-DROP that is NOT the shutdown flag:
   `webrtc_command_loop` breaks when all `WebRtcCommand` senders drop (`command.rs:41`), and
   `run_spectrum` returns on `pcm_bus` `Closed` (`spectrum.rs`). During a normal SIGTERM, the watch
   is set first by the signal handler (`main.rs:299`), but a sender drop concurrent with shutdown
   could let the task break BEFORE the watch propagates `true` to the borrow → `select_all` fires with
   `requested == false` → `exit(1)` on a NORMAL shutdown (the forbidden crash look-alike). **GUARD:**
   on a handle completing, `tokio::task::yield_now().await` then RE-READ `*shutdown_rx.borrow()`
   before classifying, so an in-flight watch update is observed; OR gate the whole race so a sender-
   drop concurrent with shutdown is never Fatal. The unit test MUST cover the "shutdown set but flag
   not yet propagated" path, not just the two static branches.
2. **Testable seam.** Factor the decision into `fn classify_task_exit(shutdown_requested: bool, name:
   &str) -> TaskExit { Expected | Fatal(name) }` (pure logic, no `process::exit` in tests). Unit-test
   both branches AND the race path (task returns due to sender-drop concurrent with shutdown=true →
   Expected). Mirror the existing `backstop_never_fires_during_normal_operation` test (≈main.rs:436).
3. **Cluster-C coordination (serve handle).** NF-15 keeps `serve()` fire-and-forget returning
   `Ok(())` on graceful shutdown. NF-2 captures serve's JoinHandle but its clean `Ok(())` after
   shutdown MUST be classified Expected — otherwise NF-15's preserved clean return reintroduces a
   restart on normal shutdown. Same "arm only after shutdown is requested" discipline as the existing
   backstop, inverted.
4. **Watch clone ordering.** Take a dedicated `shutdown_rx.clone()` for the supervisor BEFORE the
   final move into `run_daemon` (line 318); add it as the extra clone.

**Exact changes.** Capture each of the seven spawns into a named handle (e.g. `spectrum_h`,
`webrtc_h`, `bt_h`, `media_h`, `eq_h`, `output_h`, `web_h`). Build a `Vec<(label, BoxFuture<()>)>`
mapping each `JoinResult`/`JoinError` to `()`. Race with `select_all`; on first completion run the
yield+re-read guard + `classify_task_exit`; map `Fatal(name)` → `eprintln!` + `exit(1)`. Add as the
third `select!` arm. Channels/`install_crypto_provider`/the SIGTERM spawn unchanged.

**What could break + guard.** (see corrections) The dominant risk is the TOCTOU → guarded by
yield+re-read. A benign early return unrelated to shutdown would be treated as fatal — verified none
of the seven returns before shutdown under healthy operation (each loops until `wait_for_shutdown`
or, for `run_bluetooth`, joins shutdown-aware inner loops whose end IS a real death worth being
fatal). `select_all` boxing loses panic-vs-clean distinction — acceptable (both are unexpected exit;
the log just can't say which).

**Test plan.** Unit-test `classify_task_exit` (Expected when requested; Fatal(name) when not). A
`#[tokio::test]` that runs a dummy immediate handle through the same `select_all` + yield + classify
structure: shutdown unset → Fatal; shutdown pre-set → Expected; AND the race path (set shutdown,
spawn a handle that returns on a dropped sender → Expected). No `process::exit` in tests.

**Live verification.** `journalctl --user -u soundsync -f`. (1) `systemctl --user restart soundsync`
→ NO "exited unexpectedly" log; `systemctl --user show soundsync -p NRestarts` does not climb. (2)
debug-only path making one task return early/panic → log shows "critical task <name> exited
unexpectedly" and systemd restarts exactly once (NRestarts +1), not a tight loop. **This is the
daemon-stability canary** (with WebRTC continuity — SEE §6).

**Effort:** medium.

---

### NF-33 — debian/rules relies on root's ~/.local/node (LOW, mention-only)

No change. tobefixed.md:233 dismisses the audit's "drop the ~/.local/node branch" — it would BREAK
this host's only working build path (`debian/rules:13–18`: `SOUNDSYNC_NODE_BIN` → `~/.local/node`
(gated) → PATH). The only in-scope action is to pass `SOUNDSYNC_NODE_BIN` in the NF-9/10/31 build
instructions (already specified). Optional future hardening (out of scope): an early-fail in
`override_dh_auto_build` when no node is resolvable.

---

## 4. Cross-cutting coordination — shared files and ownership

| Shared file | Items | Who owns when / coordination rule |
|-------------|-------|-----------------------------------|
| `debian/postinst` | NF-9, NF-10, NF-31 | ONE serialised hunk in the `configure)` case, top→bottom: (1) apply-wp [existing] → (2) daemon-reload [existing] → (3) enable adapter + **NEW enable CoD** [NF-9] → (4) global-enable soundsync [existing] → (5) source debconf + resolve `$SS_USER` + enable-linger [NF-10] → (6) conditional user-WP restart [NF-31]. PRESERVE `#DEBHELPER#` at line 43; insert nothing after it. |
| `debian/prerm` | NF-9 | CoD disable beside the existing adapter disable. |
| `debian/postrm` | NF-10 | `db_purge` (last debconf call) in the purge case; NO `disable-linger` (correction). Do NOT touch the WP-Lua wrong-purge-path line 6 (separate LOW item, out of scope). |
| `debian/control` | NF-10 | add `debconf` to Depends. |
| `debian/changelog` | NF-9, NF-10, NF-31 | single combined entry. |
| `debian/soundsync.install` | NF-9 | append two artifact lines. |
| `debian/rules` | NF-9 (override_dh_fixperms) | add `override_dh_fixperms` chmod 0755 for the CoD script; leave the node-resolution logic intact (NF-33). |
| `src/main.rs` | NF-7, NF-37, NF-15, NF-2 | Serialised single pass, in order: NF-7 channel reorder (121–122) → NF-37 WebContext build (259–269) → NF-15 serve spawn (272–284, structurally unchanged) → **NF-2 LAST** (capture all 7 JoinHandles + add the third select! arm + supervisor `shutdown_rx.clone()` before line 318). Ranges are disjoint; order matters because `WebContext.webrtc_tx` is moved at 261 (needs NF-7's reorder first). |
| `src/web/server.rs` | NF-7, NF-37, NF-15 | One pass: NF-7 ws_connection select arm (115–202, optional) + `webrtc_tx` plumbing; NF-37 `WebContext.stream_limit` field (36–53); NF-15 `bind_with_retry` in `serve` (209–218). Disjoint regions. |
| `src/state.rs` | NF-8 | additive: `CastHealth` enum + `OutputState.cast_health` (`Option`, serde default + skip). Coordinate with NF-13's webui type edits to avoid `OutputState {…}` literal merge conflicts in tests. |
| `src/web/ws.rs` | NF-8 | NO logic change (OutputState cloned wholesale); test additions only. |
| `webui/src/lib/types.ts`, `store.tsx` | NF-13, NF-8 | both additive (one optional field + one union variant for NF-13; one optional `cast_health` for NF-8). No exhaustiveness break (store has `default: break`). Sequence the small edits. |
| `src/daemon.rs` | none | NOT touched by any item — confirm it stays untouched. |
| Shell vars in maintainer scripts | NF-10, NF-31 | `SS_USER`/`SS_UID` (NOT `USER`/`UID`). |

**No Rust runtime code is touched by any packaging item or NF-13** → zero risk of reintroducing the
8s-loop or breaking shutdown there, and the `CommandRunner`/`Fs`/`Spawner` `RefCell`/`!Send` fakes
are involved only in NF-1/NF-6 (kept synchronous, on the borrowed `&R` seam) and NF-8 (controller
tests use the Mutex-backed sync fake; no change to the seam).

---

## 5. Consolidated "WHAT COULD BREAK" ledger (ranked by severity)

| Sev | Risk | Item | Guard |
|-----|------|------|-------|
| **Critical** | NF-2 reintroduces the 8s-restart-loop via shutdown TOCTOU (task returns on sender-drop before the watch propagates → `exit(1)` on a normal shutdown). | NF-2 | `yield_now()` + re-read `*shutdown_rx.borrow()` before classifying; gate the race so a sender-drop concurrent with shutdown is Expected; unit-test the race path. |
| **High** | dpkg transaction ABORTS under `set -e` from a bare failing `systemctl`/`loginctl`/`runuser`/debconf call. | NF-9/10/31 | every new call ends in `\|\| true` (or `set +e` fence); `db_*` calls especially (they return nonzero as normal flow); copy the proven adapter idiom. |
| **High** | NF-7 callback self-echoes `Stop` on EVERY teardown (matching `Closed`/`Disconnected`). | NF-7 | match `Failed` ONLY. |
| **High** | CoD unit fails at runtime because the shipped script lacks +x (source is 0664). | NF-9 | `override_dh_fixperms` chmod 0755; verify with `dpkg-deb -c` showing `-rwxr-xr-x`. |
| **Medium** | NF-6 reuse-verify `?` propagates a command error (incl. NF-1 `TimedOut`) and ABORTS bring_up, destroying a live sink on a transient pactl stall. | NF-1+NF-6 | Err/non-success arm returns `Ok(reused)` (keep reuse), NOT `?`. |
| **Medium** | NF-10 `disable-linger` on purge revokes a shared per-user property other services need. | NF-10 | DROP disable-linger; keep only `db_purge`. |
| **Medium** | NF-8 `CastHealth::Live` is unreachable (no AppStateHandle in CastSession) → dead state + unverifiable live check. | NF-8 | ship `Connecting`/`Lost` only; fix tests. |
| **Medium** | `OutputState {…}` literal compile breaks from the new field. | NF-8 | `Option<_>` + serde default; grep all 6 controller sites + state.rs test. |
| **Medium** | main.rs/server.rs merge churn across NF-7/37/15/2. | C+B | one serialised edit pass; disjoint ranges; documented order. |
| **Low** | NF-15 retry delays SIGTERM. | NF-15 | race each sleep against `wait_for_shutdown`; ≤5 attempts, <3s. |
| **Low** | NF-37 permit leak shrinks capacity. | NF-37 | hold permit in the unfold state next to `child`; releases on Drop. |
| **Low** | NF-26 false-reaps a quiet-but-alive cast. | NF-26 | 15s/3-cycles deadline; tunable const. |
| **Low** | NF-1 pipe-buffer deadlock on a wedged child. | NF-1 | small outputs; kill-before-read on timeout. |
| **Low** | NF-8 deterministic-test flakiness (real connect races virtual time on a current-thread runtime). | NF-8 | construct a deterministically-finished JoinHandle seam, not a real connect. |
| **Low** | NF-13 tsc fails on existing fixtures. | NF-13 | `device_name?` optional + store default. |
| **Note** | dh_installsystemd double-enable for the CoD unit. | NF-9 | harmless/idempotent (already tolerated for the adapter unit); document, add no third path. |

---

## 6. Verification strategy

**Gate after EACH item** (no item lands without a green gate):
- Rust items (NF-1, NF-6, NF-7, NF-37, NF-15, NF-2, NF-8/26): `cargo build && cargo test && cargo fmt
  --check` in `sc-next`. NF-8 also: `cd webui && npm run -s tsc && npx vitest run && npm run build`.
- webui item (NF-13): `tsc --noEmit`, `vitest run`, `vite build` (backend gate is a no-op pass).
- packaging items (NF-9, NF-10, NF-31): packaging-only gate — `SOUNDSYNC_NODE_BIN=~/.local/node/bin
  dpkg-buildpackage -us -uc -b`, `dpkg-deb -c`, `dh_install --list-missing`, `lintian`. Rust/web
  gates stay untouched/green.

**Specific live checks:**
- **WebRTC continuity = the daemon-stability canary (NF-2 + NF-7).** After EVERY Rust item, open
  Safari Listen and confirm uninterrupted audio across a `systemctl --user restart soundsync` and
  across the change landing. For NF-2: confirm `NRestarts` does not climb on a normal restart, and
  climbs by exactly 1 (no tight loop) on a simulated crash.
- **The .deb path (NF-9/10/31).** Build + `apt install` on a test box; CoD unit enabled+active;
  linger=yes after a headless reboot with no login; A2DP routes after install with no manual WP
  restart; `apt remove`/`purge` clean (files gone, linger NOT revoked).
- **Cast recovery (NF-8/26).** Select a Chromecast, power-cycle it; confirm the "cast session ended"
  log + UI active-output clears within ~10–30s; re-select works; normal shutdown shows no spurious
  "cast session ended".
- **Stream cap (NF-37).** >4 concurrent `/api/stream` curls → 5th is 503; `pgrep -c ffmpeg` ≤ 4;
  drops to 0 after disconnect.
- **Bind retry (NF-15).** Provoke AddrInUse with a transient `nc -l 8080`; soundsync wins the bind;
  UI loads; SIGTERM mid-retry exits within the backstop.
- **Rename (NF-13).** Rename → input + second tab + pairing banner all show the new name; reload
  persists; phone shows the new alias.

---

## 7. OPEN QUESTIONS for the human (decide before coding)

1. **NF-10 appliance-user mechanism (the headline decision).** Confirm **debconf with an
   auto-detected default** is the chosen mechanism (vs a bare `/etc/default/soundsync USER=` or a
   loud-note-only path). Confirm the residual is acceptable: a **noninteractive (`apt -y`) install on
   a multi-user box with no preseed leaves linger unset** (the single-user default still covers the
   dominant case; the loud postinst message covers the empty case). If unacceptable, we need a
   fallback (e.g. also write `/etc/default/soundsync` so a re-run can pick it up).
2. **NF-10 purge symmetry.** Confirm we **drop `loginctl disable-linger` on purge** (linger is shared;
   `install.sh`/`uninstall.sh` never disable it). Keeping it is the design's over-reach.
3. **NF-7 WS-ping scope.** Ship **PC-state(Failed) only** now and defer the WS ping/idle-timeout as
   optional defense-in-depth? (PC-state alone closes the dominant leak; WS-ping adds a new arm to the
   hot ws loop for the rare TCP-half-open-but-ICE-connected case.)
4. **NF-7 Disconnected policy.** Confirm matching **`Failed` only** (drop `Disconnected`) — a
   transient ICE blip then re-Listen (one click) vs keeping `Disconnected` and risking teardown of a
   recoverable session.
5. **NF-8 health granularity.** Confirm the **two-state `Connecting`/`Lost`** enum (drop `Live`). Add
   `Live` only if you want a liveness channel from the cast session to the controller (extra scope).
6. **NF-1 timeout value + override.** Confirm `COMMAND_TIMEOUT = 10s` (fixed const) vs adding a
   `SOUNDSYNC_COMMAND_TIMEOUT` env override (mirroring `resolve_shutdown_timeout`). Confirm
   `wait-timeout 0.2` is acceptable for the air-gapped/vendored build.
7. **WP-Lua wrong-purge-path (postrm:6) — explicitly OUT OF SCOPE here.** Confirm we leave it for a
   separate LOW change even though NF-10 edits postrm (resist scope-creep).

---

*End of plan. Nothing in `sc-next` was modified; this document is the only artifact produced.*

# Audio Routing Nuances — "Gotchas Ledger" for SoundSync (v2.9.11)

> CROWN JEWEL / PRIORITY deliverable for the ground-up rebuild. Each row is a workaround /
> ordering dependency / magic number / race guard that was paid for in real bugs. If a
> rebuild drops or changes one of these, it will silently reproduce an old bug. Source
> columns cite `file:line` in the legacy tree (`/home/mark/sc/sc`) and commit hashes where
> the "why" is documented. Confirmed against code, comments, and git history. Anything not
> root-caused is in **Unexplained behaviours** at the end.

## End-to-end audio path (as actually implemented)

A phone/source pairs over Bluetooth Classic. **WirePlumber (not SoundSync) owns all A2DP
codec negotiation and transport acquisition** — SoundSync registers NO custom
`MediaEndpoint1` (the code in `endpoint.rs` exists but is intentionally never registered;
`main.rs:138-154`). The SoundSync Bluetooth manager (`bluer` + raw zbus) just powers the
adapter, forces an audio Class-of-Device, runs an auto-accept pairing agent, drives
discovery, and watches for a `bluez_input.*` PipeWire node to appear (signal that
WirePlumber acquired the transport → device goes `AudioActive`). WirePlumber decodes the
A2DP stream to 48 kHz stereo f32 PCM and presents `bluez_input.*` as a PipeWire source.
SoundSync creates a null sink `soundsync-capture` (via `pactl module-null-sink`, fallback
`pw-loopback`) and a 10-band biquad EQ as a **subprocess** `pipewire -c filter-chain.conf`
(`effect_input.soundsync-eq` → `effect_output.soundsync-eq` → `node.target=soundsync-capture`).
When EQ is on, the **default sink is set to `effect_input.soundsync-eq`** so BT audio flows
BT→EQ→null sink; when off, default sink is `soundsync-capture` directly. Everything
downstream captures from `soundsync-capture.monitor`: (a) spectrum analyzer (`parec/pw-cat`
broadcast → rustfft 2048 → 64 log bands), (b) WebRTC (broadcast PCM → Opus 128k → RTP PT
111 → browser), (c) HTTP `/api/stream/audio.{aac,mp3}` via **direct `parec | ffmpeg` OS
pipes per client** (no broadcast channel — that path stuttered), (d) Chromecast (tells the
cast device to pull the HTTP AAC URL), (e) AirPlay (PipeWire RAOP sink linked to the null
sink monitor via `pw-link`). Line-in is a `module-loopback` from `alsa_input.*` into the
same null sink. WebRTC signalling rides the existing `/ws/status` WebSocket.

---

## Ledger

### BlueZ / pairing / A2DP endpoint

| ID | Subsystem | Observed behaviour / workaround | Why it exists | Source (file:line / commit) | Risk if dropped |
|----|-----------|----------------------------------|---------------|-----------------------------|------------------|
| AUD-001 | BlueZ/endpoint | SoundSync registers **NO** custom `org.bluez.MediaEndpoint1`. `endpoint.rs` is dead/reference code; the D-Bus connection is kept alive only for the agent. | Registering custom endpoints steals the A2DP transport from WirePlumber → WP logs "unknown transport" and never creates `bluez_input.*` → no audio. Cost 3+ PRs. | `main.rs:138-154`; `bluetooth_improvements.md:106-108`; `CONTEXT.md:164-168` | Total silent audio failure; reproduces the original multi-PR "no audio" saga. |
| AUD-002 | BlueZ/CoD | At startup force adapter Class-of-Device to **`0x240414`** (Audio/Video major, HiFi minor, Rendering+Audio service bits) by shelling out to `hciconfig <hci> class 0x240414`. | BlueZ 5.x ignores `Class=` in main.conf and `bluer 0.17` has no setter; adapter defaults to "Computer/Laptop". A2DP **source** devices filter inquiry candidates by CoD and skip non-sink-looking devices (e.g. AT-SB727 turntable saw adapter but never paired). | `manager.rs:121-131,852-909` (`ADAPTER_COD="0x240414"`); commit `b1bc5e0`; install.sh:193 `Class = 0x240414`; lessons.md:48-50 | Source-role devices (turntables, some phones) won't initiate pairing; "visible but won't connect". |
| AUD-003 | BlueZ/name | Also set HCI local name via `hciconfig <hci> name <name>` in addition to `Adapter1.Alias`. | `set_alias` covers EIR Alias (BlueZ precedence), but some source firmwares read the raw HCI name. Belt-and-braces. A silent `set_alias` failure once made the adapter advertise the hostname and an A2DP source rejected it. | `manager.rs:133-138,911-955`; alias failure promoted to `error!` at `manager.rs:110-119` | Edge-case firmwares advertise wrong name; harder to diagnose pairing rejections. |
| AUD-004 | WirePlumber/headless | Write `wireplumber.profiles.main.monitor.bluez.seat-monitoring = disabled`. | `bluez.lua` only calls `createMonitor()` when WpLogind `seat_state == "active"` (needs a graphical/TTY seat). SSH-only/headless sessions are "online" but never "active" → BlueZ monitor never starts → no A2DP MediaEndpoints → adapter SDP omits 0x110b → iOS won't list it as audio output. | install.sh:253-267; commit `c34ddb8` ("pairs but no audio output" on Ubuntu 24.04+/PW1.6+/WP0.5+) | Headless deployments (the primary target, Raspberry Pi) pair but produce no audio. |
| AUD-005 | BlueZ/group timing | Installer verifies the **systemd user manager** (`user@UID.service`) already has the `bluetooth` group in its *running* credential set, not just `/etc/group`. | `usermod -aG` only edits `/etc/group`; already-running processes keep old credentials. `user@` spawns pipewire/wireplumber inheriting them; adding the group after boot → WP's BlueZ5 SPA monitor can't talk to BlueZ despite correct `/etc/group`. | install.sh:28-51 (`user_manager_has_group`) | WP silently can't reach BlueZ until a full re-login/reboot; "works after reboot" flakiness. |
| AUD-006 | Pairing agent | Agent capability **`NoInputNoOutput`**; auto-accepts: PIN→`"0000"`, passkey→`0`, confirmation/authorization auto-OK (gated on `auto_pair`). Registered at `/org/soundsync/agent`, then `RequestDefaultAgent`. | Headless device has no UI for pairing prompts; must auto-accept to be usable as a speaker. | `agent.rs:11-101`; `constants.rs:10` | Pairing prompts hang forever with no UI to answer them. |
| AUD-007 | Discovery/scan-vs-audio | Auto-stop BT discovery the moment any device reaches `AudioActive` (not merely Connected). | BT scanning (inquiry) and A2DP streaming share one radio; the adapter time-division-multiplexes → periodic audible stutter. Pure hardware constraint, no buffering fixes it. | `manager.rs:232-248,488-498`; lessons.md:100-103 | Audio stutters whenever the scan UI is left running; users blame the audio pipeline. |
| AUD-008 | Discovery/stale session | On scan start: `StopDiscovery` via **raw D-Bus** (bypassing bluer), poll `Discovering==false` (≤2s, 100ms steps), then create a **fresh** `bluer::Session`+`Adapter`, call `discover_devices()`, wrap stream to keep session alive. | `bluer`'s `SingleSessionToken` async-Drop may not finish before the next scan → bluer thinks a session is active → "Operation already in progress" even when BlueZ is idle. Also `adapter.events()` does NOT emit `DeviceAdded`; only `discover_devices()` subscribes to ObjectManager `InterfacesAdded`. | `manager.rs:650-730`; commits `36fad8a`, `103a851`, `1f2eea6` | Scan button breaks intermittently; discovered devices never appear. |
| AUD-009 | Discovery/name resolution | Two-tier friendly-name resolution: (a) passive per-device fast poll of `alias()` at **250ms** for ≤24 attempts (~6s); (b) active HCI `hcitool -i <hci> name <MAC>` on demand ("Identify"), capped at concurrency **3**, 7s timeout. `update_device_name` rejects empty/MAC-shaped names. | BlueZ populates `Alias` asynchronously after `DeviceAdded`; the 500ms poll can take 5–10s on a busy radio → users stare at MAC addresses. D-Bus `Device1` has no "fetch name now"; HCI Remote-Name-Request does it without pairing. Needs `bluetooth` group. | `constants.rs:20-26`; `manager.rs:733-850,957-1025`; commits `80a926f`, `8a28982` | Devices show as bare MACs; "Identify" button does nothing. |
| AUD-010 | Device state machine | 8-state machine `Disconnected→Discovered→Pairing→Paired→Connected→ProfileNegotiated→PipewireSourceReady→AudioActive`; `AudioActive` is gated on a `bluez_input.*` node actually appearing in PipeWire (detected via `pactl list short sources`), not just BlueZ "connected". | BlueZ "connected" ≠ audio flowing. WirePlumber must acquire the transport and create the PipeWire node first; that's the real readiness signal. | `manager.rs:572-593,614-648`; `device.rs`; CONTEXT.md:24-35 | UI shows "connected" with no audio; AVRCP/streaming start before the node exists. |
| AUD-011 | D-Bus liveness | AVRCP monitor tracks consecutive D-Bus failures; after **5** it tears down and re-establishes the system connection. | Long-running zbus connections silently go stale after adapter reset / suspend-resume / dbus restart; all proxy calls then fail silently. | `avrcp.rs:21-22,72-91,351-368`; lessons.md:3-5 | Media controls & metadata silently die after suspend/adapter reset with no recovery. |
| AUD-012 | Path encoding | MAC↔D-Bus path uses `dev_AA_BB_..` ↔ `AA:BB:..`; `address_from_path` finds the `dev_` segment so it also works on transport paths with trailing `/sep1/fd0`. | Transport/endpoint paths append extra segments; naive "last path component" parsing breaks. | `constants.rs:28-43` + tests `:62-72` | BT source→MAC mapping fails for transport paths; device never reaches AudioActive. |

### Codecs (A2DP capability blobs)

| ID | Subsystem | Observed behaviour / workaround | Why it exists | Source (file:line / commit) | Risk if dropped |
|----|-----------|----------------------------------|---------------|-----------------------------|------------------|
| AUD-013 | Codec advertise | WirePlumber config advertises roles `[ a2dp_sink a2dp_source hfp_hf hfp_ag hsp_hs hsp_ag ]` (0.5) and codecs `[ sbc aac ldac aptx aptx_hd ]`; `enable-sbc-xq=true`, `enable-msbc=false`, `enable-hw-volume=true`. The Rust `codecs/*.rs` blobs are **dead code** (`#[allow(dead_code)]`) — WP owns negotiation. | Codec selection happens in `libspa-bluez5`, not SoundSync (see AUD-001). The blobs document expected capabilities but are not wired in. SBC-XQ raises SBC quality; mSBC is for voice (HFP) and disabled to avoid mic-quality fallback. | install.sh:273-279, 306-330; `codecs/sbc.rs`, `aac.rs`, `ldac.rs`, `aptx.rs`, `aptx_hd.rs` (all `#[allow(dead_code)]`) | A rebuild that "implements" the Rust blobs would re-introduce the endpoint conflict (AUD-001). |
| AUD-014 | Codec libs | LDAC/aptX need `libldacbt-enc2` / `libfreeaptx0` AND the WP systemd sandbox override (AUD-015). SBC mandatory, AAC for iOS. | Vendor codec sub-plugins are separately packaged; without them WP silently falls back to SBC. | install.sh:271-275 (comment); plan/9.md | Android HQ (LDAC) / aptX silently downgrade to SBC. |
| AUD-015 | WP sandbox | Write a WirePlumber systemd user override `MemoryDenyWriteExecute=no`. | Stock `wireplumber.service` ships `MemoryDenyWriteExecute=yes`, which blocks `libspa-bluez5.so` codec sub-plugins (LDAC/aptX) from mmap'ing PROT_WRITE\|PROT_EXEC. Symptom is silent: WP starts, bluez monitor logs nothing, no MediaEndpoints. Works when WP run manually from a shell but NOT under systemd. | install.sh:283-301 | LDAC/aptX (sometimes all BT audio) silently fail only under systemd — the worst "works on my machine". |
| AUD-016 | SBC config | SBC capability advertises all rates (`0x3F`), all block/subband/alloc (`0xFF`), bitpool **2–53**; selection prefers 44.1 kHz Joint Stereo, 16 blocks, 8 subbands, Loudness. | bitpool 53 = high-quality SBC; joint stereo + loudness alloc is the standard HQ profile. (Reference only — see AUD-013.) | `codecs/sbc.rs:13-70` | If ever wired in, low bitpool = poor SBC quality. |

### WirePlumber / PipeWire graph & module ordering

| ID | Subsystem | Observed behaviour / workaround | Why it exists | Source (file:line / commit) | Risk if dropped |
|----|-----------|----------------------------------|---------------|-----------------------------|------------------|
| AUD-017 | WP version split | Detect WirePlumber version: 0.5+ → SPA-JSON `.conf` in `/etc/wireplumber/wireplumber.conf.d/`; 0.4.x → **Lua** in `~/.config/wireplumber/bluetooth.lua.d/` (and `/etc/.../bluetooth.lua.d/`). Clean up the wrong-format file for the detected version. | A `.conf` written for 0.5 is **silently ignored** by 0.4.x and vice-versa; a naive "does any a2dp_sink config exist?" check passes while the config never loads. | install.sh:219-244; lessons.md:52-55 | Config "present" but never loaded → no BT audio, passing diagnostics. |
| AUD-018 | WP 0.4 Lua | In 0.4.x Lua, set **individual** props (`bluez_monitor.properties["bluez5.roles"]=...`), never assign the whole `bluez_monitor.properties` table. | Replacing the table wipes defaults including `with-logind=true`; without logind the BlueZ monitor never activates for the user session → zero bluez5 devices. This was the actual root cause of one "connects but no audio". | install.sh:307-314, 322-329; lessons.md:57-60 | Wipes WP defaults → BlueZ monitor dead → no audio. |
| AUD-019 | Config→reload | After writing any WP config, **restart WP and verify `is-active`**, with recovery on failure; never "restart \|\| warn" and continue. | "Config written" ≠ "config loaded"; a failed restart leaves old/no config active while install reports success. | install.sh:334-365; lessons.md:23-29 | Install reports success but config isn't live; downstream steps assume a working graph. |
| AUD-020 | `systemctl --user` from root | Any `systemctl --user` from the (root) installer must `su - $USER` with `XDG_RUNTIME_DIR=/run/user/UID` and `DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/UID/bus` exported. | systemd user bus is per-user; without those env vars `systemctl --user` silently no-ops from a root shell. | install.sh:340-344; fix-audio-scheduling.sh:134-138,241-245; lessons.md:19-21 | WP/PipeWire restarts silently do nothing; config never applies. |
| AUD-021 | Null sink name | Null sink is `soundsync-capture` (desc `SoundSync-Capture`); its monitor `soundsync-capture.monitor` is the single capture tap for spectrum/WebRTC/HTTP/Chromecast/AirPlay. | One canonical capture point so all consumers get EQ-processed audio. Hard-coded across many files. | `pipeline.rs:18-19`; `capture.rs`; `airplay.rs:13`; `chromecast.rs`; `cast_stream.rs:24` | Renaming in one place but not all → silent capture failure in some consumers. |
| AUD-022 | Default-sink routing | When EQ enabled, default sink = `effect_input.soundsync-eq`; when disabled, default sink = `soundsync-capture`. Must be re-applied after every (re)start and every EQ toggle. | If default sink is the null sink while EQ is enabled, BT audio lands directly in the null sink and **bypasses EQ**. Virtual nodes are ephemeral (don't survive reboot). | `pipeline.rs:20-22,70-89,342-353`; CONTEXT.md:170-185, 201-211; lessons.md:44-46 | EQ silently has no effect; or after a reboot audio routes to hardware speakers instead of capture. |
| AUD-023 | Filter-chain spawn | EQ runs as a **subprocess**: prefer `pipewire-filter-chain --config <p>`, fallback `pipewire -c <p>` (Debian/Ubuntu/RPi OS). Config written atomically (write `.tmp` + rename) to `$XDG_RUNTIME_DIR/soundsync/filter-chain.conf`. | Distros split the filter-chain binary differently; embedded mode is the only option on RPi OS. Atomic write avoids the subprocess reading a half-written config. | `filter_chain.rs:14-91`; equalizer.rs:111-170 | EQ silently fails on RPi (no standalone binary) or reads truncated config. |
| AUD-024 | Filter-chain restart cost | Updating EQ kills and respawns the whole filter-chain subprocess (~200ms audio dropout); after respawn, `sleep 500ms` before re-setting default sink. | PipeWire builtin filter-chain has no live coefficient control via this path; restart is the only update mechanism. Nodes need time to register before the default-sink call can find them. | `pipeline.rs:74,342-348`; `filter_chain.rs:26-62`; do-over.md:434-438 | Race: default sink set before EQ nodes exist → routing fails; or no settle delay → intermittent EQ loss. |
| AUD-025 | Filter graph fields | Generated config MUST include `media.class`, `audio.rate=48000`, `audio.channels=2`, `audio.position=[FL FR]` on both capture/playback props, `node.target="soundsync-capture"`, AND `inputs`/`outputs` referencing first/last band. | Missing `inputs`/`outputs` was a real bug — filter-chain output went nowhere (v2.7.0 fix). `node.target` is how WP links it (don't create links manually). | `equalizer.rs:135-170`; CONTEXT.md:170-179; do-over.md:1227 | EQ output silently routed nowhere; "EQ on but no sound". |
| AUD-026 | Duplicate-module cleanup | systemd `ExecStartPre` and runtime code unload any `module-(null-sink\|loopback)` whose args contain `soundsync` before creating new ones; null-sink creation checks for an existing module first and reuses it. | Crashes/restarts accumulate duplicate modules → duplicate capture/EQ → stuttering. | scripts/soundsync.service:14; `pipeline.rs:180-189`; `line_in.rs:188-233`; lessons.md:31-33; audio-stutter-debug.sh:78-94 | Module pileup across restarts → progressive stuttering, hard to diagnose. |
| AUD-027 | Audio-service readiness | Before building the graph, poll `pactl info` (or `pw-cli info 0`) in a loop up to **30s** before proceeding. | systemd `After=pipewire.service` guarantees start order, NOT readiness. After reboot, services are "started" but not yet accepting commands. | `pipeline.rs:99-157`; lessons.md:7-9 | Graph creation fails on cold boot; "works after I restart it manually". |
| AUD-028 | Null-sink retry/backoff | `pactl load-module module-null-sink` retried up to **10×** with backoff (1s…capped 5s), then verified via `pactl list short sinks`. | pipewire-pulse may not be ready right after boot even once pactl responds. | `pipeline.rs:180-249` | Null sink missing on cold boot → capture falls back to direct BT (no EQ) or silence. |
| AUD-029 | Tool fallback chains | Null sink: pactl→pw-loopback. Set-default: pactl→wpctl (parse `wpctl status` for node id). BT source find: pactl→pw-cli→pw-dump. Node exists: pactl→pw-cli. | Support both PulseAudio-compat (`pactl`) and PipeWire-native tooling; not all systems have both. | `pipeline.rs:161-338`; `capture.rs:240-358` | On pactl-less systems the whole graph silently fails to build. |
| AUD-030 | snd-aloop / competing servers | Installer warns (does NOT auto-remove) if `snd-aloop` loaded, and warns if `bluealsa`/real `pulseaudio` running. Diagnostics flag PulseAudio daemon & bluealsa as transport thieves. | A competing sound server registers competing BT endpoints with BlueZ → WP logs "unknown transport... multiple sound server instances" and never creates `bluez_input.*`. Auto-removing snd-aloop could break other setups. | install.sh:132-139; lessons.md:62-64; CONTEXT.md:254 | Silent "connected but no audio" with a cryptic WP log; or auto-removal breaks user audio. |
| AUD-031 | Stale pipewire-pulse | Diagnostics compare start times of pipewire / pipewire-pulse / wireplumber; >1h apart ⇒ stale; always restart the three together. | A pipewire-pulse left running against an old pipewire instance causes intermittent routing failures. | fix-audio-scheduling.sh:207-230; lessons.md:129-131 | Intermittent, un-reproducible audio routing failures after partial restarts. |
| AUD-032 | Service user | Service MUST run as the PipeWire **session user** (`$SUDO_USER`), Group=audio, with `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, `PULSE_RUNTIME_PATH` set to that user's `/run/user/UID`. NOT root, NOT a system user. | PipeWire is per-user; a `soundsync` system user cannot reach the human user's PipeWire/null sink → silent failure. | scripts/soundsync.service:8-18; CONTEXT.md:187-191, 201-203; lessons.md:120-123 | Null sink "created" but invisible to the human session; total silence. |

### PulseAudio compatibility shims

| ID | Subsystem | Observed behaviour / workaround | Why it exists | Source (file:line / commit) | Risk if dropped |
|----|-----------|----------------------------------|---------------|-----------------------------|------------------|
| AUD-033 | "already loaded" strings | When checking `pactl load-module` results, treat **both** `"already loaded"` (PulseAudio) **and** `"Entity exists"` (pipewire-pulse) as success. | pipewire-pulse returns a different "already loaded" string than real PulseAudio; checking only one treats a benign re-load as an error. | `airplay.rs:318-323`; lessons.md:145-147 | RAOP/module reloads spuriously reported as failures. |
| AUD-034 | `@DEFAULT_MONITOR@` avoided | Capture targets `soundsync-capture.monitor` explicitly, never `@DEFAULT_MONITOR@`. | The default monitor is rarely the BT/EQ source; targeting it captured the wrong thing (silence). | `capture.rs:191-194`; cast_stream.rs:15-45; do-over.md:1226 | Silent capture of the wrong source. |
| AUD-035 | pulseaudio-utils dependency | `pulseaudio-utils` is a distinct required package (provides `pactl`/`parec`), separate from `pipewire-pulse`. | Easy to assume pipewire-pulse provides the CLI tools; it does not. Missing it = no capture, no module management. | install.sh:150; CONTEXT.md:144-147; ALWAYSUPDATE.md:85 | `parec`/`pactl` missing → capture & module mgmt silently disabled. |

### Resampling / encoding / sample rates / latency

| ID | Subsystem | Observed behaviour / workaround | Why it exists | Source (file:line / commit) | Risk if dropped |
|----|-----------|----------------------------------|---------------|-----------------------------|------------------|
| AUD-036 | Canonical PCM format | The internal capture/broadcast format is fixed: **48 kHz, 2ch, f32, 960 samples/ch = 1920 f32 = 7680 bytes per 20ms frame**. Opus/MP3 encoders all assume exactly 1920 interleaved f32. | One frame size shared by capture, Opus (960/ch = 20ms), MP3, spectrum. Mismatches corrupt framing. | `capture.rs:21-25`; `opus_encoder.rs:5-7,31`; `mp3_encoder.rs:4-7,43` | Any change here desyncs every encoder; clicks/garbage/crashes. |
| AUD-037 | parec latency vs quantum | `parec --latency-msec` must exceed PipeWire's quantum. Broadcast-path capture uses **20ms**; the HTTP `parec\|ffmpeg` path uses **50ms**. | A 20ms parec buffer is *smaller* than the default 1024-sample quantum (21.3ms@48k) → underruns that are silent at system level but cause audible gaps. lessons.md prescribes ≥2× quantum (50ms safe for 256–2048). NB: the broadcast path still uses 20ms (see UB-1). | `capture.rs:206`; cast_stream.rs:62,90; lessons.md:87-89 | Underruns / audible gaps, especially on larger quanta. |
| AUD-038 | No artificial pacer | RTP packets are sent **immediately after encoding**; there is deliberately NO `tokio::time::interval` pacer. `parec`'s blocking `read_exact()` of one 20ms frame *is* the clock. | A second pacer is an unsynchronized clock that drifts vs parec → packets bunch/gap → browser jitter buffer hears stuttering. Removing the pacer fixed stuttering. | `webrtc_audio.rs:201-209,281-283`; commit `7f19fc8`; lessons.md:111-114 | Reintroduces the exact stuttering the v2.8.4 fix removed. |
| AUD-039 | Broadcast capacity 256 | Capture broadcast channel capacity is **256 frames (~5.12s)**, not 64. | Capacity 64 (1.28s) makes ALL subscribers `Lagged` when ANY one (spectrum FFT / HTTP encoder / briefly-stalled WebRTC) falls behind ~1s → frame drops & stutter for everyone. Cost ~1.8MB. | `capture.rs:29-34`; lessons.md:83-85 | Cascade lag: one slow consumer stutters all streams even at low CPU. |
| AUD-040 | RTP ts advance on lag | On `broadcast Lagged(n)`, advance RTP timestamp by `n*960` and sequence by `n` before continuing. | Otherwise timestamps fall behind wall-clock; the browser jitter buffer "catches up" by playing fast → stutter. | `webrtc_audio.rs:241-261`; lessons.md:75-77 | Post-lag fast-playback stuttering. |
| AUD-041 | Encode off the async runtime | Opus encode runs in `spawn_blocking` with the encoder behind `Arc<Mutex>`. | Opus encode is CPU-bound (~0.5–2ms/frame); running it inline on a Tokio worker stalls all other async work (other sessions, WS, HTTP) → cascading latency. | `webrtc_audio.rs:263-279`; lessons.md:79-81 | Per-frame CPU spikes stall the whole runtime; multi-client stutter. |
| AUD-042 | HTTP stream = direct OS pipes | `/api/stream/audio.{aac,mp3}` spawn a per-client `sh -c "parec … \| ffmpeg …"`; they explicitly do NOT use the broadcast channel. AAC-LC 256k ADTS (`-flush_packets 1`, `-fflags +nobuffer`), MP3 fallback 192k. `kill_on_drop(true)` cleans up on client disconnect. | The shared `parec→broadcast→consumer` path stuttered identically on AAC/MP3/WebRTC, proving the broadcast forwarding was the culprit. Kernel pipe buffering is far more reliable for real-time than userspace async forwarding. (Reference project BluetoothA2DP works this way.) | `cast_stream.rs:47-151`; commit `f89748c`; lessons.md:105-114 | HTTP streams stutter; reintroduces the v2.8.5 regression. |
| AUD-043 | AAC params | A2DP-advertised AAC and the FFmpeg encoder are both **AAC-LC, 256 kbps, 48 kHz stereo**; encoder adds `-profile:a aac_low -cutoff 20000`, ADTS container. | AAC-LC/ADTS is the most universally compatible streaming format across Safari/Chrome/Chromecast/AirPlay. | `aac_encoder.rs:29-55`; cast_stream.rs:60-68; codecs/aac.rs (256000 blob) | Chromecast/Safari compatibility breaks; wrong container = no playback. |
| AUD-044 | MP3 fallback bitrates | Standalone MP3 encoder = 256k CBR; HTTP MP3 fallback path = 192k. Chromecast streaming = AAC URL with MP3 fallback. | CBR + flush-per-frame keeps Chromecast latency bounded; bitrates chosen empirically. (Two different MP3 bitrates is a latent inconsistency — see UB-2.) | `mp3_encoder.rs:4,25`; cast_stream.rs:94; plan:166 | Chromecast latency/quality regressions. |
| AUD-045 | Opus bitrate 128k | WebRTC Opus fixed at **128 kbps**, `Application::Audio`, 48k stereo, encode buffer 4000 bytes (> max ~1275). | 128k Opus is the WebRTC quality target; Application::Audio (not VoIP) for music. | `opus_encoder.rs:15-24`; do-over.md:608-616 | Lower quality or buffer-too-small encode failures. |
| AUD-046 | Spectrum rate/shape | Spectrum FFT uses **48000 Hz** (matching capture), FFT 2048, 64 log bands (20Hz–20kHz), Hanning window, EMA α=0.35, 0.0=−80dBFS…1.0=0dBFS. Stereo→mono by averaging. | Must match the actual 48k capture rate. NB: do-over.md spec says 44100 — the code uses 48000 (the spec is wrong; see UB-3). | `spectrum.rs:9-14,97`; conflicts with do-over.md:771-781 | Wrong band frequencies if a rebuild trusts the 44.1k spec. |

### WebRTC media path & signalling

| ID | Subsystem | Observed behaviour / workaround | Why it exists | Source (file:line / commit) | Risk if dropped |
|----|-----------|----------------------------------|---------------|-----------------------------|------------------|
| AUD-047 | RTP payload type 111 | Opus track sent with RTP **payload type 111**, `mime "audio/opus"`, clock 48000, channels 2, RTP header version 2. | 111 is the conventional dynamic PT browsers negotiate for Opus. | `webrtc_audio.rs:157-166,284-291` | PT mismatch → browser ignores the track; silent. |
| AUD-048 | sdpMLineIndex casing | ICE candidate JSON keys must be exactly `sdpMid` and `sdpMLineIndex` — set via explicit `#[serde(rename=...)]`, NOT `rename_all="camelCase"` (which yields `sdpMlineIndex`, lowercase l). | Safari strictly requires at least one of `sdpMid`/`sdpMLineIndex` non-null on every candidate; the wrong key means Safari never reads it and drops valid candidates → connection fails. Chrome is lenient. | `ws.rs:384-395,425-437`; lessons.md:70-73; commit `d7454b9` | Safari WebRTC silently fails entirely. |
| AUD-049 | ICE candidate queue (client) | Browser buffers ICE candidates until `setRemoteDescription` resolves, then flushes; `handleAnswer` is awaited before candidates are applied. | Safari throws `InvalidStateError` and drops candidates if `addIceCandidate` runs before `setRemoteDescription` resolves; Chrome/FF silently queue. | webrtc.ts:11-15,101-142; lessons.md:133-135; commit `7d3d098` | Safari ICE negotiation breaks. |
| AUD-050 | Drop null-mid candidates | Client drops candidates where both `sdpMid` and `sdpMLineIndex` are null (end-of-candidates / malformed). Also normalizes snake_case↔camelCase from the server. | Safari rejects such candidates; they're not real candidates. | webrtc.ts:116-128 | Safari throws on end-of-candidate markers. |
| AUD-051 | ontrack uses event.track | Client builds the stream from `event.track` (`new MediaStream([event.track])`) because `event.streams[0]` is undefined. | webrtc-rs `add_track()` doesn't associate the track with a stream, so the browser gets no stream id. | webrtc.ts:54-59 | No audio element source; silent playback. |
| AUD-052 | Offer/answer direction | Browser is the offerer (`addTransceiver('audio',{direction:'recvonly'})`); server answers and pushes a sendonly Opus track. STUN `stun.l.google.com:19302`. One PeerConnection per browser. | Pure-Rust `webrtc` crate; signalling over `/ws/status`. | `webrtc_audio.rs:141-199`; webrtc.ts:90-99; do-over.md:618-625 | Negotiation direction mismatch → no media. |
| AUD-053 | WebRTC session = WS session id | Each WebRTC session is keyed by the WebSocket connection's UUID; WebRTC answer/ICE events are filtered to only the matching `ws_session_id`; on WS disconnect the WebRTC session is torn down. | Multiple browser clients share one event broadcast; without session filtering, clients would cross-receive each other's SDP/ICE. | `ws.rs:20,82-89,197-214`; `webrtc_audio.rs:357-365` | SDP/ICE cross-talk between clients; sessions leak on disconnect. |
| AUD-054 | UDP buffer sizes | Tuning script raises `net.core.wmem_max/rmem_max` to 2MB (persisted in `/etc/sysctl.d/99-soundsync-network.conf`). | Small UDP socket buffers drop WebRTC packets → audio stuttering. | fix-audio-scheduling.sh:168-192; audio-stutter-debug.sh:204-216 | Packet drops under load → WebRTC stutter. |

### RT scheduling (affects every audio path)

| ID | Subsystem | Observed behaviour / workaround | Why it exists | Source (file:line / commit) | Risk if dropped |
|----|-----------|----------------------------------|---------------|-----------------------------|------------------|
| AUD-055 | RT limits — two places | Set RT limits in BOTH `/etc/security/limits.d/99-soundsync-rt.conf` (rtprio 95, memlock unlimited, nice -15) AND `/etc/systemd/user.conf` `DefaultLimitRTPRIO=95` / `DefaultLimitMEMLOCK=infinity`, plus `LimitRTPRIO=95` on the soundsync service. | `limits.d` only applies to PAM login sessions; systemd `--user` services (PipeWire/WP) do NOT go through PAM and never see it — so PipeWire stays SCHED_OTHER despite `ulimit -r` showing 95 under `sudo -u`. | fix-audio-scheduling.sh:49-128; lessons.md:95-98,120-123 | PipeWire at SCHED_OTHER → preemption at 20ms boundaries → stutter at low CPU. |
| AUD-056 | CPU governor / rtkit | Tuning sets CPU governor to `performance` and ensures `rtkit-daemon` running. | `powersave`/`schedutil` cause frequency-scaling latency spikes; rtkit is what lets PipeWire escalate to RT. | fix-audio-scheduling.sh:75-89,141-166 | Frequency-scaling jitter; RT escalation impossible. |
| AUD-057 | Zombie reaping | Tokio `Child` killed in synchronous `Drop` is reaped by spawning `child.wait()`; capture/filter-chain/encoders use `kill_on_drop(true)`. | `Drop` can't `await`; killed-but-unreaped async children become zombies that accumulate across restarts. | `capture.rs:136-147`; `filter_chain.rs:122-135`; lessons.md:125-127 | Zombie pileup across restarts; eventual fd/PID exhaustion. |

### AirPlay (PipeWire RAOP)

| ID | Subsystem | Observed behaviour / workaround | Why it exists | Source (file:line / commit) | Risk if dropped |
|----|-----------|----------------------------------|---------------|-----------------------------|------------------|
| AUD-058 | RAOP via PipeWire | AirPlay uses PipeWire's `module-raop-discover`; SoundSync links the null-sink monitor to the RAOP sink via `pw-link`. No Rust AirPlay/RAOP protocol, no SSL/FairPlay. | RAOP sinks get EQ-processed audio natively through the PipeWire graph; matches the subprocess pattern. | `airplay.rs:43-58,303-343`; docs/airplay-chromecast-integration-plan.md:7,15,65-66 | Re-implementing RAOP from scratch is huge wasted effort and likely buggier. |
| AUD-059 | Wake SUSPENDED sink | Before linking, `pactl suspend-sink <raop> 0` then poll `pw-link -i` up to 2s (10×200ms) for ports to appear. | SUSPENDED RAOP sinks expose **zero** ports to `pw-link -i`; linking fails "No playback ports". They appear in `pactl list short sinks` but are invisible to the linker until woken. | `airplay.rs:194-201,605-643`; lessons.md:141-143; commit `86e9f56` | AirPlay link fails with a misleading "no ports" error. |
| AUD-060 | Discover ports at runtime | `pw-link` source/target port names are discovered at runtime (`pw-link -o/-i`, fuzzy match dots↔underscores, FL/FR pairing with positional fallback), never constructed from sink names. | PipeWire port names differ from PulseAudio sink names (dots/underscores/hostname parts); constructed names fail "No such file". | `airplay.rs:763-906` | Channel links fail; mono/no audio to AirPlay. |
| AUD-061 | Avahi escape decode + IPv6 | `avahi-browse -p` output `\NNN` decimal escapes are decoded; RAOP names `MAC@Friendly#` stripped to friendly part; **IPv6 entries skipped BEFORE dedup**. | Parseable output escapes spaces/`@`/`#`. If an IPv6 entry claims the name in the dedup set first, the IPv4 entry is dropped and the device is lost entirely. | `airplay.rs:438-477,527-592`; chromecast.rs:324-340 | Device names mangled; devices silently disappear (IPv6-first). |
| AUD-062 | RAOP availability + Avahi | On startup check for `libpipewire-module-raop-sink*` (multi-arch paths) and `avahi-daemon` active; degrade gracefully with a warning if absent. | Not all distros ship the RAOP module; Avahi owns 5353 and is needed for discovery. | `airplay.rs:60-81,346-393`; plan:167 | Hard failure / crash instead of graceful "AirPlay unavailable". |

### Chromecast (CASTV2 + HTTP pull)

| ID | Subsystem | Observed behaviour / workaround | Why it exists | Source (file:line / commit) | Risk if dropped |
|----|-----------|----------------------------------|---------------|-----------------------------|------------------|
| AUD-063 | HTTP-pull, no SSL | Chromecast is told to load `http://<local-ip>:<port>/api/stream/audio.aac` (the per-client parec\|ffmpeg pipe). DefaultMediaReceiver app `CC1AD845`, `StreamType::Live`, content-type `audio/aac`. | Chromecast pulls plain-HTTP media (no certs needed); reuses the existing HTTP stream so all audio stays in one pipeline. | `chromecast.rs:19-22,148-153,548-593`; cast_stream.rs:55-81; plan:7,42 | Re-introducing SSL/transcoding complexity; or wrong content-type → no playback. |
| AUD-064 | Local IP detection | `detect_local_ip()` = first `hostname -I` token, fallback UDP-connect-to-8.8.8.8 socket trick, fallback 127.0.0.1. | The cast device must reach the server by a routable IP, not localhost. | cast_stream.rs:153-178 | Chromecast can't fetch the stream (gets 127.0.0.1). |
| AUD-065 | rust_cast on blocking threads | All `rust_cast` calls run in `spawn_blocking` (synchronous TCP I/O); a 5s heartbeat loop reconnects each ping; after **3** consecutive heartbeat failures the session is declared lost. | rust_cast is blocking; must not run on async workers. CASTV2 needs periodic pings or the receiver drops the connection. | `chromecast.rs:157-159,595-638` | Async runtime stalls; cast sessions silently die without heartbeat. |
| AUD-066 | Discovery: avahi then mdns-sd | Chromecast discovery tries `avahi-browse` first (avahi-daemon owns 5353 on Linux), falls back to the `mdns-sd` library; mdns-sd runs entirely in one `spawn_blocking` (its flume receiver can't be cloned across spawns). | On Linux, binding 5353 conflicts with avahi-daemon; library fallback for non-avahi systems. | `chromecast.rs:102-122,381-489` | Discovery fails on systems with avahi (port conflict) or without it. |

### Frontend / browser

| ID | Subsystem | Observed behaviour / workaround | Why it exists | Source (file:line / commit) | Risk if dropped |
|----|-----------|----------------------------------|---------------|-----------------------------|------------------|
| AUD-067 | Safari gesture priming | On the user click, create the `<audio>` element and call `.play()` **synchronously before any `await`**; only then `await AudioContext.resume()`. | Safari autoplay: the first `await` breaks the synchronous user-gesture chain; later `.play()` calls are blocked as non-user-initiated. Pre-priming inside the gesture is the only reliable unlock. | webrtc.ts:22-48; lessons.md:137-139 | Safari blocks playback with `NotAllowedError`; no audio. |
| AUD-068 | Muted-toggle unlock | On `ontrack`, if `.play()` rejects, set `muted=true`, `.play()`, then unmute. | Safari fallback unlock trick when the direct play is blocked. | webrtc.ts:61-70 | Safari audio stays silent after connect. |
| AUD-069 | playsinline | `<audio>` gets `playsinline` and `autoplay`. | iOS Safari requires `playsinline` to avoid fullscreen takeover / play blocking. | webrtc.ts:28-30 | iOS playback quirks / blocked autoplay. |
| AUD-070 | WS reconnect backoff | WebSocket auto-reconnects with exponential backoff 1s→30s plus up-to-1s random jitter; resets to 1s on open. | Survive transient drops without hammering; jitter avoids thundering-herd reconnect. | websocket.ts:5-58 | Reconnect storms or no recovery after a drop; WebRTC signalling channel dies. |
| AUD-071 | WS dual-purpose | A single `/ws/status` carries BOTH state/spectrum events AND WebRTC signalling (`webrtc_offer/ice_candidate/stop`). Serialization tag = `type`, content = `data`, `rename_all="snake_case"` for events but explicit renames for ICE keys (AUD-048). | One socket avoids a second connection; but the WebRTC session lifecycle is tied to the WS lifecycle (AUD-053). | `ws.rs:13-15,94-142,287-289`; do-over.md:588-603 | Splitting them naively breaks session correlation and teardown. |
| AUD-072 | Spectrum cadence/shape | Spectrum events: 64 floats 0.0–1.0; consumer lag is tolerated (`Lagged` just warns, doesn't kill the analyzer). | Visualization is non-timing-critical; must never block or crash the audio path. | `spectrum.rs:44-76`; do-over.md:749-753 | A stalled visualizer could otherwise wedge the capture broadcast. |
| AUD-073 | Stream-started name guard | `StreamStarted`/`AudioActive` WS messages send an **empty** name; the frontend must NOT overwrite a known device name with empty. | Backend doesn't always have the friendly name at AudioActive time; empty-overwrite would erase resolved names in the UI. | `ws.rs:215-224`; `discovery::update_device_name` guard | Device names flicker to blank/MAC when audio starts. |

### Line-in

| ID | Subsystem | Observed behaviour / workaround | Why it exists | Source (file:line / commit) | Risk if dropped |
|----|-----------|----------------------------------|---------------|-----------------------------|------------------|
| AUD-074 | Line-in loopback | Line-in = `pactl load-module module-loopback source=<alsa_input.*> sink=soundsync-capture latency_msec=20 source_dont_move=true sink_dont_move=true`. Source detected as the first `alsa_input.*`. Orphaned loopbacks targeting the null sink are cleaned up on init. | Routes analog input through the SAME null sink so it gets EQ/spectrum/streaming like BT. `*_dont_move=true` pins routing so WP doesn't relocate it. | `line_in.rs:8,65-119,184-233,270-278` | Line-in bypasses EQ/streaming, or module pileup across restarts. |

---

## Unexplained behaviours (observed but not fully root-caused)

- **UB-1 — parec latency 20ms on the broadcast path contradicts the lessons rule.**
  `capture.rs:206` uses `--latency-msec=20`, but lessons.md:87-89 explicitly says parec
  latency must exceed the PipeWire quantum (default 21.3ms@48k) and prescribes 50ms; the
  HTTP path (`cast_stream.rs`) correctly uses 50ms. This looks like an un-applied fix on
  the broadcast capture path. *To confirm:* check `pw-metadata -n settings` for the actual
  quantum on target hardware and whether the broadcast path underruns; git-blame
  `capture.rs:206` to see if the 50ms lesson postdates it.

- **UB-2 — two different MP3 bitrates, and `mp3_encoder` may be dead.** `mp3_encoder.rs` is
  256k CBR but the live HTTP MP3 fallback (`cast_stream.rs:94`) is 192k via the shell pipe.
  The standalone `Mp3Encoder` doesn't appear to be on any live path. *To confirm:* grep for
  callers of `Mp3Encoder` (and `AacEncoder`) — these struct-based encoders are likely legacy,
  superseded by the OS-pipe approach (AUD-042). If dead, the rebuild should drop them rather
  than reconcile the bitrates.

- **UB-3 — spectrum sample-rate spec mismatch.** Code uses 48000 Hz (`spectrum.rs:10`); the
  rebuild spec do-over.md:771-781 says 44100 Hz with `parec --rate=44100`. The code is
  authoritative (capture is 48k); the spec is stale. The rebuild MUST follow the code (48k),
  not the do-over spec, or band frequencies skew.

- **UB-4 — AVRCP artwork extraction is largely best-effort/non-functional.**
  `try_extract_artwork` only handles an `Image` *file path* in the Track dict; most BlueZ
  builds expose cover art via `MediaItem1`/Browsing (OBEX BIP), which isn't implemented.
  *To confirm:* test against a real iOS/Android source; artwork likely never populates in
  practice. A rebuild should treat AVRCP artwork as unimplemented, not "working".

- **UB-5 — `bluez5.roles` differs between WP 0.5 and 0.4 configs.** The 0.5 `.conf`
  advertises the full role set (`a2dp_sink a2dp_source hfp_hf hfp_ag hsp_hs hsp_ag`) while
  both 0.4 Lua configs advertise only `[ a2dp_sink ]`. Whether the narrower 0.4 set is
  intentional (avoid HFP mic capture) or an oversight is undocumented. *To confirm:*
  git-blame the install.sh role lines / look for an HFP-related report; decide deliberately
  in the rebuild rather than copying inconsistently.

- **UB-6 — `effect_output.soundsync-eq → soundsync-capture` link is never verified.** The
  design says "let WirePlumber create links" (do-over.md:442-445) and the filter-chain sets
  `node.target="soundsync-capture"`, but no code asserts the link actually formed. Combined
  with the 200ms EQ-restart dropout (AUD-024), this is the highest-impact recurring-bug zone
  (CONTEXT.md:125-137). *To confirm:* after an EQ toggle, assert via `pw-link -l` that the
  link exists; the legacy code never does, so a missing link is silent.

- **UB-7 — `pw-loopback` null-sink fallback produces a `Audio/Source/Virtual`, not a true
  monitor.** When pactl is absent, the fallback (`pipeline.rs:251-278`) creates a
  sink+`*-source` pair; downstream code still appends `.monitor` to the *sink* name for
  parec (`capture.rs:193`). Whether the `.monitor` exists on a pw-loopback sink is unverified.
  *To confirm:* on a pactl-less system, check `pactl/pw-cli` for `soundsync-capture.monitor`
  vs `soundsync-capture-source`; the capture target may be wrong in the fallback path.

---

## Addendum — live findings on Ubuntu 26.04 / bluez 5.85 (2026-06-12)

> Captured while bringing the rebuild up on a clean 26.04 box. See the full
> capture in `clean-install-2604-runbook.md`. This extends AUD-002.

- **AUD-002b — main.conf `Class` is IGNORED on bluez 5.85; CoD must be re-pinned
  REACTIVELY, not once.** Verified by test: `[General] Class = 0x240414` (and even a
  clean major/minor `0x000414`) is not applied — after a `bluetooth` restart with
  PipeWire stopped the adapter stays `0x00010c` (Computer/Laptop). `hciconfig hci0
  class 0x240414` *does* set it, but **`bluetoothd` recomputes the adapter class as
  `kernel-default-major (Computer) | service-bits` on every BlueZ media-endpoint
  (re)registration.** WirePlumber registers its A2DP/HFP endpoints at startup **and on
  every device connect / card creation**, so a boot-time hciconfig oneshot is wiped
  seconds later (proven: assert CoD → `systemctl --user restart wireplumber` → reverts
  to `0x6c010c` every time). Because A2DP **source** devices filter on CoD (AUD-002),
  the adapter then "won't appear as a speaker" and audio plays locally — no `bluez_card`,
  no transport. The unprivileged user daemon can't fix it (no `CAP_NET_ADMIN`).
  **Fix (shipped in packaging):** root `soundsync-adapter-cod.service` runs
  `soundsync-cod-watch.sh`, which `dbus-monitor`s BlueZ `PropertiesChanged`/`InterfacesAdded`
  and re-pins `0x240414` on drift (writes only when drifted; 30s backstop;
  `Restart=always`, `WantedBy=bluetooth.target`). This is the ONLY approach that keeps
  the speaker CoD stable for every source device across connects and WP churn. Requires
  `dbus-bin` (provides `dbus-monitor`).

- **AUD-013b — the WP 0.5 role set is unchanged and correct.** On 26.04 the rebuild
  writes `bluez5.roles = [ a2dp_sink a2dp_source hfp_hf hfp_ag hsp_hs hsp_ag ]` to
  `/etc/wireplumber/wireplumber.conf.d/51-soundsync.conf`, matching the legacy 0.5
  install (AUD-013). Confirmed the adapter then advertises **Audio Sink (0x110b)**, so
  the SPA bluez5 A2DP-sink endpoint IS registered (libspa-0.2-bluetooth present). The
  roles are NOT the cause of the "no speaker" symptom — CoD (AUD-002b) is. (UB-5 remains
  open but is not implicated here.)

- **AUD-017b — WP 0.5 SPA-JSON path confirmed on 0.5.13.** `capabilities::version::
  config_format()` returns `SpaJson` for `(0,5)`, `apply-wireplumber-config` writes the
  `.conf` (with `monitor.bluez.seat-monitoring = disabled`, AUD-004) and cleans up the
  stale Lua file. NUANCE: run as root it prints `wireplumber restarted: false` — it cannot
  restart the *user-session* WirePlumber; the installer now does that via
  `sudo -u <user> … systemctl --user restart wireplumber`.

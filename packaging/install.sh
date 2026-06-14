#!/usr/bin/env bash
# SoundSync installer + maintenance tool — run as root (via sudo); idempotent.
#
# Run with --help for usage. Three modes: (default) install, --doctor, --reset.
set -euo pipefail

BINARY_DEST=/usr/bin/soundsync
WEBUI_DEST=/usr/share/soundsync/webui
USER_UNIT_DEST=/usr/lib/systemd/user/soundsync.service
SYSTEM_UNIT_DEST=/usr/lib/systemd/system/soundsync-adapter.service
COD_UNIT_DEST=/usr/lib/systemd/system/soundsync-adapter-cod.service
COD_SCRIPT_DEST=/usr/lib/soundsync/soundsync-cod-watch.sh
WP_CONF=/etc/wireplumber/wireplumber.conf.d/51-soundsync.conf
BIND_PORT=8080

MODE=install
NO_BUILD=0
NO_DEPS=0
FROM_DIR=""

usage() {
    cat <<'EOF'
SoundSync installer / maintenance tool — run as root (via sudo).

USAGE:
  sudo packaging/install.sh [MODE] [OPTIONS]

MODES (default: install):
  (none)        Install: apt deps -> build -> install binary/webui/units,
                apply the WirePlumber A2DP config, enable services + linger.
  --doctor      Diagnose the running system and apply safe, idempotent repairs
                (start/enable services, re-apply WP config, enable linger,
                re-pin the adapter Class-of-Device). Reports OK / WARN / FAIL.
  --reset       Nuclear reset: stop the pipeline, clear PipeWire/WirePlumber
                user state + cache, re-apply config, and restart everything in
                the correct order. Use when audio is wedged. Non-destructive to
                pairings and installed files.
  -h, --help    Show this help.

INSTALL OPTIONS:
  --no-build    Skip the Rust + SPA build; use prebuilt artifacts in place.
  --no-deps     Skip the apt dependency-install step.
  --from <dir>  Install prebuilt artifacts from <dir> (implies --no-build).

ENVIRONMENT:
  SOUNDSYNC_NODE_BIN   Path to a node/npm bin dir for the SPA build.

NOTES:
  * --doctor and --reset must be run via sudo as the appliance user, so they can
    reach that user's PipeWire session:  sudo packaging/install.sh --doctor
EOF
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --doctor)   MODE=doctor; shift ;;
        --reset)    MODE=reset;  shift ;;
        --no-build) NO_BUILD=1;  shift ;;
        --no-deps)  NO_DEPS=1;   shift ;;
        --from)
            if [[ $# -lt 2 ]]; then
                echo "error: --from requires a directory argument" >&2
                exit 1
            fi
            FROM_DIR="$2"; NO_BUILD=1; shift 2 ;;
        -h|--help)  usage; exit 0 ;;
        *)
            echo "error: unknown argument: $1" >&2
            echo "run: $0 --help" >&2
            exit 1 ;;
    esac
done

# ---------------------------------------------------------------------------
# Root check (all modes need root: apt/install, hciconfig, system services)
# ---------------------------------------------------------------------------
if [[ $EUID -ne 0 ]]; then
    echo "error: this must be run as root — use: sudo $0 $*" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "$SCRIPT_DIR")"

# N11: systemd is a hard requirement — every mode shells out to systemctl/
# loginctl. Fail with a clear message rather than a raw error at the first call.
command -v systemctl >/dev/null || { echo 'error: SoundSync requires systemd.' >&2; exit 1; }

# ---------------------------------------------------------------------------
# G16 + N16: read appliance config from the deb conffile if present.
# We do NOT `source` it (that would let arbitrary assignments leak into the
# script env); instead parse the four keys we honor. These influence the
# adapter (SOUNDSYNC_HCI), the CoD comparison in doctor/reset (SOUNDSYNC_COD),
# the bind port (SOUNDSYNC_BIND), and the appliance user (SOUNDSYNC_USER,
# preferred over SUDO_USER below). Pre-set env wins over the conffile.
# ---------------------------------------------------------------------------
CONFFILE=/etc/default/soundsync
_conf_get() {
    # Print the value of KEY=... from $CONFFILE (last assignment wins),
    # ignoring commented lines and stripping optional surrounding quotes.
    [[ -r "$CONFFILE" ]] || return 0
    sed -n "s/^[[:space:]]*$1=//p" "$CONFFILE" 2>/dev/null \
        | tail -n1 | sed -e 's/^"\(.*\)"$/\1/' -e "s/^'\(.*\)'$/\1/"
}
if [[ -r "$CONFFILE" ]]; then
    : "${SOUNDSYNC_USER:=$(_conf_get SOUNDSYNC_USER)}"
    : "${SOUNDSYNC_HCI:=$(_conf_get SOUNDSYNC_HCI)}"
    : "${SOUNDSYNC_COD:=$(_conf_get SOUNDSYNC_COD)}"
    : "${SOUNDSYNC_BIND:=$(_conf_get SOUNDSYNC_BIND)}"
fi
# Honor SOUNDSYNC_HCI from the conffile in detect_hci (it reads the env).
export SOUNDSYNC_HCI="${SOUNDSYNC_HCI:-}"
# CoD to compare against (doctor/reset); default matches the CoD watcher.
WANT_COD="${SOUNDSYNC_COD:-0x240414}"
# Bind port: SOUNDSYNC_BIND may be "host:port" or bare "port"; take the port.
if [[ -n "${SOUNDSYNC_BIND:-}" ]]; then
    BIND_PORT="${SOUNDSYNC_BIND##*:}"
fi

# ---------------------------------------------------------------------------
# Target user + helpers to act inside that user's PipeWire/D-Bus session.
# The daemon is a systemd *user* service, so anything touching pipewire/
# wireplumber/pactl/the user soundsync.service must run AS the user, not root.
# ---------------------------------------------------------------------------
# G5 + G16/N16: resolve the appliance user. Precedence:
#   1. SOUNDSYNC_USER (env, or read from the conffile above)
#   2. SUDO_USER (the human who ran `sudo install.sh`)
#   3. auto-detect the SOLE human login account, the SAME way debian/postinst
#      does (exactly one passwd uid in [1000,65534) with a real login shell).
# This makes a raw-root invocation (su -, CI, cloud-init) target the real
# appliance user instead of silently producing a dead appliance.
TARGET_USER="${SOUNDSYNC_USER:-${SUDO_USER:-}}"
if [[ -z "$TARGET_USER" || "$TARGET_USER" == "root" ]]; then
    _candidates="$(
        getent passwd 2>/dev/null | awk -F: '
            $3 >= 1000 && $3 < 65534 &&
            $7 != "/usr/sbin/nologin" && $7 != "/sbin/nologin" &&
            $7 != "/bin/false" && $7 != "/usr/bin/false" && $7 != "" \
            { print $1 }
        '
    )" || true
    _ncand="$(printf '%s\n' "$_candidates" | grep -c . || true)"
    if [[ "$_ncand" -eq 1 ]]; then
        TARGET_USER="$_candidates"
        echo "info: auto-detected appliance user '${TARGET_USER}' (sole human account)."
    else
        # Zero or several candidates: do NOT guess. Warn loudly so we never
        # report "installed successfully" with a dead, user-less appliance.
        echo "warn: could not determine the appliance user (${_ncand} candidate(s))." >&2
        echo "      The user daemon, linger and WirePlumber restart will be SKIPPED." >&2
        echo "      Set the appliance user and re-run, e.g.:" >&2
        echo "        sudo SOUNDSYNC_USER=<user> $0" >&2
        echo "        (or add 'SOUNDSYNC_USER=<user>' to ${CONFFILE})" >&2
        TARGET_USER=""
    fi
fi

TARGET_UID=""
TARGET_HOME=""
if [[ -n "$TARGET_USER" ]]; then
    TARGET_UID="$(id -u "$TARGET_USER" 2>/dev/null || true)"
    TARGET_HOME="$(getent passwd "$TARGET_USER" 2>/dev/null | cut -d: -f6)"
fi

run_user() {
    # Run a command inside the target user's session.
    sudo -u "$TARGET_USER" \
        XDG_RUNTIME_DIR="/run/user/${TARGET_UID}" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/${TARGET_UID}/bus" \
        "$@"
}

require_target_user() {
    if [[ -z "$TARGET_USER" || "$TARGET_USER" == "root" || -z "$TARGET_UID" ]]; then
        echo "error: --$1 must be run via sudo as the appliance user (to reach its" >&2
        echo "       PipeWire session):  sudo $0 --$1" >&2
        exit 1
    fi
    if [[ ! -d "/run/user/${TARGET_UID}" ]]; then
        echo "error: ${TARGET_USER}'s session (/run/user/${TARGET_UID}) is not active." >&2
        echo "       Log in once, or enable linger:  sudo loginctl enable-linger ${TARGET_USER}" >&2
        exit 1
    fi
}

# Resolve the Bluetooth adapter index (no hardcoded hci0): SOUNDSYNC_HCI if set,
# else the first /sys/class/bluetooth/hci*, else hci0.
detect_hci() {
    local h="${SOUNDSYNC_HCI:-}"
    if [[ -z "$h" ]]; then
        h="$(ls -1 /sys/class/bluetooth/ 2>/dev/null | grep -m1 '^hci' || echo hci0)"
    fi
    printf '%s' "$h"
}

# ===========================================================================
# Mode: --doctor  (diagnose + safe idempotent repair)
# ===========================================================================
do_doctor() {
    require_target_user doctor
    set +e
    local fails=0 warns=0
    local G='\033[0;32m' Y='\033[1;33m' R='\033[0;31m' N='\033[0m'
    ok(){ printf "  ${G}[OK]${N}   %s\n" "$1"; }
    wn(){ printf "  ${Y}[WARN]${N} %s\n" "$1"; warns=$((warns+1)); }
    fl(){ printf "  ${R}[FAIL]${N} %s\n" "$1"; fails=$((fails+1)); }

    echo "==> SoundSync doctor (target user: ${TARGET_USER})"

    # -- install integrity (G9): a partial/broken install must FAIL loudly, not
    #    let the "repairs" below silently no-op on missing files. --
    [[ -x "$BINARY_DEST" ]] && ok "binary installed (${BINARY_DEST})" \
        || fl "binary missing/not executable: ${BINARY_DEST}  (run: sudo $0)"
    [[ -f "$WEBUI_DEST/index.html" ]] && ok "webui installed (${WEBUI_DEST}/index.html)" \
        || fl "webui missing: ${WEBUI_DEST}/index.html  (run: sudo $0)"
    local u; for u in "$USER_UNIT_DEST" "$SYSTEM_UNIT_DEST" "$COD_UNIT_DEST"; do
        [[ -f "$u" ]] && ok "unit installed ($(basename "$u"))" \
            || fl "unit file missing: $u  (run: sudo $0)"
    done
    [[ -x "$COD_SCRIPT_DEST" ]] && ok "CoD watcher script installed (${COD_SCRIPT_DEST})" \
        || fl "CoD watcher script missing/not executable: ${COD_SCRIPT_DEST}  (run: sudo $0)"

    # -- binaries --
    local miss=() b
    for b in soundsync pactl wpctl hciconfig bluetoothctl dbus-monitor ffmpeg; do
        command -v "$b" >/dev/null 2>&1 || miss+=("$b")
    done
    [[ ${#miss[@]} -eq 0 ]] && ok "required binaries present" \
        || fl "missing binaries: ${miss[*]}  (run: sudo $0  to install deps)"

    find /usr/lib -path '*/spa-0.2/bluez5' -type d 2>/dev/null | grep -q . \
        && ok "libspa-0.2-bluetooth present" || fl "libspa-0.2-bluetooth MISSING — no A2DP possible"

    # -- system services (repair: start/enable) --
    if systemctl is-active --quiet bluetooth; then ok "bluetooth.service active"
    else fl "bluetooth.service inactive — starting"; systemctl start bluetooth; fi

    if systemctl is-active --quiet soundsync-adapter-cod.service; then ok "CoD watcher (soundsync-adapter-cod) active"
    else fl "CoD watcher inactive — enabling"; systemctl enable --now soundsync-adapter-cod.service 2>/dev/null; fi

    # -- user-session services (repair: start) --
    local s
    for s in pipewire wireplumber pipewire-pulse; do
        if run_user systemctl --user is-active --quiet "$s"; then ok "user $s active"
        else fl "user $s inactive — starting"; run_user systemctl --user start "$s"; fi
    done
    if run_user systemctl --user is-active --quiet soundsync.service; then ok "soundsync (user) active"
    else wn "soundsync (user) inactive — starting"; run_user systemctl --user start soundsync.service; fi

    # -- linger --
    if loginctl show-user "$TARGET_USER" 2>/dev/null | grep -q 'Linger=yes'; then ok "linger enabled (headless autostart)"
    else wn "linger not enabled — enabling"; loginctl enable-linger "$TARGET_USER"; fi

    # -- WP A2DP config (the headless A2DP-sink fix) --
    # G10: validate CONTENT, not just presence — assert seat-monitoring is
    # disabled AND the A2DP sink role and codecs lines are present, so a config
    # with a2dp_sink dropped/replaced or a missing codecs line does not pass.
    if [[ -f "$WP_CONF" ]] \
        && grep -q 'seat-monitoring = disabled' "$WP_CONF" \
        && grep -q 'a2dp_sink' "$WP_CONF" \
        && grep -qi 'codec' "$WP_CONF"; then
        ok "WP A2DP config present (seat-monitoring disabled, a2dp_sink role + codecs)"
    else
        fl "WP A2DP config missing/incomplete — rewriting + reloading WP"
        # G9: do NOT hide failures of the rewrite; report them.
        if "$BINARY_DEST" apply-wireplumber-config; then
            ok "re-applied WP A2DP config"
        else
            fl "apply-wireplumber-config failed (is wireplumber installed?)"
        fi
        run_user systemctl --user restart wireplumber 2>/dev/null || true
    fi

    # -- conflicting audio servers --
    local conf=0 p
    for p in pulseaudio bluealsa bluez-alsa; do
        pgrep -x "$p" >/dev/null && { fl "$p running — conflicts with PipeWire"; conf=1; }
    done
    [[ $conf -eq 0 ]] && ok "no conflicting audio servers (pulseaudio/bluealsa)"

    # -- adapter Class-of-Device + speaker advertisement --
    local cod; cod="$(hciconfig "$(detect_hci)" class 2>/dev/null | grep -oE '0x[0-9a-fA-F]{6}' | head -1)"
    [[ "$cod" == "$WANT_COD" ]] && ok "adapter CoD = ${WANT_COD} (Audio/Video speaker)" \
        || wn "adapter CoD = ${cod:-unknown} — the CoD watcher should re-pin to ${WANT_COD}"
    bluetoothctl show 2>/dev/null | grep -q '0000110b-0000-1000-8000-00805f9b34fb' \
        && ok "adapter advertises Audio Sink 0x110b (speaker is live)" \
        || fl "adapter NOT advertising 0x110b — source devices won't see a speaker"

    # -- capture null sink --
    run_user pactl list short sinks 2>/dev/null | grep -q soundsync-capture \
        && ok "soundsync-capture sink present" \
        || wn "soundsync-capture sink absent (daemon may still be starting)"

    # -- web UI bind/liveness (G17): "active" != "bound". If the user daemon is
    #    active but the port doesn't answer, another process likely holds it
    #    (soundsync then flaps on bind). Reuse the install-time holder detection.
    #    Honors SOUNDSYNC_BIND via BIND_PORT. --
    if run_user systemctl --user is-active --quiet soundsync.service; then
        if run_user curl -s -o /dev/null "http://localhost:${BIND_PORT}/" 2>/dev/null; then
            ok "web UI responding on port ${BIND_PORT}"
        elif ss -tlnp 2>/dev/null | grep ":${BIND_PORT}" | grep -qv 'soundsync'; then
            local holder
            holder="$(ss -tlnp 2>/dev/null | grep ":${BIND_PORT}" | awk '{print $NF}' | head -1 || true)"
            fl "port ${BIND_PORT} held by another process (${holder:-unknown}) — soundsync can't bind"
        else
            wn "web UI not responding on port ${BIND_PORT} (daemon may still be starting)"
        fi
    fi

    # -- latent: WP MemoryDenyWriteExecute (only matters for LDAC/aptX) --
    if run_user systemctl --user cat wireplumber.service 2>/dev/null | grep -q 'MemoryDenyWriteExecute=yes'; then
        printf "  [INFO] wireplumber MemoryDenyWriteExecute=yes — fine for SBC/AAC (verified);\n"
        printf "         only relevant if LDAC/aptX codec plugins are required.\n"
    fi

    # -- the daemon's own self-check --
    echo "  -- soundsync doctor (daemon view):"
    run_user env SOUNDSYNC_WEB_DIR="$WEBUI_DEST" "$BINARY_DEST" doctor 2>&1 | sed 's/^/     /'

    echo ""
    printf "==> doctor: %d fail, %d warn\n" "$fails" "$warns"
    if [[ $fails -gt 0 ]]; then
        echo "    Failures remain — for a full pipeline reset run:  sudo $0 --reset"
    fi
    return 0
}

# ===========================================================================
# Mode: --reset  (nuclear reset of the audio pipeline to a known-good state)
# ===========================================================================
do_reset() {
    require_target_user reset
    set +e
    echo "==> SoundSync nuclear reset (target user: ${TARGET_USER})"
    echo "    Stops the pipeline, clears PipeWire/WirePlumber user state, re-applies"
    echo "    config, and restarts everything in order. Pairings + installed files kept."

    echo "[1/8] Stopping SoundSync + audio services"
    run_user systemctl --user stop soundsync.service 2>/dev/null
    systemctl stop soundsync-adapter-cod.service 2>/dev/null
    run_user systemctl --user stop wireplumber pipewire-pulse pipewire 2>/dev/null
    run_user pkill -f 'pw-loopback' 2>/dev/null
    run_user pkill -f 'parec.*soundsync' 2>/dev/null
    sleep 2

    echo "[2/8] Clearing PipeWire/WirePlumber user state + cache"
    if [[ -n "$TARGET_HOME" ]]; then
        run_user rm -rf "${TARGET_HOME}/.local/state/wireplumber" \
                        "${TARGET_HOME}/.local/state/pipewire" \
                        "${TARGET_HOME}/.cache/wireplumber"
    fi

    echo "[3/8] Removing stale soundsync WP configs (both formats; /etc + user)"
    rm -f /etc/wireplumber/wireplumber.conf.d/51-soundsync*.conf 2>/dev/null
    rm -f /etc/wireplumber/bluetooth.lua.d/51-soundsync* 2>/dev/null
    run_user bash -c 'rm -f ~/.config/wireplumber/wireplumber.conf.d/51-soundsync*.conf ~/.config/wireplumber/bluetooth.lua.d/51-soundsync* 2>/dev/null'

    echo "[4/8] Re-applying WirePlumber A2DP config"
    "$BINARY_DEST" apply-wireplumber-config

    echo "[5/8] Restarting bluetooth"
    systemctl restart bluetooth
    local i
    for i in 1 2 3 4 5; do
        dbus-send --system --print-reply --dest=org.bluez / \
            org.freedesktop.DBus.ObjectManager.GetManagedObjects &>/dev/null && break
        sleep 1
    done

    echo "[6/8] Starting PipeWire stack (pipewire -> pipewire-pulse -> wireplumber)"
    run_user systemctl --user start pipewire.service;       sleep 1
    run_user systemctl --user start pipewire-pulse.service; sleep 1
    run_user systemctl --user start wireplumber.service;    sleep 2
    for i in $(seq 1 10); do run_user pactl info &>/dev/null && break; sleep 1; done

    echo "[7/8] Re-pinning adapter + starting SoundSync services"
    systemctl restart soundsync-adapter.service 2>/dev/null   # HCI name + initial CoD
    systemctl restart soundsync-adapter-cod.service           # CoD watcher (keeps 0x240414)
    run_user systemctl --user daemon-reload
    run_user systemctl --user start soundsync.service
    loginctl enable-linger "$TARGET_USER" 2>/dev/null
    sleep 3

    echo "[8/8] Verification"
    local s cod code
    for s in pipewire wireplumber pipewire-pulse soundsync; do
        run_user systemctl --user is-active --quiet "$s" \
            && echo "      [OK] user $s" || echo "      [!!] user $s"
    done
    systemctl is-active --quiet bluetooth \
        && echo "      [OK] bluetooth" || echo "      [!!] bluetooth"
    systemctl is-active --quiet soundsync-adapter-cod.service \
        && echo "      [OK] CoD watcher" || echo "      [!!] CoD watcher"
    cod="$(hciconfig "$(detect_hci)" class 2>/dev/null | grep -oE '0x[0-9a-fA-F]{6}' | head -1)"
    echo "      adapter CoD: ${cod:-unknown} (want ${WANT_COD})"
    bluetoothctl show 2>/dev/null | grep -q '0000110b-0000-1000-8000-00805f9b34fb' \
        && echo "      [OK] advertises Audio Sink 0x110b" \
        || echo "      [!!] not advertising 0x110b"
    run_user pactl list short sinks 2>/dev/null | grep -q soundsync-capture \
        && echo "      [OK] soundsync-capture sink" || echo "      [!!] no soundsync-capture sink"
    code="$(run_user curl -s -o /dev/null -w '%{http_code}' "http://localhost:${BIND_PORT}/" 2>/dev/null)"
    echo "      web UI http: ${code:-no-response}"

    echo ""
    echo "==> reset complete. Connect a device to 'SoundSync' and play; open the web UI to Listen."
    return 0
}

# ---------------------------------------------------------------------------
# Mode dispatch — doctor/reset run and exit; otherwise fall through to install.
# ---------------------------------------------------------------------------
case "$MODE" in
    doctor) do_doctor; exit 0 ;;
    reset)  do_reset;  exit 0 ;;
esac

# ===========================================================================
# Mode: install
# ===========================================================================

# ---------------------------------------------------------------------------
# Node detection (for SPA build)
# ---------------------------------------------------------------------------
_setup_node() {
    if [[ -n "${SOUNDSYNC_NODE_BIN:-}" ]]; then
        export PATH="${SOUNDSYNC_NODE_BIN}:${PATH}"
        echo "info: using node from SOUNDSYNC_NODE_BIN=${SOUNDSYNC_NODE_BIN}"
    elif [[ -d "${HOME}/.local/node/bin" ]]; then
        export PATH="${HOME}/.local/node/bin:${PATH}"
        echo "info: detected node at ~/.local/node/bin"
    elif [[ -n "${TARGET_HOME:-}" && -d "${TARGET_HOME}/.local/node/bin" ]]; then
        # N3: under sudo, HOME is /root; the appliance user's node lives here.
        export PATH="${TARGET_HOME}/.local/node/bin:${PATH}"
        echo "info: detected node at ${TARGET_HOME}/.local/node/bin"
    fi
    if ! command -v node &>/dev/null; then
        echo "error: node not found on PATH. Set SOUNDSYNC_NODE_BIN or install node." >&2
        exit 1
    fi
    echo "info: node $(node --version), npm $(npm --version)"
}

# ---------------------------------------------------------------------------
# System dependency install (apt) — Debian/Ubuntu/Raspberry Pi OS
# ---------------------------------------------------------------------------
# Validated on Ubuntu 26.04 (PipeWire 1.6 / WirePlumber 0.5 / bluez 5.85).
# Idempotent: apt-get install is a no-op for already-present packages.
#   RUNTIME — needed to run + capture A2DP audio.
#   BUILD   — needed only when compiling from source here (NOT for --no-build).
#             cmake is required because webrtc-srtp builds a native lib via the
#             cmake crate; build-essential does NOT include it.
RUNTIME_PKGS=(
    pipewire pipewire-pulse pipewire-audio pipewire-bin wireplumber
    # pipewire-bin provides the pw-* tools (pw-cat/pw-cli/pw-metadata/pw-link/
    # pw-loopback) the daemon hard-requires; not contractual transitively.
    libspa-0.2-bluetooth pulseaudio-utils
    bluez bluez-tools ffmpeg
    avahi-daemon avahi-utils rfkill
    dbus-bin                              # dbus-monitor, used by the CoD watcher
    curl                                 # used by --reset / --doctor port check
)
BUILD_PKGS=(
    build-essential pkg-config cmake
    libdbus-1-dev libopus-dev
    cargo rustc nodejs npm
    git curl unzip
)
_install_system_deps() {
    if ! command -v apt-get &>/dev/null; then
        echo "warn: apt-get not found — skipping automatic dependency install." >&2
        echo "      Install these manually before continuing:" >&2
        echo "      runtime: ${RUNTIME_PKGS[*]}" >&2
        [[ $NO_BUILD -eq 0 ]] && echo "      build:   ${BUILD_PKGS[*]}" >&2
        return 0
    fi
    local pkgs=( "${RUNTIME_PKGS[@]}" )
    if [[ $NO_BUILD -eq 0 ]]; then
        pkgs+=( "${BUILD_PKGS[@]}" )
    fi
    echo "==> Installing system dependencies (apt)..."
    # N1: a transient/offline network must not abort a re-install where deps are
    # already present. Warn and continue; the install batch reports any failures.
    apt-get update -qq || echo 'warn: apt update failed (offline?); continuing' >&2
    DEBIAN_FRONTEND=noninteractive apt-get install -y "${pkgs[@]}" \
        || echo 'warn: some deps failed; verify with --doctor' >&2
}

if [[ $NO_DEPS -eq 0 ]]; then
    _install_system_deps
else
    echo "info: --no-deps given; skipping system dependency install."
fi

# G2: hciconfig (legacy bluez tool) is required for CoD pinning + the watcher;
# on bluez 5.x it may not be in the default bluez package. Warn, don't fail.
command -v hciconfig >/dev/null \
    || echo 'warn: hciconfig not found (CoD pinning will be unavailable on this host)' >&2

# ---------------------------------------------------------------------------
# Build step
# ---------------------------------------------------------------------------
if [[ $NO_BUILD -eq 0 ]]; then
    echo "==> Building Rust binary..."
    (cd "$REPO_DIR" && cargo build --release)

    echo "==> Building SPA (webui)..."
    _setup_node
    (cd "$REPO_DIR/webui" && npm ci && npm run build)
fi

# ---------------------------------------------------------------------------
# Source paths (either repo or --from dir)
# ---------------------------------------------------------------------------
if [[ -n "$FROM_DIR" ]]; then
    FROM_DIR="$(realpath "$FROM_DIR")"
    SRC_BINARY="${FROM_DIR}/soundsync"
    SRC_WEBUI="${FROM_DIR}/webui"
    SRC_USER_UNIT="${FROM_DIR}/soundsync.service"
    SRC_ADAPTER_UNIT="${FROM_DIR}/soundsync-adapter.service"
    SRC_COD_UNIT="${FROM_DIR}/soundsync-adapter-cod.service"
    SRC_COD_SCRIPT="${FROM_DIR}/soundsync-cod-watch.sh"
else
    SRC_BINARY="${REPO_DIR}/target/release/soundsync"
    SRC_WEBUI="${REPO_DIR}/webui/dist"
    SRC_USER_UNIT="${SCRIPT_DIR}/systemd/soundsync.service"
    SRC_ADAPTER_UNIT="${SCRIPT_DIR}/systemd/soundsync-adapter.service"
    SRC_COD_UNIT="${SCRIPT_DIR}/systemd/soundsync-adapter-cod.service"
    SRC_COD_SCRIPT="${SCRIPT_DIR}/soundsync-cod-watch.sh"
fi

# Validate sources
for f in "$SRC_BINARY" "$SRC_WEBUI" "$SRC_USER_UNIT" "$SRC_ADAPTER_UNIT" "$SRC_COD_UNIT" "$SRC_COD_SCRIPT"; do
    if [[ ! -e "$f" ]]; then
        echo "error: expected artifact not found: $f" >&2
        echo "hint:  run a build first, or use --from <prebuilt-dir>" >&2
        exit 1
    fi
done

# ---------------------------------------------------------------------------
# Install binary
# ---------------------------------------------------------------------------
echo "==> Installing binary -> ${BINARY_DEST}"
install -m 0755 "$SRC_BINARY" "$BINARY_DEST"

# ---------------------------------------------------------------------------
# Install webui SPA
# ---------------------------------------------------------------------------
echo "==> Installing webui -> ${WEBUI_DEST}"
# G14: deploy atomically. The SPA uses content-hashed asset filenames, so we
# must replace (not merge) the dir — but a clear-before-copy can leave the live
# UI empty/partial if interrupted. Build the new tree in a sibling, then swap.
# Refuse to touch the live dir unless the source actually has built content.
if [[ ! -f "${SRC_WEBUI}/index.html" ]]; then
    echo "error: webui source has no index.html: ${SRC_WEBUI}" >&2
    echo "hint:  run a build first, or use --from <prebuilt-dir>" >&2
    exit 1
fi
mkdir -p "$(dirname "$WEBUI_DEST")"
rm -rf "${WEBUI_DEST}.new" "${WEBUI_DEST}.old"
mkdir -p "${WEBUI_DEST}.new"
cp -a "${SRC_WEBUI}/." "${WEBUI_DEST}.new/"
if [[ -e "$WEBUI_DEST" ]]; then
    mv "$WEBUI_DEST" "${WEBUI_DEST}.old"
fi
mv "${WEBUI_DEST}.new" "$WEBUI_DEST"
rm -rf "${WEBUI_DEST}.old"

# ---------------------------------------------------------------------------
# Install systemd units
# ---------------------------------------------------------------------------
echo "==> Installing systemd units"
mkdir -p "$(dirname "$USER_UNIT_DEST")"
install -m 0644 "$SRC_USER_UNIT"    "$USER_UNIT_DEST"
mkdir -p "$(dirname "$SYSTEM_UNIT_DEST")"
install -m 0644 "$SRC_ADAPTER_UNIT" "$SYSTEM_UNIT_DEST"
install -m 0644 "$SRC_COD_UNIT"     "$COD_UNIT_DEST"

# ---------------------------------------------------------------------------
# Install the Class-of-Device watcher (keeps the adapter advertising as a
# speaker; see soundsync-cod-watch.sh / audio-routing-nuances.md AUD-002).
# ---------------------------------------------------------------------------
echo "==> Installing Class-of-Device watcher -> ${COD_SCRIPT_DEST}"
mkdir -p "$(dirname "$COD_SCRIPT_DEST")"
install -m 0755 "$SRC_COD_SCRIPT" "$COD_SCRIPT_DEST"

# ---------------------------------------------------------------------------
# Apply WirePlumber config (root, install-time)
# ---------------------------------------------------------------------------
echo "==> Applying WirePlumber A2DP config..."
if "$BINARY_DEST" apply-wireplumber-config; then
    echo "    WirePlumber config written."
else
    echo "warn: apply-wireplumber-config failed — WirePlumber may not be installed yet." \
         "Re-run after installing pipewire + wireplumber." >&2
fi

# ---------------------------------------------------------------------------
# Headless autostart: enable linger + reload the target user's WirePlumber
# ---------------------------------------------------------------------------
# The daemon is a systemd *user* service living in the target user's PipeWire
# session. Without linger that session (and the daemon) only exists while the
# user is logged in — wrong for a headless appliance. And because we wrote the
# A2DP config as root, the target user's WirePlumber must be restarted to load
# it (root cannot restart a user-session service directly).
if [[ -n "$TARGET_USER" && "$TARGET_USER" != "root" ]]; then
    echo "==> Enabling linger for ${TARGET_USER} (headless autostart across reboot)..."
    loginctl enable-linger "$TARGET_USER" \
        || echo "warn: could not enable linger for ${TARGET_USER}" >&2

    if [[ -n "$TARGET_UID" && -d "/run/user/${TARGET_UID}" ]]; then
        if run_user systemctl --user restart wireplumber 2>/dev/null; then
            echo "    restarted ${TARGET_USER}'s WirePlumber — A2DP config now active."
        else
            echo "    note: restart ${TARGET_USER}'s WirePlumber to load the A2DP config:"
            echo "          systemctl --user restart wireplumber   (or reboot)"
        fi
    fi
else
    echo "note: not run via sudo as the target user — set up headless autostart manually:"
    echo "        sudo loginctl enable-linger <user>"
    echo "        systemctl --user restart wireplumber   # load the A2DP config"
fi

# ---------------------------------------------------------------------------
# Enable systemd units
# ---------------------------------------------------------------------------
echo "==> Reloading systemd and enabling units..."
systemctl daemon-reload

# G13: on a RE-INSTALL the new binary/CoD script are on disk but the already-
# active system units keep running the OLD code (enable --now won't restart an
# active unit). try-restart picks up the new code now, mirroring the user-unit
# restart below. (no-op when the unit is inactive)
systemctl try-restart soundsync-adapter.service || true
systemctl try-restart soundsync-adapter-cod.service || true

# G3: adapter setup must NOT abort the whole install under `set -e`. The oneshot
# runs apply-adapter-config, which can fail if hciconfig is missing (G2) or no
# BT adapter is present. Split enable (persist) from start (this run) so a start
# failure is a WARNING and the rest of the enable section still runs.
systemctl enable soundsync-adapter.service || true
systemctl start soundsync-adapter.service \
    || echo 'warn: adapter setup failed (no BT adapter or missing hciconfig); will retry at boot' >&2
systemctl enable soundsync-adapter-cod.service || true
systemctl start soundsync-adapter-cod.service \
    || echo 'warn: CoD watcher failed to start (no BT adapter or missing hciconfig); will retry at boot' >&2
systemctl --global enable soundsync.service || true

# Reload the TARGET USER's manager too, so changes to the *user* unit (e.g.
# TimeoutStopSec/KillMode) apply to a currently-running instance now rather than
# only after the next login/reboot. Restart only if it is already running.
if [[ -n "$TARGET_USER" && -n "$TARGET_UID" && -d "/run/user/${TARGET_UID}" ]]; then
    run_user systemctl --user daemon-reload || true
    if run_user systemctl --user is-active --quiet soundsync.service; then
        run_user systemctl --user restart soundsync.service \
            && echo "    restarted ${TARGET_USER}'s running soundsync.service (unit changes applied)."
    else
        # G4: a fresh install must actually START the daemon THIS run, not just
        # enable it for the next login. enable --now also persists for the user.
        run_user systemctl --user enable --now soundsync.service \
            && echo "    started ${TARGET_USER}'s soundsync.service (running now)." \
            || echo "warn: could not start ${TARGET_USER}'s soundsync.service this run" >&2
    fi
fi

# ---------------------------------------------------------------------------
# Port-in-use check (ignore our own already-running daemon)
# ---------------------------------------------------------------------------
echo "==> Checking port availability..."
if ss -tlnp 2>/dev/null | grep ":${BIND_PORT}" | grep -qv 'soundsync'; then
    HOLDER="$(ss -tlnp 2>/dev/null | grep ":${BIND_PORT}" | awk '{print $NF}' | head -1 || true)"
    echo ""
    echo "WARN: port ${BIND_PORT} is already in use by another process (${HOLDER})."
    echo "      SoundSync will fail to bind on startup."
    echo "      To use a different port, create a systemd drop-in override:"
    echo ""
    echo "        mkdir -p ~/.config/systemd/user/soundsync.service.d"
    echo "        cat > ~/.config/systemd/user/soundsync.service.d/override.conf <<'EOF'"
    echo "        [Service]"
    echo "        Environment=SOUNDSYNC_BIND=0.0.0.0:8081"
    echo "        EOF"
    echo "        systemctl --user daemon-reload"
    echo ""
fi

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------
echo ""
echo "==> SoundSync installed successfully."
echo ""
echo "Next steps:"
echo "  1. Log out and back in (or run: systemctl --user start soundsync)"
echo "     to start the user-session daemon."
echo "  2. Open http://$(hostname -f 2>/dev/null || hostname):${BIND_PORT}"
echo "     in a browser from another device on the LAN."
echo "  3. Self-check / repair anytime:  sudo $0 --doctor"
echo "     Full reset if audio wedges:    sudo $0 --reset"
echo ""

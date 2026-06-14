#!/usr/bin/env bash
# SoundSync uninstaller — run as root; idempotent.
# Usage:
#   sudo packaging/uninstall.sh            # remove binary, webui, units
#   sudo packaging/uninstall.sh --purge    # also remove /etc WirePlumber config
#                                          # (per-user ~/.config/soundsync/ is not touched)
set -euo pipefail

BINARY_DEST=/usr/bin/soundsync
WEBUI_DEST=/usr/share/soundsync/webui
USER_UNIT=/usr/lib/systemd/user/soundsync.service
SYSTEM_UNIT=/usr/lib/systemd/system/soundsync-adapter.service
COD_UNIT=/usr/lib/systemd/system/soundsync-adapter-cod.service
COD_SCRIPT=/usr/lib/soundsync/soundsync-cod-watch.sh
# G6: purge the real superset the binary actually writes (mirror install.sh
# --reset). WP 0.5 uses .../wireplumber.conf.d/51-soundsync*.conf; WP 0.4.x uses
# .../bluetooth.lua.d/51-soundsync*. The old .../wireplumber.conf.d/51-soundsync.lua.d
# path was bogus (a directory the binary never wrote) and is dropped.
WP_CONF_GLOB='/etc/wireplumber/wireplumber.conf.d/51-soundsync*.conf'
WP_LUA_GLOB='/etc/wireplumber/bluetooth.lua.d/51-soundsync*'
# N7: deb-only conffile (shipped by the .deb, not by install.sh); a
# deb-then-shell teardown would otherwise orphan it.
DEB_CONFFILE=/etc/default/soundsync

PURGE=0

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --purge)
            PURGE=1
            shift
            ;;
        -h|--help)
            grep '^# ' "$0" | sed 's/^# //'
            exit 0
            ;;
        *)
            echo "error: unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Root check
# ---------------------------------------------------------------------------
if [[ $EUID -ne 0 ]]; then
    echo "error: this uninstaller must be run as root — use: sudo $0 $*" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Helper: remove a file if it exists (idempotent)
# ---------------------------------------------------------------------------
_rm_if_exists() {
    local path="$1"
    if [[ -e "$path" || -L "$path" ]]; then
        rm -rf "$path"
        echo "    removed: $path"
    else
        echo "    (already absent: $path)"
    fi
}

# ---------------------------------------------------------------------------
# G15: resolve the appliance user (mirror install.sh / debian/postinst).
# Prefer SUDO_USER; else auto-detect the SOLE human login account (exactly one
# passwd entry with uid in [1000,65534) and a real login shell). If zero or
# several candidates exist we leave it empty and skip the per-user stop — the
# --global disable below still removes the symlink. Best-effort, never fatal.
# ---------------------------------------------------------------------------
TARGET_USER="${SUDO_USER:-}"
if [[ -z "$TARGET_USER" || "$TARGET_USER" == "root" ]]; then
    TARGET_USER="$(
        getent passwd 2>/dev/null | awk -F: '
            $3 >= 1000 && $3 < 65534 &&
            $7 != "/usr/sbin/nologin" && $7 != "/sbin/nologin" &&
            $7 != "/bin/false" && $7 != "/usr/bin/false" && $7 != "" \
            { u=$1; n++ }
            END { if (n == 1) print u }
        '
    )" || true
fi
TARGET_UID=""
if [[ -n "$TARGET_USER" ]]; then
    TARGET_UID="$(id -u "$TARGET_USER" 2>/dev/null || true)"
fi

# ---------------------------------------------------------------------------
# G15: stop the running per-user soundsync.service BEFORE the binary is removed.
# --global disable (below) only removes the enable symlink; it does not stop a
# currently-running per-user instance. Once the binary is gone, that instance
# would hit Restart=always and flap until the session ends/reboot. Reach into
# the user's session bus to stop (and disable for good measure). Best-effort.
# ---------------------------------------------------------------------------
if [[ -n "$TARGET_USER" && -n "$TARGET_UID" && -d "/run/user/${TARGET_UID}" ]]; then
    echo "==> Stopping ${TARGET_USER}'s running soundsync.service (G15)..."
    sudo -u "$TARGET_USER" \
        XDG_RUNTIME_DIR="/run/user/${TARGET_UID}" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/${TARGET_UID}/bus" \
        systemctl --user stop soundsync.service 2>/dev/null || true
    sudo -u "$TARGET_USER" \
        XDG_RUNTIME_DIR="/run/user/${TARGET_UID}" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/${TARGET_UID}/bus" \
        systemctl --user disable soundsync.service 2>/dev/null || true
    echo "    stopped: soundsync.service (user: ${TARGET_USER})"
else
    echo "    (no active per-user session to stop soundsync.service in)"
fi

# ---------------------------------------------------------------------------
# Stop + disable units (ignore errors — units may already be inactive)
# ---------------------------------------------------------------------------
echo "==> Stopping and disabling systemd units..."

# Disable the user unit globally (best-effort — may not be enabled)
if systemctl --global is-enabled soundsync.service &>/dev/null; then
    systemctl --global disable soundsync.service || true
    echo "    disabled: soundsync.service (user global)"
else
    echo "    (already disabled: soundsync.service user global)"
fi

# Stop + disable the system adapter unit (best-effort)
if systemctl is-enabled soundsync-adapter.service &>/dev/null 2>&1; then
    systemctl disable --now soundsync-adapter.service || true
    echo "    disabled: soundsync-adapter.service"
else
    systemctl stop soundsync-adapter.service 2>/dev/null || true
    echo "    (already disabled or absent: soundsync-adapter.service)"
fi

# Stop + disable the Class-of-Device watcher (best-effort)
if systemctl is-enabled soundsync-adapter-cod.service &>/dev/null 2>&1; then
    systemctl disable --now soundsync-adapter-cod.service || true
    echo "    disabled: soundsync-adapter-cod.service"
else
    systemctl stop soundsync-adapter-cod.service 2>/dev/null || true
    echo "    (already disabled or absent: soundsync-adapter-cod.service)"
fi

# ---------------------------------------------------------------------------
# Remove installed files
# ---------------------------------------------------------------------------
echo "==> Removing installed files..."
_rm_if_exists "$BINARY_DEST"
_rm_if_exists "$WEBUI_DEST"
rmdir /usr/share/soundsync 2>/dev/null || true
_rm_if_exists "$USER_UNIT"
_rm_if_exists "$SYSTEM_UNIT"
_rm_if_exists "$COD_UNIT"
_rm_if_exists "$COD_SCRIPT"
rmdir /usr/lib/soundsync 2>/dev/null || true

# ---------------------------------------------------------------------------
# Reload systemd
# ---------------------------------------------------------------------------
echo "==> Reloading systemd daemon..."
systemctl daemon-reload

# ---------------------------------------------------------------------------
# Purge: remove /etc WirePlumber config
# ---------------------------------------------------------------------------
if [[ $PURGE -eq 1 ]]; then
    echo "==> Purging WirePlumber config..."
    # G6: remove the real paths the binary writes (both WP formats), mirroring
    # install.sh --reset. rm -f is idempotent + best-effort when nothing matches.
    rm -f $WP_CONF_GLOB 2>/dev/null || true
    echo "    removed (if present): $WP_CONF_GLOB"
    rm -f $WP_LUA_GLOB 2>/dev/null || true
    echo "    removed (if present): $WP_LUA_GLOB"
    # N7: best-effort remove the deb-only conffile (shipped by the .deb, never by
    # install.sh) so a deb-then-shell teardown does not orphan it.
    _rm_if_exists "$DEB_CONFFILE"
    echo ""
    echo "NOTE: per-user config in ~/.config/soundsync/ is NOT removed."
    echo "      Remove it manually if desired."
fi

# ---------------------------------------------------------------------------
# Done
# ---------------------------------------------------------------------------
echo ""
echo "==> SoundSync uninstalled."
if [[ $PURGE -eq 0 ]]; then
    echo "    Tip: re-run with --purge to also remove the WirePlumber config"
    echo "         (/etc/wireplumber/wireplumber.conf.d/51-soundsync*)."
fi
echo ""

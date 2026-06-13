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
WP_CONF=/etc/wireplumber/wireplumber.conf.d/51-soundsync.conf
WP_LUA=/etc/wireplumber/wireplumber.conf.d/51-soundsync.lua.d

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
    _rm_if_exists "$WP_CONF"
    _rm_if_exists "$WP_LUA"
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

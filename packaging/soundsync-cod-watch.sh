#!/usr/bin/env bash
# soundsync-cod-watch.sh — keep the Bluetooth adapter Class-of-Device pinned to an
# audio speaker so A2DP *source* devices (phones, turntables, laptops, …) reliably
# present SoundSync as a speaker and route audio to it. Runs as root (hciconfig
# needs CAP_NET_ADMIN). Installed to /usr/lib/soundsync/ and driven by
# soundsync-adapter-cod.service.
#
# WHY THIS EXISTS (root cause, verified on bluez 5.85 / Ubuntu 26.04 — see
# research/audio-routing-nuances.md AUD-002):
#   * bluez 5.85 IGNORES main.conf [General] Class (tested: even a clean
#     major/minor value like 0x000414 is not applied at startup).
#   * hciconfig can set the CoD, but bluetoothd RECOMPUTES the adapter class
#     (kernel-default major "Computer/Laptop" + dynamic service bits) every time a
#     BlueZ media endpoint is (re)registered. WirePlumber registers its A2DP/HFP
#     endpoints at startup AND whenever a device connects and a card is created,
#     so the audio major class set by a boot-time oneshot is wiped seconds later.
#   * The SoundSync daemon is an unprivileged systemd *user* service with no
#     CAP_NET_ADMIN, so it cannot re-assert the CoD itself.
# Therefore: re-pin the CoD reactively on every BlueZ D-Bus change (device
# connect/disconnect, profile re-registration, adapter Class property change),
# with a periodic backstop. Writes only when the CoD has actually drifted.
set -uo pipefail

# Adapter index: SOUNDSYNC_HCI if set, else the first /sys/class/bluetooth/hci*,
# else hci0 — no hardcoded single-adapter assumption.
HCI="${SOUNDSYNC_HCI:-}"
if [[ -z "$HCI" ]]; then
    HCI="$(ls -1 /sys/class/bluetooth/ 2>/dev/null | grep -m1 '^hci' || echo hci0)"
fi
COD="${SOUNDSYNC_COD:-0x240414}"   # Audio/Video major, Loudspeaker minor, Rendering+Audio service bits

get_cod() { hciconfig "$HCI" class 2>/dev/null | grep -oE '0x[0-9a-fA-F]{6}' | head -1; }

assert_cod() {
    local cur
    cur="$(get_cod)"
    [[ "$cur" == "$COD" ]] && return 0
    if hciconfig "$HCI" class "$COD" 2>/dev/null; then
        echo "soundsync-cod: re-pinned ${COD} on ${HCI} (was ${cur:-unknown})"
    fi
}

# Pin once at startup, then maintain.
assert_cod

# React to BlueZ adapter/device/transport changes; also re-check at least every
# 30s as a backstop. If dbus-monitor exits (EOF), leave the loop so systemd
# (Restart=always) respawns us with a fresh monitor.
dbus-monitor --system \
  "type='signal',interface='org.freedesktop.DBus.Properties',member='PropertiesChanged'" \
  "type='signal',interface='org.freedesktop.DBus.ObjectManager',member='InterfacesAdded'" 2>/dev/null \
| while true; do
      read -r -t 30 _ ; rc=$?
      # rc==0: a signal line; rc>128: read timed out (periodic backstop);
      # 0<rc<=128: pipe EOF -> dbus-monitor died, exit for a clean restart.
      if (( rc > 0 && rc <= 128 )); then break; fi
      assert_cod
  done

#!/bin/sh
# DCP Provider — .deb prerm hook (Linux parity for the Windows NSIS
# PREUNINSTALL hook; Tareq's one-click-uninstall request, 2026-07-02).
#
# Runs as root before the package is removed. Responsibilities:
#   1. Stop + disable the user-scope systemd unit (dcp-provider.service) for
#      the invoking user — mirrors the SUDO_USER / PKEXEC_UID walk postinst.sh
#      uses to find the human behind the root install.
#   2. Kill any GUI-spawned daemon running outside systemd (the desktop app
#      spawns `<python> ~/.dcp/dcp_daemon.py` detached — same match pattern
#      as the app's own kill_by_name("dcp_daemon.py")).
#   3. Bring the DCP WireGuard tunnel down (wg0 primary, wg1 fallback path —
#      see activate_wireguard()/activate_wireguard_fallback() in lib.rs).
#      Best-effort: absence of wg-quick or an already-down tunnel is fine.
#
# Deliberately KEPT: ~/.dcp (provider key/config.json, wg0.conf identity) and
# /etc/wireguard/*.conf — a re-install reconnects as the same provider.
# Everything is `|| true` best-effort: a prerm failure would abort the whole
# package removal, which is worse than leaving a process behind.
set -e

case "$1" in
  remove)
    # ── Identify the human user (postinst.sh mirror) ──────────────────
    TARGET_USER=""
    if [ -n "$SUDO_USER" ] && [ "$SUDO_USER" != "root" ]; then
      TARGET_USER="$SUDO_USER"
    elif [ -n "$PKEXEC_UID" ] && [ "$PKEXEC_UID" != "0" ]; then
      TARGET_USER="$(getent passwd "$PKEXEC_UID" | cut -d: -f1)"
    fi

    # ── 1. Stop + disable the systemd user unit ───────────────────────
    if [ -n "$TARGET_USER" ] && command -v systemctl >/dev/null 2>&1; then
      runuser -u "$TARGET_USER" -- systemctl --user disable --now dcp-provider.service 2>/dev/null || true
    fi

    # ── 2. Kill any daemon started outside systemd ────────────────────
    # Matches the GUI-spawned python daemon and the systemd-managed
    # ~/.dcp/daemon binary, for any user, in case unit teardown missed it.
    if command -v pkill >/dev/null 2>&1; then
      pkill -f 'dcp_daemon\.py' 2>/dev/null || true
      pkill -f '\.dcp/daemon' 2>/dev/null || true
    fi

    # ── 3. Deactivate the DCP WireGuard tunnel (best-effort) ──────────
    if command -v wg-quick >/dev/null 2>&1; then
      wg-quick down wg0 2>/dev/null || true
      wg-quick down wg1 2>/dev/null || true
    fi
    ;;

  upgrade|deconfigure|failed-upgrade)
    # Upgrades must stay non-disruptive: daemon + tunnel keep running while
    # only the app binary is replaced (matches Windows $UpdateMode behavior).
    ;;
esac

exit 0

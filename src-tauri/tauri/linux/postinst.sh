#!/bin/sh
# DCP Provider — .deb postinst hook.
#
# Runs as root after the package is unpacked. Two responsibilities:
#   1. Reload polkit so /usr/share/polkit-1/actions/sa.dcp.provider.policy
#      is picked up immediately. Without this the WG-tunnel-up prompt at
#      first run still works, but it shows the generic
#      "Authentication is required to run /usr/bin/wg-quick" string until
#      the next login. Reloading is harmless if polkit isn't running.
#   2. Enable the user-scope systemd unit for the *invoking* user so the
#      DCP daemon survives reboot. The .deb runs postinst as root, so we
#      walk the SUDO_USER / PKEXEC_UID chain to figure out who actually
#      installed us. If we can't, we no-op gracefully — the user can still
#      `systemctl --user enable dcp-provider` themselves later.
set -e

case "$1" in
  configure)
    # ── 1. Reload polkit (best-effort) ────────────────────────────────
    if command -v systemctl >/dev/null 2>&1; then
      systemctl reload polkit.service 2>/dev/null || true
      systemctl reload polkitd.service 2>/dev/null || true
    fi
    if command -v pkill >/dev/null 2>&1; then
      pkill -HUP polkitd 2>/dev/null || true
    fi

    # ── 2. Enable user systemd unit ───────────────────────────────────
    # Identify the human user who triggered the install. SUDO_USER is set
    # under sudo, PKEXEC_UID under pkexec/Apt-via-graphical-installer.
    TARGET_USER=""
    if [ -n "$SUDO_USER" ] && [ "$SUDO_USER" != "root" ]; then
      TARGET_USER="$SUDO_USER"
    elif [ -n "$PKEXEC_UID" ] && [ "$PKEXEC_UID" != "0" ]; then
      TARGET_USER="$(getent passwd "$PKEXEC_UID" | cut -d: -f1)"
    fi

    if [ -n "$TARGET_USER" ]; then
      USER_UID="$(id -u "$TARGET_USER" 2>/dev/null || echo "")"
      if [ -n "$USER_UID" ] && command -v systemctl >/dev/null 2>&1; then
        # `systemctl --user` requires the user's bus. For an offline
        # install the bus may not be running; --machine=USER@ provides a
        # unit-of-work that systemd will activate on next login if the
        # bus isn't up.
        runuser -u "$TARGET_USER" -- systemctl --user daemon-reload 2>/dev/null || true
        runuser -u "$TARGET_USER" -- systemctl --user enable dcp-provider.service 2>/dev/null || true
        # Try to start it now too — this is harmless if the user hasn't
        # finished first-run yet (ConditionPathExists in the unit file
        # gates it on ~/.dcp/daemon-config.json existing).
        runuser -u "$TARGET_USER" -- systemctl --user start dcp-provider.service 2>/dev/null || true
      fi
    fi
    ;;

  abort-upgrade|abort-remove|abort-deconfigure)
    ;;
esac

#DEBHELPER#

exit 0

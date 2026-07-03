; DCP Provider — NSIS installer hooks (wired via bundle.windows.nsis.installerHooks).
;
; Why this exists (Tareq, 2026-07-02):
;   1. (Re)installing over a RUNNING provider daemon used to conflict or leave a
;      zombie daemon — Tauri's template only kills the app exe
;      (CheckIfAppIsRunning "${MAINBINARYNAME}.exe"), not the detached python
;      daemon the app spawns.
;   2. Uninstall left orphaned services behind: the WireGuard tunnel service
;      ("WireGuardTunnel$wg0", registered elevated via
;      `wireguard.exe /installtunnelservice %USERPROFILE%\.dcp\wg0.conf`) and
;      the daemon runtime the app downloads into %USERPROFILE%\.dcp.
;
; Facts these hooks rely on (see src-tauri/src/lib.rs):
;   • Daemon = `<python> %USERPROFILE%\.dcp\dcp_daemon.py --no-watchdog ...`,
;     spawned detached (survives app exit). Python is usually the embedded
;     %USERPROFILE%\.dcp\python\python.exe, but may be system python — so we
;     match on the command line (dcp_daemon.py), same as the app's own
;     kill_by_name() fallback, not on the image name.
;   • PID file: %USERPROFILE%\.dcp\daemon.pid (can be stale — PIDs get reused,
;     so we never kill blindly by it; we just clean it up).
;   • WG tunnel: conf file wg0.conf → tunnel name "wg0" → Windows service
;     "WireGuardTunnel$wg0". Managing it REQUIRES ADMIN; this installer runs
;     per-user, so removal elevates via Start-Process -Verb RunAs (one UAC
;     prompt) and tolerates a declined prompt.
;   • $UpdateMode = 1 when tauri-plugin-updater runs this installer (and the
;     previous version's uninstaller) with /UPDATE. Auto-updates must stay
;     non-disruptive: no UAC prompts, tunnel stays up, daemon runtime kept.
;     The app auto-restarts the daemon on next launch (startup auto-restart in
;     lib.rs), so stopping the daemon during update is safe.
;
; Hook contract (Tauri v2): NSIS_HOOK_PREINSTALL runs before files are copied
; (and before the template's own CheckIfAppIsRunning); NSIS_HOOK_PREUNINSTALL
; runs before the uninstaller removes anything; NSIS_HOOK_POSTUNINSTALL runs
; after files/registry/shortcuts are gone. All hooks also run during
; auto-updates (install side, plus the OLD uninstaller with /UPDATE), hence
; the $UpdateMode guards below.

; ── Helpers ─────────────────────────────────────────────────────────────────

; Stop the provider daemon. Kills every process whose command line contains
; dcp_daemon.py (excluding the killer powershell itself, whose own command
; line matches), then removes the stale PID file. Runs as the installing
; user — the same user the daemon runs as — so no elevation is needed.
!macro DCP_STOP_DAEMON
  DetailPrint "Stopping DCP provider daemon (if running)..."
  Push $0
  nsExec::ExecToLog `powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$$ErrorActionPreference='SilentlyContinue'; Get-CimInstance Win32_Process -Filter 'CommandLine LIKE ''%dcp_daemon.py%''' | Where-Object { $$_.ProcessId -ne $$PID } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force }; Remove-Item (Join-Path $$env:USERPROFILE '.dcp\daemon.pid') -Force -ErrorAction SilentlyContinue"`
  Pop $0 ; nsExec exit code — best-effort, ignored
  Pop $0
!macroend

; Deactivate + deregister the WireGuard tunnel service ("WireGuardTunnel$wg0").
; No-op when the service doesn't exist (fresh machines never see a UAC prompt).
; Primary path: `wireguard.exe /uninstalltunnelservice wg0` — the exact inverse
; of what activate_wireguard() runs (lib.rs), elevated the same way. Fallback
; when wireguard.exe is gone but the service lingers: elevated sc stop+delete.
; A declined UAC prompt is tolerated (tunnel simply stays, nothing breaks).
!macro DCP_REMOVE_WG_TUNNEL
  DetailPrint "Removing DCP WireGuard tunnel service (if present)..."
  Push $0
  nsExec::ExecToLog `powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "$$ErrorActionPreference='SilentlyContinue'; if (-not (Get-Service -Name 'WireGuardTunnel$$wg0' -ErrorAction SilentlyContinue)) { exit 0 }; $$wg='C:\Program Files\WireGuard\wireguard.exe'; if (Test-Path $$wg) { try { Start-Process -FilePath $$wg -ArgumentList '/uninstalltunnelservice','wg0' -Verb RunAs -Wait } catch {}; Start-Sleep -Seconds 2 }; if (Get-Service -Name 'WireGuardTunnel$$wg0' -ErrorAction SilentlyContinue) { try { Start-Process powershell.exe -ArgumentList '-NoProfile','-Command','sc.exe stop ''WireGuardTunnel$$wg0''; sc.exe delete ''WireGuardTunnel$$wg0''' -Verb RunAs -Wait } catch {} }"`
  Pop $0 ; nsExec exit code — best-effort, ignored
  Pop $0
!macroend

; ── Install ─────────────────────────────────────────────────────────────────

!macro NSIS_HOOK_PREINSTALL
  ; Always: make sure no daemon holds files/ports while we (re)install.
  ; Covers manual reinstall over a running node AND silent auto-updates
  ; (the updater runs this installer with /UPDATE while the detached daemon
  ; keeps running behind the exiting app).
  !insertmacro DCP_STOP_DAEMON

  ; Clean-slate tunnel removal ONLY for interactive, non-update (re)installs.
  ; Never during auto-updates or /S installs: a surprise UAC prompt would
  ; stall/fail unattended updates and dropping the tunnel would cut the node
  ; off mid-update. The setup wizard re-activates (or reuses) the tunnel
  ; after install, so an interactive reinstall reconnects cleanly.
  ${If} $UpdateMode <> 1
    ${IfNot} ${Silent}
      !insertmacro DCP_REMOVE_WG_TUNNEL
    ${EndIf}
  ${EndIf}
!macroend

; ── Uninstall ───────────────────────────────────────────────────────────────

!macro NSIS_HOOK_PREUNINSTALL
  ; Always stop the daemon — also when this uninstaller is invoked by an
  ; update (/UPDATE): the incoming installer expects a quiet system.
  !insertmacro DCP_STOP_DAEMON

  ; Real uninstall only: take the WireGuard tunnel service out with us so
  ; nothing orphaned keeps running. Skipped in update mode so auto-updates
  ; never drop connectivity or raise UAC.
  ${If} $UpdateMode <> 1
    !insertmacro DCP_REMOVE_WG_TUNNEL
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Real uninstall only: remove the daemon RUNTIME the app downloaded into
  ; %USERPROFILE%\.dcp (daemon script, embedded python, logs, cached
  ; third-party installers). Deliberately KEEP the provider identity/config —
  ; config.json (api_key, run_mode, ...) and the WireGuard configs
  ; (wg0.conf/wg1.conf) — so a re-install reconnects as the same provider.
  ; No RMDir on .dcp itself, and no blanket wildcard delete: keep-list safety.
  ${If} $UpdateMode <> 1
    DetailPrint "Removing DCP daemon runtime (provider identity/config kept)..."
    Delete "$PROFILE\.dcp\dcp_daemon.py"
    Delete "$PROFILE\.dcp\daemon.pid"
    Delete "$PROFILE\.dcp\daemon.log"
    Delete "$PROFILE\.dcp\daemon_error.log"
    Delete "$PROFILE\.dcp\startup.log"
    Delete "$PROFILE\.dcp\mlx-server.log"
    Delete "$PROFILE\.dcp\python-embed.zip"
    Delete "$PROFILE\.dcp\get-pip.py"
    Delete "$PROFILE\.dcp\wireguard-installer.exe"
    Delete "$PROFILE\.dcp\OllamaSetup.exe"
    RMDir /r "$PROFILE\.dcp\python"
    RMDir /r "$PROFILE\.dcp\.cache"
  ${EndIf}
!macroend

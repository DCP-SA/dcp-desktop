# DCP Provider — Daemon & Desktop Stability Brief
**Date:** 2026-04-26
**Audience:** Tarek (and for side-by-side comparison vs Tito's external review)
**Scope:** Last 48 h of work on `dcp-desktop` (Tauri .exe) + `dcp_daemon.py`
**Authoring branches:** `peter/48h-stability-roadmap` (desktop), `peter/daemon-4.2.0-rename` (daemon, merged to main as #293)

---

## 1. What was reviewed

Two parallel internal reviews on 2026-04-26 produced 91 findings combined:

| Doc | Scope | Findings |
|---|---|---|
| `docs/CODE-REVIEW-2026-04-26.md` | Tauri Rust correctness, security, error handling | 29 |
| `docs/QOL-STABILITY-REVIEW-2026-04-26.md` | Provider UX, daemon lifecycle, supervisor model | 62 |

These were merged into a single execution doc:

- `docs/48H-ROADMAP-2026-04-26.md` — 5 tiers, low-risk → high-risk, deferred lists for 0.3.0 and 0.4.0+

The roadmap explicitly excludes structural rewrites (single-supervisor architecture, signed update bundles, pynvml replacement, run-mode states) so we could ship surgical commits without a full rebuild loop.

---

## 2. What shipped (last 48 h)

### Yesterday (2026-04-25) — desktop hardening

| Commit | Fix |
|---|---|
| `7c12ec8` | Hide `cmd.exe` consoles in **all** subprocess sites on Windows (CREATE_NO_WINDOW). Prior builds flashed black windows during heartbeat / metric polls. |
| `c0eef00`, `52c6b85`, `213ecdc`, `4de8041`, `39d72f1`, `b75409c`, `3b10697` | Tauri updater signing chain — base64 key passing, heredoc preservation in GitHub Actions, version sync, trailing-comma fix in `tauri.conf.json`, temporary signing disable for Fadi-blocking DXGI build, then re-enable. |

### Today (2026-04-26) — daemon 4.1.2

Both shipped and **deployed to VPS**:

| ID | Fix |
|---|---|
| **G31** | `compute_cap` added to `nvidia-smi` query → backend now sees `gpu_compute` / `gpu_tier` correctly (was empty for ~all providers). |
| **G30** | `RotatingFileHandler` for `daemon.log` (10 MB × 5 files). Previously could grow unbounded. |

### Today (2026-04-26) — desktop 0.2.2 (Tier 1 + Tier 2 of roadmap)

11 fixes, one commit each, all on `peter/48h-stability-roadmap`:

**Tier 1 — trivials (0 behavior risk):**

| ID | Commit | Fix |
|---|---|---|
| G55 | `e3e67f5` | Tray "View Logs" opens correct daemon log path |
| H9 | `3bfd4ee` | Replace `unwrap()` in `setup()` (3 sites) with logged fallbacks |
| L2 | `0218612` | Tray menu — build `separator4` instead of reusing `separator3` |
| L4 | `464c8b4` | `chrono_now` → ISO 8601 UTC timestamps in human-facing logs |
| M7 | `898d35b` | `daemon.log` / `cloudflared.log` / `mlx.log` append-only (was truncated on every start) |

**Tier 2 — single-file Rust:**

| ID | Commit | Fix |
|---|---|---|
| G2  | `283ac35` | Detach daemon from parent process group — survives parent .exe quit |
| M3 partial | `b98fdd0` | Replace `let _ = ...` with logged errors at 4 critical sites |
| M6  | `564226b` | `tail_file` seeks from end, reads last 64 KB window |
| M11 | `8f584fd` | Atomic daemon-file write (temp + fsync + rename) — updater relaunch can't catch a half-written binary |
| H8  | `78a7b53` | Removed broken `register_provider` Tauri command (was unreachable + half-implemented) |

**DC1→DCP rename Phase A** (today, separate from the roadmap):

| Commit | Fix |
|---|---|
| `2b23311` (PR #293) | `dcp_daemon.py` 4.2.0 — rename all path constants `~/dc1-provider` → `~/dcp-provider`, **one-way auto-migration block** (atomic `os.rename`) so existing providers don't lose their config or split-brain. 11 thread names + 4 docstrings rebranded. |
| `5be05a2` | Tauri `lib.rs` tray "View Logs" path updated to `~/dcp-provider/logs/daemon.log` |
| `804ce9e` | CI fix: pin `pytest>=8.3,<8.4` (8.4.x dropped Python 3.8 support, was breaking the daemon-tests matrix) |
| `82e3c82` (PR #294) | `install.sh` + `daemon.sh` rename alignment — fixes API-key validation that was rejecting real `dcp-provider-…` keys, and stops the GPU-autodetect script from writing to `~/dc1-provider/` while the daemon now uses `~/dcp-provider/` |

---

## 3. What's queued (next 0–24 h, still 0.2.2 → 0.2.3)

From the same roadmap, **not yet implemented**:

**Tier 2 leftovers:**
- **G56** — Wire tray Pause/Resume match arms (currently no-ops)
- **M5** — Track spawned child PIDs in `DaemonState` instead of `pkill -f` / `wmic ... like '%pattern%'`

**Tier 3 — feature-level:**
- **G57** — 60 s tray refresh loop so status/earnings reflect real state
- **G33** — Wire `upload_provider_logs` to tray "Report a Problem" + Dashboard button
- **C2** — **Highest leverage:** Updater error path — events from Rust, UI prompt, "skip update for this launch" escape hatch, categorized error logs. De-risks every future .exe push.

**Tier 4 — coordinated daemon + Tauri:**
- **G47** — Real pause via heartbeat (`_is_paused` flag refreshed each beat, gates `poll_and_execute`)
- **G19** — sha256 manifest verification in `perform_update` (backend serves `.sha256`, daemon verifies before atomic replace)
- **G6**  — Backup-before-overwrite + crash-rollback for Tauri `update_daemon` (symmetric with Python watchdog)
- **G37** — `tauri-plugin-single-instance` (also closes M4 PID-file race)

**Tier 5 — needs WebView regression test:**
- **C1 / L1** — Drop `shell:allow-execute`; tighten CSP to `default-src 'self'; connect-src 'self' https://api.dcp.sa`

---

## 4. Deferred (NOT in this 48 h window)

### To 0.3.0
H1–H4 async runtime hygiene, H5 API key in headers, H6/H7 signed bundles, G1 Tauri-side supervisor, G9 heartbeat backlog persistence, G11 job-result spool, G25 wizard resume, G34 differentiated error states, G13 GPU OOM classification, G10 cloudflared watchdog, G17/G18 temp/usage caps, in-app log viewer.

### To 0.4.0+ / structural
- Single-supervisor architecture decision (.exe-shim vs OS-supervised daemon — biggest open question)
- First-class `run_mode = scheduled / idle`
- Update channels (stable / beta / canary)
- E2E signed update bundles
- `pynvml` replacing `subprocess.run("nvidia-smi")`
- Cross-platform CI install matrix

---

## 5. Roadmap discipline (commitments)

- One Tier item per commit, conventional commit messages
- `cargo check` / `npm run build` after each commit, before the next
- **No new .exe build and no PM2 deploy without explicit Peter approval**
- Daemon hot-deploy to VPS only after md5 match against the tagged build
- Auto-migration `os.rename` is one-way (no rollback) — by design, simpler than dual-read fallbacks; all 43 registered providers will migrate on next daemon start

---

## 6. Side-finding (not blocking, worth flagging to Tarek)

- GitHub PAT exposed in plaintext in the `dc1-platform` git remote URL on Peter's box. Should be rotated and switched to SSH or `gh auth`.
- `pytest` CI matrix was broken for everyone before today's `804ce9e` (Python 3.8 couldn't install pytest 8.4.x). Pre-existing infra bug.

---

## 7. Comparison hooks for Tito's review

When reading Tito's review against this brief, the most useful framing questions are:

1. **Does Tito flag the same Tier 4 items (G47, G19, G6, G37) as the high-leverage work?** If yes, the priority is independently validated.
2. **Does Tito raise structural concerns we deferred to 0.4.0+** (single-supervisor, signed bundles, run-mode)? If yes, that's the next planning conversation, not this sprint.
3. **Does Tito flag anything we missed entirely?** Most likely candidates: telemetry/observability gaps, PDPL/privacy posture of the upload-logs flow, GPU thermal/power policy enforcement.
4. **Does Tito flag false alarms in our review?** Items rated H/M internally that he sees as L, or vice-versa.

---

**Source files (all in `dcp-desktop/docs/`):**
- `CODE-REVIEW-2026-04-26.md`
- `QOL-STABILITY-REVIEW-2026-04-26.md`
- `48H-ROADMAP-2026-04-26.md`
- `DC1-TO-DCP-RENAME-2026-04-26.md` (cutover plan, Phase A done)

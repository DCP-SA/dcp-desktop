# DCP Provider QoL & Stability Review — daemon + .exe

**Date:** 2026-04-26
**Scope:** Tauri desktop app (`dcp-desktop/`) + Python daemon (`dc1-platform/backend/installers/dcp_daemon.py`).
**Out of scope:** items already covered in `CODE-REVIEW-2026-04-26.md` (security/bug review).

## Current state assessment

Further along than most fleets at this stage: the Python daemon has a real outer watchdog with auto-rollback, exponential backoff with jitter on heartbeats, per-endpoint circuit breaker for job polling, engine-health watchdog with argv-replay restart for vLLM/llama.cpp, atomic update writes with fsync, 5MB rotating events journal, HMAC verification on `task_spec`, graceful drain on SIGTERM, in-process thread supervisor that reports dead threads, and structured `report_event` telemetry. **Crash recovery and supervision is the strongest area.**

Fragility concentrates in three places:

1. **The daemon ignores almost every operator-facing knob the wizard collects.** `run_mode`, `gpu_usage_cap`, `temp_limit`, `start_on_boot`, even `is_paused` are not consulted anywhere in `dcp_daemon.py`. The pause button hits a backend endpoint and prays.
2. **The Tauri-spawned daemon is parented to the .exe and uses `--no-watchdog`** (`lib.rs:1252`). Force-quit Tauri = orphan/dead daemon, no crash supervisor at all in that deploy path. The watchdog only protects providers who installed via the .ps1/.sh scripts.
3. **Observability is one-way and shallow.** `daemon.log` does not rotate, no in-app log viewer beyond a 20-line tail, `upload_provider_logs` Rust command is not exposed in `invoke_handler`, daemon does not echo `is_paused`/`run_mode` in heartbeats so the dashboard cannot say "you are paused" vs "no jobs available."

## Three findings that should make us uncomfortable

### Pause is fiction (G47/G48/G49)
Exhaustive grep on `dcp_daemon.py` — **zero matches** for `is_paused|paused|run_mode|schedule`. The pause button (`lib.rs:885`) hits `POST /providers/pause`. If the backend stops sending jobs, fine. But:
- `poll_and_execute` keeps polling 6×/min regardless.
- If backend pause flag is missed, daemon accepts the next assigned job.
- `run_mode = "scheduled"` and `run_mode = "idle"` are collected by the wizard, persisted, never implemented.
- Currently-running job behavior on pause: undefined.

A provider who hits Pause and then sees a job complete 90s later loses confidence. This is not a future concern — Fadi will hit it.

### Tray buttons are no-ops or open empty files (G55/G56/G57)
- "View Logs" opens `~/.dcp/daemon.log` — but the daemon writes to `~/dc1-provider/logs/daemon.log` (`dcp_daemon.py:177`). **The button opens a non-existent file.**
- "Pause Provider" / "Resume Provider" tray menu items at `lib.rs:2919-2920` have no match arm in `on_menu_event` (`:2952-2972`). **Clicks are no-ops.**
- "Earnings: calculating..." and "Status: Starting..." are static strings forever. No update loop.

### `gpu_compute` empty in DB has a root cause (G31)
You asked why Fadi's `gpu_compute` and `gpu_tier` are empty. The nvidia-smi query at `dcp_daemon.py:1201` is `--query-gpu=name,memory.total,driver_version` — it does **not** include `compute_cap`. So `gpu.get("compute_capability")` (`:4407`) is always None, regardless of GPU. Five-minute fix.

## 48-hour roadmap (recommended execution order)

Each is small. Together they close the biggest visible-correctness, observability, and pause-trust gaps before more providers come online.

| # | Finding | Fix | Effort |
|---|---|---|---|
| 1 | G31 | Add `compute_cap` to nvidia-smi query in daemon | 5 min |
| 2 | G55 | Fix tray "View Logs" path | 5 min |
| 3 | G56 | Wire tray Pause/Resume match arms | 1 hr |
| 4 | G57 | 60s tray refresh loop for status/earnings text | 2 hrs |
| 5 | G30 | `RotatingFileHandler` for `daemon.log` (10MB×5) | 10 min |
| 6 | G37 | Add `tauri-plugin-single-instance` | 1 hr |
| 7 | G47 | Daemon-side `_is_paused` flag + gate in `poll_and_execute`, refreshed via heartbeat response | 0.5 day |
| 8 | G2 | `start_new_session=True` (Unix) / `CREATE_NEW_PROCESS_GROUP` (Windows) on daemon spawn | 1 hr |
| 9 | G33 | Wire existing `upload_provider_logs` to tray "Report a Problem" + Dashboard button | 2 hrs |
| 10 | G19 | sha256 manifest verification in `perform_update` (backend serves `.sha256`, daemon verifies before atomic replace) | 0.5 day |
| 11 | G6 | Backup-before-overwrite + crash-rollback for Tauri `update_daemon` (symmetric with Python watchdog) | 0.5 day |

After these 11: most operational visibility + pause-trust + update-safety story closed. Can scale to next 5-10 providers without support burden ballooning.

## Phased roadmap

### Right now — surgical (the 48-hour list above)
G31, G55, G56, G57, G30, G37, G47, G2, G33, G19, G6.

### Next 2-3 releases — medium-effort, high-impact
- **G4**: Implement `start_on_boot` via `tauri-plugin-autostart` (initialized but never called).
- **G1**: Tauri-side daemon supervisor (30s loop, respawn on dead PID after grace window). Mirrors Python watchdog crash-window/give-up logic. Required for the .exe path.
- **G9**: Persist heartbeat backlog → `~/.dcp/heartbeat_backlog.jsonl`, batch-upload on first 2xx after outage.
- **G11**: Job-result spool to disk when `RESULT_POST_RETRIES` exhausted, retry on 60s loop forever. Paid work — losing it is unacceptable.
- **G25**: Wizard resume — persist `wizard_state.json` after each step.
- **G34**: Differentiated error states in dashboard (DNS / TLS / 401 / 5xx / OK-but-no-jobs). Daemon emits structured `last_outcome` in heartbeat.
- **G13**: GPU OOM classification. Parse `outcome.stderr` for `OutOfMemoryError`, `CUDA out of memory`; set `transient: True`.
- **G10**: Cloudflared liveness watchdog — hit tunnel URL every 60s externally; restart on failure.
- **G17/G18**: Apply `temp_limit` and `gpu_usage_cap` in pre-job and mid-job checks.
- **In-app log viewer** with auto-tail, search, "copy to clipboard" and "upload" buttons.

### Strategic — multi-release, structural
1. **Single supervisor, single source of truth.** The .exe path bypasses the .ps1/.sh watchdog. Pick: (a) the .exe is UI + thin shim, daemon installed and supervised by the OS (LaunchAgent / systemd / Windows Service), desktop optional; or (b) the .exe owns the supervisor end-to-end and the .py watchdog goes away. The current half-and-half is the source of most §1 gaps.
2. **First-class `run_mode = "scheduled"` and `"idle"`** with timezone-aware schedule, idle detection (CPU < 10% for 5min, no foreground GPU process, etc.).
3. **Formalize update channels** (stable / beta / canary) with `--pin-version` for support escalations and "this provider is on hold for n hours" backend override flag.
4. **End-to-end signed update bundles** (tied to security review).
5. **Replace `subprocess.run("nvidia-smi")` polling with `pynvml`.** Order of magnitude cheaper; no subprocess per heartbeat tick.
6. **Cross-platform install/auto-start matrix and CI.** GitHub Actions runners on macOS, Windows, Ubuntu, Fedora that install via the .exe and run a 5-minute provider session.

## Strengths to keep doing

- Watchdog auto-rollback with `UPDATE_CRASH_THRESHOLD=90s`, persisted via `UPDATE_SUPPRESSION_FILE` so suppression survives the rollback restart (`dcp_daemon.py:6506-6531`, `:306-326`).
- Atomic update write via tempfile + fsync + `os.replace`, mode preservation (`:1330-1355`).
- Heartbeat exponential backoff with jitter, capped at 300s, reset on 2xx (`:4642-4672`).
- Per-endpoint circuit breaker for job poll cascade — only opens on 5xx/429/408, treats 401/403 separately as a credential issue (`:5636-5675`).
- HMAC verification of `task_spec` before execution (`:5827-5843`).
- Graceful drain on SIGTERM/SIGINT with `_handle_signal` and final draining heartbeat (`:6086-6110`). 5-min cap.
- Pre-flight VRAM, disk, profitability, dedup, HMAC guards (`:5772-5843`).
- Job dedup file with size cap (`_DEDUP_MAX_ENTRIES = 10_000`).
- Code-hash sha256 prefix in heartbeat for fleet-wide drift detection (`:210-222`, `:4551`).
- Thread supervisor reports any dead background thread to backend (`:6358-6376`).
- Tauri prevents window-close from quitting (hides to tray) (`lib.rs:2940-2945`).

---

## Full gap list (62 findings)

### 1. Crash recovery & supervision

- **G1 BLOCKER** — Tauri-spawned daemon runs with `--no-watchdog` (`lib.rs:1252`) and there is no .exe-side supervisor. A daemon crash on the .exe path = provider offline until next app open.
- **G2 SERIOUS** — Daemon spawned without `start_new_session`/`setsid` on Unix (`lib.rs:1250-1262`). Tauri force-quit kills the daemon on macOS/Linux. (vLLM restart at `dcp_daemon.py:2437` correctly uses `start_new_session=True` — same pattern missing here.)
- **G3 SERIOUS** — Force-quitting Tauri leaves orphan Ollama / mlx_lm.server / cloudflared. PIDs not recorded; killed by name only on next `full_start_provider`.
- **G4 SERIOUS** — `start_on_boot` collected by wizard but `tauri-plugin-autostart` never invoked despite being initialized at `lib.rs:2905`. Reboot = provider offline.
- **G5 IMPROVEMENT** — Engine-watchdog gap up to 180s (`ENGINE_WATCHDOG_INTERVAL=60`, `ENGINE_FAILURE_THRESHOLD=3`).
- **G6 IMPROVEMENT** — Tauri `update_daemon` (`lib.rs:2148-2213`) keeps no backup, has no crash detector, silently overwrites. Watchdog rollback path never reached for .exe-driven updates.
- **G7 NICE-TO-HAVE** — `MAX_CRASH_RESTARTS` give-up path leaves no recovery (`:6534-6543`).

### 2. Network resilience

- **G8 BLOCKER** — DNS / TLS / unreachable-host failures collapse into one WARNING. Dashboard can't differentiate "no internet" from "DCP backend down."
- **G9 SERIOUS** — No queueing of failed heartbeats. 1-hour outage = backend has no telemetry for the gap, no catch-up. Local events.jsonl exists but not uploaded on recovery.
- **G10 SERIOUS** — Cloudflared has no liveness check. Tunnel dies = daemon reports `engines_active` but backend can't reach engine.
- **G11 IMPROVEMENT** — Job-result POST has 3-attempt linear retry (5s, 10s) then **lost**. No spool-to-disk. Flaky 60s outage = paid work lost.
- **G12 IMPROVEMENT** — Persistent 401/403 only logged. Should escalate to UI notification after N consecutive auth failures.

### 3. GPU / VRAM

- **G13 SERIOUS** — No GPU OOM classification. Generic error path; backend can't distinguish "model too big" from "transient pressure."
- **G14 SERIOUS** — `nvidia-smi` 5s timeout silently → `gpu_status = {}`. No "driver hang detected" event.
- **G15 SERIOUS** — Multi-GPU `check_vram_available` returns first GPU, not slot. GPU 1 job rejected because GPU 0 full.
- **G16 IMPROVEMENT** — Daemon that started before driver came up reports unknown tier forever. No re-detect on subsequent heartbeats.
- **G17 IMPROVEMENT** — `gpu_usage_cap` collected by wizard, unused in daemon.
- **G18 IMPROVEMENT** — `temp_limit` collected by wizard, unused in daemon.

### 4. Model lifecycle

- **G19 BLOCKER** — Daemon update validates with **substring search** — `"DCP Provider Daemon" in candidate_code and "def main()" in candidate_code` (`:1306`). MITM, corrupted CDN, or partial backend response with those two strings passes. Backups exist but a malicious file with the right preamble installs and executes.
- **G20 SERIOUS** — Ollama pull blocking subprocess.run; no progress, no resumability surfacing.
- **G21 SERIOUS** — Disk-full mid-pull = generic "ollama pull failed". `check_disk_space` only runs pre-job, not pre-install.
- **G22 IMPROVEMENT** — Apple Silicon model swap `rm -rf`'s old HF cache inline. Race or reentry = corrupt cache, manual cleanup required.
- **G23 IMPROVEMENT** — `precache_models` actually loads to GPU before `del`/`empty_cache`. Tight-VRAM machines can race Ollama/vLLM.
- **G24 IMPROVEMENT** — Model-compatibility pre-flight only at startup; safe-context warnings logged, never escalated to backend or dashboard.

### 5. First-run / install UX

- **G25 SERIOUS** — Wizard state in React `useState` only. Mid-install close = full restart.
- **G26 SERIOUS** — `command_exists("ollama")` is yes/no. Old Ollama version silently used.
- **G27 SERIOUS** — Embedded Python install has no rollback. Half-installed state may be picked up on next launch (`python_exe.exists()` true after extract before pip wired).
- **G28 IMPROVEMENT** — "Already registered backend-side" not detected; wizard re-run can duplicate registration.
- **G29 IMPROVEMENT** — PDPL consent flow not wired into desktop register path (`register_provider`).

### 6. Observability / diagnostics

- **G30 BLOCKER** — `daemon.log` does not rotate (`:244-251` uses bare `FileHandler`). Compare to `_EVENTS_MAX_BYTES = 5MB` rotation done correctly for events.jsonl. GBs in weeks on busy provider.
- **G31 BLOCKER** — `compute_capability` is plumbed but never populated. nvidia-smi query at `:1201` excludes `compute_cap`. Plus `provider_tier` falls back to `"unknown"` on classify_provider_tier exception with no retry.
- **G32 SERIOUS** — Tray "View Logs" hardcoded `~/.dcp/daemon.log` ≠ daemon's actual `~/dc1-provider/logs/daemon.log` (`dcp_daemon.py:177`). Button opens empty/missing file.
- **G33 SERIOUS** — `upload_provider_logs` (`lib.rs:2861-2890`) not in `invoke_handler` list. **Dead code.**
- **G34 SERIOUS** — Dashboard cannot distinguish "down" vs "idle." Daemon emits no `paused`, `last_job_at`, `idle_reason` in heartbeat.
- **G35 IMPROVEMENT** — `tail_file` returns 20 lines. Often insufficient.
- **G36 IMPROVEMENT** — `report_event` payloads silently dropped if HMAC_SECRET missing.

### 7. State consistency

- **G37 BLOCKER** — No single-instance guard on Tauri app. Double-click → 2 .exe → both `full_start_provider` → mutual `kill_by_name("dcp_daemon.py")`. PID file race.
- **G38 SERIOUS** — `start_daemon_process` PID file read-then-write not atomic. Two-process race possible.
- **G39 SERIOUS** — `config.json` writes non-atomic via `std::fs::write`. Power loss mid-write = corrupt JSON, dashboard fails to start.
- **G40 SERIOUS** — `kill_by_name("dcp_daemon.py")` kills every daemon on machine (debug, second-account).
- **G41 IMPROVEMENT** — Dedup file rewritten in full every job (`:684`). Write amplification, not atomic-replaced.
- **G42 IMPROVEMENT** — Tauri force-quit leaves stale PID file; wizard `checkSetupComplete` doesn't check it.

### 8. Performance & resource consumption

- **G43 IMPROVEMENT** — Heartbeat is heavyweight: per-tick (30s) detect_gpu, get_gpu_info, get_model_cache_metrics, detect_vllm_models (multiple HTTP), classify_provider_tier, estimate_concurrency_capacity, detect_model_architecture per model, get_turboquant_config, get_cpu_offload_state, plus several subprocess shellouts (100-300ms each). 1-2s of work / 30s = 3-7% CPU wasted in instrumentation on busy 4-engine setup.
- **G44 IMPROVEMENT** — `concurrency_reprobe_loop` (6h) fires real inference traffic — wasted GPU-seconds on idle providers; competes with paid traffic on busy providers.
- **G45 IMPROVEMENT** — Dedup file FIFO eviction policy unverified; backend re-issuing old IDs after cutoff = false re-execution.
- **G46 NICE-TO-HAVE** — Tauri Dashboard polling cadence shells out to nvidia-smi/tasklist every poll.

### 9. Pause / resume / scheduled

- **G47 BLOCKER** — `is_paused` is backend-only. Daemon does not enforce. Zero matches in `dcp_daemon.py` for `is_paused|paused|run_mode|schedule`.
- **G48 BLOCKER** — `run_mode = "scheduled"` and `"idle"` collected, never implemented. All three modes behave identically.
- **G49 SERIOUS** — Currently-running job behavior on pause undefined. Backend pause cannot kill in-flight Docker job.
- **G50 IMPROVEMENT** — No timezone handling for scheduled mode.

### 10. Update path correctness

- **G51 SERIOUS** — Tauri auto-update and daemon auto-update are decoupled. No version-skew compat check at startup.
- **G52 SERIOUS** — Tauri auto-updates on every launch, downloads + installs + relaunches with no user gate, no rollback, no version pin.
- **G53 SERIOUS** — Daemon-vs-backend API skew not checked. Persistent 4xx loop possible.
- **G54 IMPROVEMENT** — Watchdog rollback path logically correct but `installers/tests/` has no `test_rollback`. Untested.

### 11. Operator ergonomics

- **G55 SERIOUS** — Tray "View Logs" wrong path (see G32).
- **G56 SERIOUS** — Tray Pause/Resume = no-op. Match arm at `lib.rs:2952-2972` only handles `show`, `dashboard`, `logs`, `quit`.
- **G57 SERIOUS** — Tray earnings/status static placeholders. No update loop.
- **G58 IMPROVEMENT** — `tauri-plugin-notification` initialized (`:2909`) but never invoked. No notifications for: job completion, daemon crash, update applied, provider approved, low disk space.
- **G59 IMPROVEMENT** — Tray tooltip static "DCP Provider — Running."
- **G60 IMPROVEMENT** — Tray Quit calls `app.exit(0)` — daemon's drain logic skipped.

### 12. Cross-platform parity

- **G61 SERIOUS** — Linux `.desktop` / autostart unimplemented; no Linux integration tests.
- **G62 SERIOUS** — macOS Intel "untested limited mode." Warning string only, no gating; defaults to Ollama+qwen3:8b CPU-only.
- **G63 IMPROVEMENT** — `python_cmd()` selection on Windows 11 may pick MS Store stub `python.exe`. Known footgun.

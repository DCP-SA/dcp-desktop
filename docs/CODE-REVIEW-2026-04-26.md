# Tauri Code Review — dcp-desktop @ v0.2.1

**Date:** 2026-04-26
**Reviewer:** Claude (Opus 4) — automated deep review
**Scope:** Full Tauri 2.0 desktop app (Rust + React/TS), excluding the cmd.exe cascade fix already shipped in 0.2.1.

## Summary

29 findings: **2 CRITICAL, 9 HIGH, 11 MEDIUM, 7 LOW**.

Top three to fix immediately:
1. `shell:allow-execute` capability with no scope + `csp: null` = XSS-to-RCE primitive (C1).
2. Silent auto-updater with no error path or user gating = bricks installs on bad bundles (C2).
3. Daemon + Python embed + cloudflared downloaded and executed with zero integrity verification = supply-chain RCE on the entire fleet (H6/H7).

## Phased remediation plan

| Phase | Target release | Bundle | Risk |
|---|---|---|---|
| 1 | 0.2.2 | Quick surgical hardening | Low |
| 2 | 0.3.0 | Async runtime hygiene | Medium |
| 3 | Separate workstream | Security hardening (signing infra, keychain) | High |
| 4 | Drop into any release | Polish | Trivial |

### Phase 1 — 0.2.2 (after Fadi confirms 0.2.1 cascade fix)

- C1 — Tighten `shell:allow-execute` scope; add CSP.
- H8 — Remove `register_provider` + `validate_api_key` from `invoke_handler` (fake key generator).
- L5 — Validate `model_name` against `^[A-Za-z0-9._/-]+$` before passing to Python `-c`.
- M11 — Atomic write for daemon download (temp + rename).
- M7 — `OpenOptions::append` for `daemon.log` and `daemon_error.log`.

### Phase 2 — 0.3.0

- H1 — `std::sync::Mutex` → `tokio::sync::Mutex` (or tight non-await scopes).
- H2/H3 — `std::thread::sleep` → `tokio::time::sleep` in async paths (lib.rs:1313, 1327, 2186, 2221, 2279, 2330, 2338, 2610).
- H4 — Sync `Command::output()` in async handlers → `tokio::process::Command` or `spawn_blocking`.
- M3 — Replace `let _ = ...` with logged errors on critical paths (~25 sites).
- M5 — Track child PIDs in `DaemonState`; remove broad `pkill -f`.
- M6 — `tail_file` should seek from end (4–16 KB) instead of reading whole file.
- M9 — macOS cloudflared: extract `.tgz` before exec.
- M1 — Shared `reqwest::Client` in Tauri State.

### Phase 3 — Security hardening (backend coordination required)

- H6/H7 — Sign daemon + cloudflared + python-embed bundles with minisign; verify signature in Rust before write/spawn.
- H5 — Move API key from URL query string to header on daemon download.
- C2 — Updater UX: gate behind user prompt, surface failures, support "skip this launch."
- Plaintext API key in `~/.dcp/config.json` → OS keychain (`keyring` crate).
- Add `tauri-plugin-single-instance`.

### Phase 4 — Polish

L1 (CSP — covered in Phase 1), L2 (separator3 reuse), L3 (EnumAdapters1 upper bound), L4 (ISO timestamp in startup.log), L6 (rename `validate_api_key`), L7 (Date validation in `fetchRecentJobs`).

---

## Full findings

### CRITICAL

#### C1 — `shell:allow-execute` granted with no scope
- **File:** `src-tauri/capabilities/default.json:9`
- **Problem:** Bare `shell:allow-execute` permission lets the WebView call `Command.execute()` for any binary. Combined with `security.csp: null` in `tauri.conf.json:30`, a single XSS or supply-chain compromise of a npm dep = OS-wide RCE on the user's account.
- **Fix:** Drop the permission (Rust commands handle every shell-out already), or add an explicit narrow allow-list. Set a real CSP: `default-src 'self'; connect-src 'self' https://api.dcp.sa`.

#### C2 — Auto-installing updater with no error path
- **File:** `src/App.tsx:42–54`
- **Problem:** Every launch silently `check()` → `downloadAndInstall()` → `relaunch()` inside one `try/catch` whose only branch is `console.log`. Broken bundle, sig mismatch, partial download, or keychain refusal drops users into an infinite update loop with no UI escape hatch.
- **Fix:** Move logic to Rust side via `tauri-plugin-updater` events; user prompt before install; categorized error logs; "skip update for this launch" escape.

### HIGH

#### H1 — `std::sync::Mutex` held across `.await`
- **File:** `lib.rs:65` (DaemonState mutex), used in async commands at 1172–1278, 1282–1336, 1339–1404, 1867–2012, 2308–2854.
- **Problem:** `std::sync::Mutex` in async contexts; pattern is mostly "lock, copy, drop" but several handlers re-lock around `.await` calls. A future scheduler move with the guard alive deadlocks the IPC bridge.
- **Fix:** `tokio::sync::Mutex`, or enforce that every lock scope is closed before any `.await`.

#### H2 — `std::thread::sleep` blocking the async runtime
- **File:** lib.rs:1313, 1327, 2186, 2221, 2279, 2330, 2338, 2610
- **Problem:** Sleeps inside `async fn` bodies park the executor's worker thread. With 5s/10s/30s/60s polls running concurrently this can starve the runtime; user clicks Pause and UI freezes 10s.
- **Fix:** `tokio::time::sleep(...).await`.

#### H3 — Same pattern in `update_daemon` and `stop_daemon_process`
- **File:** lib.rs:2185–2192, 1311–1327
- **Problem:** Graceful-shutdown loops use `std::thread::sleep`. These are the most-called paths.
- **Fix:** `tokio::time::sleep`.

#### H4 — Sync `Command::output()` inside async handlers
- **File:** lib.rs:1480, 1586, 1668, 1704, 1885, 2027, 2056, 2107, 2128, 2627, 2668, 2717
- **Problem:** `std::process::Command::output()` blocks the calling thread. A 5-minute `pip install mlx` parks an async worker for the entire duration. Concurrent IPC during install/pull blocks; renderer's setInterval(5000) stacks behind it.
- **Fix:** `tokio::process::Command` with `.spawn().wait_with_output().await`, or wrap with `tokio::task::spawn_blocking`.

#### H5 — API key in URL query string
- **File:** lib.rs:1199, 2154, 2745
- **Problem:** `?key={apiKey}` ends up in proxy/CDN/server access logs.
- **Fix:** Header form (`x-api-key`) — already used elsewhere, just standardize.

#### H6 — Daemon downloaded over network and executed without integrity verification
- **File:** lib.rs:1199–1222 (`start_daemon_process`), 2154–2179 (`update_daemon`), 2742–2772 (`full_start_provider`)
- **Problem:** Python file fetched from `api.dcp.sa/.../daemon` and immediately spawned. No SHA-256, no signature. TLS protects transport, but backend compromise / DNS hijack / malicious admin = code execution on every provider machine. Written to user-writable `~/.dcp/dcp_daemon.py`, so any local user-process can swap it before launch.
- **Fix:** Sign daemon with minisign (already used for updater bundles); verify signature in Rust before `fs::write` and before `spawn`. Store under directory writable only by elevated process.

#### H7 — cloudflared / Ollama / Python embed / get-pip.py same pattern
- **File:** lib.rs:2073, 2228–2252, 2655–2664, 2705–2715
- **Problem:** Download to user-writable path, run as user. No checksum pinning. `get-pip.py` is especially sensitive (full pip privileges).
- **Fix:** Pin to specific release versions; verify SHA-256.

#### H8 — `register_provider` returns a fake API key
- **File:** lib.rs:636–666
- **Problem:** Returns `dcp-provider-<djb2(email)[..32]>` (the helper is named `md5_hash` but is djb2). `validate_api_key` (lib.rs:629–633) approves any string starting with `dcp-provider-` longer than 20 chars. TODO comment: "Replace with actual HTTP call." If any backend code path trusts this prefix shape for authorization, full bypass.
- **Fix:** Implement real HTTP register, or remove the command from `invoke_handler` until ready. Rename `md5_hash`.

#### H9 — `unwrap()` in `setup`
- **File:** lib.rs:2938 (`get_webview_window("main").unwrap()`), 2948 (`default_window_icon().unwrap()`), 3017 (`.expect("error while building tauri application")`)
- **Problem:** Silent crashes before logging is initialized if window config ever changes.
- **Fix:** `if let Some(win) = ...` with logged error event.

### MEDIUM

#### M1 — `reqwest::Client::new()` allocated per-call
- **File:** lib.rs:739, 794, 832, 887, 906, 1203, 2158, 2653, 2831
- **Fix:** Shared `Client` in Tauri State or `OnceLock<Client>`.

#### M2 — `unwrap()` inside command handlers on JSON serialization
- **File:** lib.rs:691, 2569, 2578, 2845
- **Fix:** Propagate as `Result`.

#### M3 — `let _ = ...` swallows critical errors
- **File:** ~25 sites, including lib.rs:293, 308, 324, 348, 993, 997, 1005, 1009, 1017, 1030, 2087, 2192, 2335, 2508, 2543, 2569, 2578, 2685, 2725, 2754, 2814, 2821, 2832–2838, 2845
- **Problem:** Critical operations like tunnel-URL registration with backend, config saves, old-daemon kills, installer cleanup — all silently lose `Result`. Hard to diagnose.
- **Fix:** Log errors at minimum; surface tunnel registration failures to UI.

#### M4 — PID file race
- **File:** lib.rs:1148–1167
- **Problem:** No file lock; two desktop instances can write same PID file, see each other's daemon, skip spawn while state diverges. No fsync after write.
- **Fix:** `flock` (Unix) / share-mode none (Windows), or `tauri-plugin-single-instance`.

#### M5 — Broad `pkill -f` / `wmic ... CommandLine like '%pattern%'`
- **File:** lib.rs:2324–2325 (`full_start_provider` Step 0)
- **Problem:** Will kill any process whose command line contains `dcp_daemon.py` or `mlx_lm.server` (developer's editor previewing the file, etc.).
- **Fix:** Track spawned child PIDs in `DaemonState`.

#### M6 — `tail_file` reads entire file each poll
- **File:** lib.rs:1137–1146
- **Problem:** With 5s metric polls and verbose MLX output, dashboard freezes after hours.
- **Fix:** Seek from end, read last 4–16 KB.

#### M7 — `daemon.log` truncated on every start
- **File:** lib.rs:1228, 2776
- **Problem:** Loses post-mortem context exactly when needed.
- **Fix:** `OpenOptions::new().append(true).create(true)`.

#### M8 — `python_cmd()` Windows leak: unsafe `&'static str` from `OnceLock<String>`
- **File:** lib.rs:1118–1124
- **Problem:** Functionally OK because OnceLock keeps String for process lifetime, but unsafe transmute bypasses lifetime checking.
- **Fix:** Return `String` (cloned).

#### M9 — `find_nvidia_smi` macOS cloudflared broken
- **File:** lib.rs:2231–2235
- **Problem:** Both Apple Silicon / Intel branches return same `cloudflared-darwin-amd64.tgz`. Downloads `.tgz` but never extracts before spawn — won't run on macOS at all.
- **Fix:** Extract with tar before exec.

#### M10 — Dashboard mock-feed setTimeout chain leaks
- **File:** `src/components/Dashboard.tsx:528–541`
- **Problem:** `scheduleNext` reassigns `timerRef.current` inside callback, but cleanup `clearTimeout(timerRef.current)` runs only with original handle captured at mount. After first reschedule, leak until tab close.
- **Fix:** Clear current ref value in cleanup.

#### M11 — Updater + auto-relaunch races with daemon-write
- **File:** `src/App.tsx:42–54` + lib.rs:2742–2772
- **Problem:** If `full_start_provider` is mid-download of `dcp_daemon.py` when updater fires `relaunch()`, get a corrupt half-written daemon file. Next launch executes it.
- **Fix:** Updater check before any provider start; daemon write via temp-file + rename.

### LOW

#### L1 — `csp: null`
- **File:** `tauri.conf.json:30`
- **Fix:** `default-src 'self'; connect-src 'self' https://api.dcp.sa`.

#### L2 — Tray menu reuses `separator3`
- **File:** lib.rs:2932
- **Fix:** Build a `separator4`.

#### L3 — `EnumAdapters1` loop has no upper bound guard
- **File:** lib.rs:373–420
- **Fix:** `if i > 64 { break; }` defensively.

#### L4 — `chrono_now` returns seconds-since-epoch as a string
- **File:** lib.rs:957–962
- **Fix:** ISO date for human-facing logs.

#### L5 — Python `-c` string injection via `model_name`
- **File:** lib.rs:2128–2132 — `format!("from mlx_lm import load; load('{}')", model_name)`
- **Problem:** Quote in name = parse error; malicious quote = code injection.
- **Fix:** Validate `model_name` against `^[A-Za-z0-9._/-]+$` or pass via argv.

#### L6 — `validate_api_key` is trivial
- **File:** lib.rs:629–633
- **Fix:** Rename, or actually verify with backend.

#### L7 — `fetchRecentJobs` Date parse without validation
- **File:** `src/components/Dashboard.tsx:483–495`
- **Fix:** Validate ISO format before construction.

## Notes / observations (non-bugs)

- No `println!`/`eprintln!` in lib.rs; structured logs via files in `~/.dcp/` and `upload_provider_logs` POST. No `log`/`tracing` crate — server-side correlation has to come from those files.
- Identifier `sa.dcp.provider` is sane.
- `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` correct in main.rs:2.
- Single-instance not enforced (no `tauri-plugin-single-instance`).
- Window-close hides instead of quits (lib.rs:2940–2945) — confirmed; tray quit menu wired (lib.rs:2969).
- No `dangerouslySetInnerHTML` anywhere in `src/`.
- No `localStorage` usage in `src/`.
- API key persisted plaintext in `~/.dcp/config.json` (lib.rs:683–692, 933–942) — readable by any user-process.
- `tauri-plugin-store` permissions broad but plugin not actually used by the front-end. Drop most allow-* entries when removing the plugin.
- No `cargo test` suite exists.
- Frontend `useEffect` polling cleanups are correct everywhere except M10.
- `RegistrationResult` is in the public IPC surface but only `register_provider` returns it, and that command is mocked (H8).

# Wizard Install Progress — Technical Spec

**Date:** 2026-04-27
**Owner:** Peter
**Component:** dcp-desktop-installer-fix (Tauri desktop app)
**Trigger:** Fadi 2026-04-27 session — provider's only feedback during a 2-5 min model download was a single spinning indicator with the literal text "Downloading...". No model name, no size, no ETA, no per-substep status. Errors were ambiguous because download/install were collapsed into one step.

## Goal

Replace the wizard's single-spinner "Downloading AI model" experience with granular, real-time progress: model name, model size in GB, live percent, current throughput in MB/s, and a refining ETA. Split download and install/verify into distinct sub-steps so a failure points to one specific cause instead of "something during model setup."

## Scope (in)

- Wizard `Installing.tsx` step: live percent + size + ETA + sub-steps.
- Hybrid speed test (option C) — pre-flight 10 MB probe followed by passive refinement from real download throughput.
- Tauri events from Rust → React for live progress.
- Per-sub-step error surface so logs and UI both pinpoint the failing phase.
- Console-window suppression sweep on Windows (any `Command::new` not yet wrapped in `hide_window()`).
- Engine-aware step labels (the wizard's `Installing.tsx` already does this correctly; out of scope here).

## Scope (out)

- `Dashboard.tsx` post-install startup overlay's hardcoded `"MLX installed"` (separate component, separate fix).
- Tier 3.14 G33 remote log fetch (already tracked).
- Replacing `OllamaSetup.exe` with `ollama-windows-amd64.zip` to remove the Ollama tray + welcome browser tab (separate, larger change — flagged but deferred).

## Architecture

Three layers, top to bottom.

### Layer 1 — Rust progress emitter (`src-tauri/src/lib.rs`)

Convert two long-running blocking spawns into streaming spawns and emit Tauri events.

**Affected sites:**
1. **Ollama installer download** (`reqwest::get(...)` at lib.rs:2636). Currently buffers the entire 1.85 GB into memory before writing. Switch to a streaming response: read chunks, write to file, emit `wizard:progress` every ~250ms with bytes-downloaded / total-bytes / mbps.
2. **`ollama pull <model>`** (lib.rs:2816). Currently `Command::new(&ollama_cmd()).args(["pull", &model]).output()` — blocks until done with no stdout visibility. Switch to `Command::spawn()` with stdout piped, parse Ollama's progress lines (format: `pulling abc123...  X% Y MB/Z MB`), emit `wizard:progress` events.
3. **Final verification** (lib.rs:3083 area, the `/api/tags` GET). Already short — emit start/done events only.

**New Tauri command — `pre_install_speed_probe()`:**
- Issues an HTTP GET with `Range: bytes=0-10485759` (10 MB) against `https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe` — same CDN we'll subsequently pull from, so the measurement reflects real conditions.
- Reads the streaming response, times it, returns `{ mbps, sample_bytes, elapsed_ms }`.
- 5-second hard timeout. Returns `null` mbps on timeout/error so frontend can degrade gracefully.

**New Tauri command — `get_model_metadata(model_id: String)`:**
- Returns `{ display_name, size_gb }` from a hardcoded constant table.
- Table:
  ```
  qwen3:30b-a3b           → "Qwen3 30B-A3B",  17.7 GB
  qwen3:8b                → "Qwen3 8B",        5.2 GB
  qwen3:4b                → "Qwen3 4B",        2.5 GB
  mistral:7b              → "Mistral 7B",      4.1 GB
  mlx-community/Qwen3-30B-A3B-4bit → "Qwen3 30B-A3B (MLX)", 16.4 GB
  mlx-community/Qwen3-8B-4bit       → "Qwen3 8B (MLX)",      4.5 GB
  mlx-community/Qwen3-4B-4bit       → "Qwen3 4B (MLX)",      2.3 GB
  ```
- Fallback for unknown models: `{ display_name: model_id, size_gb: null }`.

**Event payload (typed):**

```rust
#[derive(serde::Serialize, Clone)]
struct WizardProgress {
    step_id: &'static str,        // "ollama_download" | "ollama_install" | "model_download" | "model_verify"
    status: &'static str,         // "active" | "done" | "error"
    pct: Option<f32>,             // 0.0..100.0
    mb_done: Option<f64>,
    mb_total: Option<f64>,
    mbps: Option<f64>,            // current rolling throughput (megabits/sec)
    eta_seconds: Option<u64>,
    detail: Option<String>,       // human-readable line, e.g. "Mistral 7B • 4.1 GB"
    error: Option<String>,        // only set when status == "error"
}
```

Emitter helper: `emit_wizard_progress(window: &Window, payload: WizardProgress)`. Always also writes the same fields to `startup.log` for offline diagnostics.

### Layer 2 — Frontend wizard (`src/components/Installing.tsx`)

Restructure `INITIAL_STEPS` from 6 → 8:

```
1. Detecting hardware
2. Speed test
3. Downloading Ollama          <- split from "Downloading inference engine"
4. Installing Ollama           <- split
5. Downloading model           <- split from "Downloading AI model"
6. Loading model               <- verify model registered with Ollama
7. Starting DCP daemon
8. Connecting to DCP network
```

(Drop "Running first benchmark" — fold a "verified" status into step 8.)

**Lifecycle:**

1. On mount, call `preInstallSpeedProbe()`.
   - If returns `mbps`, store it; mark step 2 done with `"<X> Mbps"`.
   - If returns `null`, mark step 2 done with `"unknown — measuring during download"`.
2. Look up `getModelMetadata(modelId)` to get name + size; show name/size in step 5's `pending`/`active` detail before download starts: `"Mistral 7B • 4.1 GB"`.
3. Compute initial ETA from `(size_gb * 8000) / probe_mbps` seconds; display under step 5 detail: `"Mistral 7B • 4.1 GB • ~3m20s @ 25 Mbps"`.
4. Subscribe via `listen<WizardProgress>("wizard:progress", ...)`.
5. As events arrive, update the matching step's detail string. ETA refines from rolling 5-second average of `mbps` once real data starts flowing.
6. On `status: "error"` — mark that step error and stop.

**Event-handling tip:** Throttle React state updates to ≤4/sec (250ms coalesce). Otherwise progress events 30-50/sec will thrash the render tree.

### Layer 3 — Console-window suppression sweep

Roughly two-thirds of `Command::new` sites (≈48 of 71) on Windows do not call the existing `hide_window()` helper at lib.rs:911. The visible offenders right now are the PowerShell `Expand-Archive` at lib.rs:2858 and any spawned `ollama list` / `ollama pull` calls. Sweep everything: every `Command::new(...)` that runs on Windows passes through `hide_window(&mut cmd)` unless we explicitly need a console (we don't, anywhere). This is mechanical: edit each call-site to chain `hide_window` before `.output()` / `.spawn()` / `.status()`.

## Testing

- Manual: full wizard run on Fadi's RTX 3060 Ti machine. Expectation:
  - Step 2 shows a real Mbps number after ≤5s.
  - Step 3 shows a moving percent + MB/s + ETA throughout the 1.85 GB Ollama download.
  - Step 4 ticks done within ~30s post-install (after the existing 30s `:11434` poll).
  - Step 5 shows `mistral:7b` (or whatever VRAM-selected model) name + size + live %.
  - Step 6 ticks done after `/api/tags` confirms.
  - No PowerShell window flashes anywhere during the run.
  - On a deliberate failure (e.g., disconnect network during step 5), step 5 shows error text — steps 1-4 stay green.
- Logging: tail `%LOCALAPPDATA%\dcp\startup.log` during the run, confirm every event line is present.

## Risk

- **Blocking → streaming refactor on Ollama installer download:** if the chunk loop hangs, install hangs forever. Mitigation: reuse the existing `reqwest` client with its 10s connect timeout, plus a 5-minute total deadline; on deadline expiry, fall back to current `.bytes()` blocking path.
- **`ollama pull` stdout parse fragility:** Ollama's progress format is not a stable API. Mitigation: parse defensively (regex `(\d+(?:\.\d+)?)\s*%`); on parse miss, still emit "active" events with elapsed time so the user sees motion.
- **Speed probe against GitHub CDN:** GitHub may rate-limit unauthenticated range requests. Mitigation: no auth, single 10 MB request per wizard run, return `null` on 429 and let the wizard proceed without an upfront ETA.
- **`hide_window` sweep volume:** ≈48 sites. Risk of touching async branches incorrectly. Mitigation: helper is a no-op on non-Windows; safe to apply to every site unconditionally. Tested by running the wizard end-to-end and watching for any console flash.

## Out-of-scope follow-ups

1. Replace `OllamaSetup.exe` with `ollama-windows-amd64.zip` to eliminate the Ollama tray + welcome-page browser tab on first install.
2. Surface `startup.log` tail as a "View Logs" panel in the wizard during install.
3. Apply the same progress UX to the post-install Dashboard.tsx startup overlay (currently hardcoded "MLX installed").

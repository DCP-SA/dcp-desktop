# Wizard Install Progress Implementation Plan

> **For agentic workers:** Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the wizard's single-spinner "Downloading AI model" with granular per-substep progress, model size + name, live percent, throughput, and ETA. Sweep Windows console-window leaks.

**Architecture:** Rust streaming spawns + Tauri events → React listener with rolling-average ETA. New `pre_install_speed_probe` and `get_model_metadata` Tauri commands. 6→8 wizard steps. `hide_window()` applied to every Windows `Command::new`.

**Tech Stack:** Rust + Tauri 2 + React + TypeScript. Existing patterns (lib.rs `hide_window` helper, `tauri::Window::emit`, React `listen` from `@tauri-apps/api/event`).

**Spec:** see `docs/2026-04-27-wizard-install-progress-spec.md`.

---

## File Structure

| File | Responsibility |
|---|---|
| `src-tauri/src/lib.rs` | Add event types, emitter helper, `pre_install_speed_probe` command, `get_model_metadata` command, model size table. Convert Ollama installer download + `ollama pull` to streaming + emit events. Sweep `hide_window` across all Windows `Command::new` sites. |
| `src-tauri/Cargo.toml` | Confirm `futures-util` available for streaming reqwest (likely already present); add if missing. |
| `src/lib/api.ts` | Add `preInstallSpeedProbe()` and `getModelMetadata(modelId)` invoke wrappers. Add `WizardProgress` TypeScript type. |
| `src/components/Installing.tsx` | Restructure 6→8 steps; subscribe to `wizard:progress`; throttle state updates; render size/ETA/Mbps in step details. |

---

## Task 1: Add event payload type and emitter helper (Rust)

**Files:**
- Modify: `src-tauri/src/lib.rs` — add types + helper near the top, after `hide_window()` (~line 920).

- [ ] **Step 1: Add `WizardProgress` struct and `emit_wizard_progress` helper**

Insert after the `hide_window` helpers (lib.rs:920):

```rust
#[derive(serde::Serialize, Clone, Default)]
pub struct WizardProgress {
    pub step_id: String,
    pub status: String,
    pub pct: Option<f32>,
    pub mb_done: Option<f64>,
    pub mb_total: Option<f64>,
    pub mbps: Option<f64>,
    pub eta_seconds: Option<u64>,
    pub detail: Option<String>,
    pub error: Option<String>,
}

fn emit_wizard_progress(window: &tauri::Window, payload: WizardProgress) {
    let line = format!(
        "[{}] [wizard step={} status={} pct={:?} mb={:?}/{:?} mbps={:?} eta={:?}] {}",
        chrono_now(),
        payload.step_id,
        payload.status,
        payload.pct,
        payload.mb_done,
        payload.mb_total,
        payload.mbps,
        payload.eta_seconds,
        payload.detail.clone().unwrap_or_default(),
    );
    if let Ok(home) = dcp_home() {
        let path = home.join("startup.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(&path) {
            use std::io::Write;
            let _ = writeln!(f, "{}", line);
        }
    }
    let _ = window.emit("wizard:progress", payload);
}
```

- [ ] **Step 2: Build**

Run: `cd src-tauri && cargo check 2>&1 | tail -30`
Expected: clean compile (or error pointing only at unused-warning/import that you can fix immediately).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(wizard): add WizardProgress event type + emit helper"
```

---

## Task 2: Add model size table + `get_model_metadata` Tauri command

**Files:**
- Modify: `src-tauri/src/lib.rs` — add table constant + command, register command.

- [ ] **Step 1: Add table constant**

Insert near other constants (top of file or near `dcp_home()`):

```rust
const MODEL_METADATA: &[(&str, &str, f64)] = &[
    ("qwen3:30b-a3b", "Qwen3 30B-A3B", 17.7),
    ("qwen3:8b", "Qwen3 8B", 5.2),
    ("qwen3:4b", "Qwen3 4B", 2.5),
    ("mistral:7b", "Mistral 7B", 4.1),
    ("mlx-community/Qwen3-30B-A3B-4bit", "Qwen3 30B-A3B (MLX)", 16.4),
    ("mlx-community/Qwen3-8B-4bit", "Qwen3 8B (MLX)", 4.5),
    ("mlx-community/Qwen3-4B-4bit", "Qwen3 4B (MLX)", 2.3),
];
```

- [ ] **Step 2: Add the command**

```rust
#[derive(serde::Serialize)]
struct ModelMetadata {
    display_name: String,
    size_gb: Option<f64>,
}

#[tauri::command]
fn get_model_metadata(model_id: String) -> ModelMetadata {
    for (id, name, size) in MODEL_METADATA {
        if *id == model_id {
            return ModelMetadata {
                display_name: name.to_string(),
                size_gb: Some(*size),
            };
        }
    }
    ModelMetadata { display_name: model_id, size_gb: None }
}
```

- [ ] **Step 3: Register in `tauri::Builder::default().invoke_handler(...)`**

Find the existing `tauri::generate_handler![...]` macro (around lib.rs:3282) and add `get_model_metadata` to the list.

- [ ] **Step 4: Build and commit**

```bash
cd src-tauri && cargo check 2>&1 | tail -15
git add src-tauri/src/lib.rs
git commit -m "feat(wizard): add get_model_metadata command + size table"
```

---

## Task 3: Add `pre_install_speed_probe` Tauri command

**Files:**
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Add the command**

```rust
#[derive(serde::Serialize)]
struct SpeedProbeResult {
    mbps: Option<f64>,
    sample_bytes: u64,
    elapsed_ms: u64,
}

#[tauri::command]
async fn pre_install_speed_probe() -> SpeedProbeResult {
    let url = "https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe";
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return SpeedProbeResult { mbps: None, sample_bytes: 0, elapsed_ms: 0 },
    };

    let started = std::time::Instant::now();
    let resp = client.get(url).header("Range", "bytes=0-10485759").send().await;
    let resp = match resp {
        Ok(r) => r,
        Err(_) => return SpeedProbeResult { mbps: None, sample_bytes: 0, elapsed_ms: started.elapsed().as_millis() as u64 },
    };
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(_) => return SpeedProbeResult { mbps: None, sample_bytes: 0, elapsed_ms: started.elapsed().as_millis() as u64 },
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let sample_bytes = bytes.len() as u64;
    let mbps = if elapsed_ms > 0 {
        Some((sample_bytes as f64 * 8.0) / (elapsed_ms as f64 * 1000.0)) // (bytes*8 bits) / (ms * 1000) = Mbps
    } else {
        None
    };
    SpeedProbeResult { mbps, sample_bytes, elapsed_ms }
}
```

- [ ] **Step 2: Register in `tauri::generate_handler![...]`**

- [ ] **Step 3: Build and commit**

```bash
cd src-tauri && cargo check 2>&1 | tail -15
git add src-tauri/src/lib.rs
git commit -m "feat(wizard): add pre_install_speed_probe command"
```

---

## Task 4: Convert Ollama installer download to streaming + emit progress

**Files:**
- Modify: `src-tauri/src/lib.rs:2636` (the `reqwest::get(...)` call inside the Windows `cfg` block of `full_start_provider`).

- [ ] **Step 1: Replace blocking `.bytes()` with chunked streaming**

The Tauri command needs `window: tauri::Window` access. `full_start_provider` already has `state: State<'_, DaemonManager>` — add a `window: tauri::Window` parameter to the command signature and propagate it.

Replace:
```rust
let response = reqwest::get("https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe")
    .await
    .map_err(|e| format!("Failed to download Ollama installer: {}", e))?;
let bytes = response.bytes().await
    .map_err(|e| format!("Failed to read Ollama installer bytes: {}", e))?;
std::fs::write(&installer_path, &bytes)
    .map_err(|e| format!("Failed to save OllamaSetup.exe: {}", e))?;
```

with:
```rust
emit_wizard_progress(&window, WizardProgress {
    step_id: "ollama_download".into(),
    status: "active".into(),
    detail: Some("Starting Ollama download (~1.85 GB)".into()),
    ..Default::default()
});
let response = reqwest::get("https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe")
    .await
    .map_err(|e| format!("Failed to download Ollama installer: {}", e))?;
let total = response.content_length();
let mut stream = response.bytes_stream();
use futures_util::StreamExt;
let mut file = std::fs::File::create(&installer_path)
    .map_err(|e| format!("Failed to create OllamaSetup.exe: {}", e))?;
let mut downloaded: u64 = 0;
let mut last_emit = std::time::Instant::now();
let started = std::time::Instant::now();
while let Some(chunk_res) = stream.next().await {
    let chunk = chunk_res.map_err(|e| format!("Download stream error: {}", e))?;
    use std::io::Write;
    file.write_all(&chunk).map_err(|e| format!("Write error: {}", e))?;
    downloaded += chunk.len() as u64;
    if last_emit.elapsed().as_millis() >= 250 {
        let elapsed_s = started.elapsed().as_secs_f64().max(0.001);
        let mbps = (downloaded as f64 * 8.0) / (elapsed_s * 1_000_000.0);
        let pct = total.map(|t| (downloaded as f32 / t as f32) * 100.0);
        let eta = total.and_then(|t| {
            let remaining = t.saturating_sub(downloaded) as f64;
            if mbps > 0.0 { Some((remaining * 8.0 / (mbps * 1_000_000.0)) as u64) } else { None }
        });
        emit_wizard_progress(&window, WizardProgress {
            step_id: "ollama_download".into(),
            status: "active".into(),
            pct,
            mb_done: Some(downloaded as f64 / 1_048_576.0),
            mb_total: total.map(|t| t as f64 / 1_048_576.0),
            mbps: Some(mbps),
            eta_seconds: eta,
            ..Default::default()
        });
        last_emit = std::time::Instant::now();
    }
}
emit_wizard_progress(&window, WizardProgress {
    step_id: "ollama_download".into(),
    status: "done".into(),
    detail: Some("Ollama installer downloaded".into()),
    ..Default::default()
});
```

Add `futures-util = "0.3"` to `src-tauri/Cargo.toml` if not already present.

- [ ] **Step 2: Wrap installer execution with progress events**

Right before `Command::new(&chosen_installer).args(["/VERYSILENT", ...])`:

```rust
emit_wizard_progress(&window, WizardProgress {
    step_id: "ollama_install".into(),
    status: "active".into(),
    detail: Some("Running Ollama installer (silent)".into()),
    ..Default::default()
});
```

After successful `:11434` poll-up:

```rust
emit_wizard_progress(&window, WizardProgress {
    step_id: "ollama_install".into(),
    status: "done".into(),
    detail: Some("Ollama running on :11434".into()),
    ..Default::default()
});
```

- [ ] **Step 3: Build and commit**

```bash
cd src-tauri && cargo check 2>&1 | tail -20
git add src-tauri/src/lib.rs src-tauri/Cargo.toml
git commit -m "feat(wizard): stream Ollama installer download + emit progress"
```

---

## Task 5: Convert `ollama pull` to streaming + emit progress

**Files:**
- Modify: `src-tauri/src/lib.rs:2814-2823` area.

- [ ] **Step 1: Replace blocking `.output()` with `.spawn()` + line-by-line stdout read**

Replace:
```rust
if !model_cached {
    let pull = Command::new(&ollama_cmd())
        .args(["pull", &model])
        .output()
        .map_err(|e| format!("Model pull failed: {}", e))?;
    if !pull.status.success() {
        let stderr = String::from_utf8_lossy(&pull.stderr).to_string();
        return Err(format!("ollama pull {} failed: {}", model, stderr));
    }
}
```

with:
```rust
if !model_cached {
    emit_wizard_progress(&window, WizardProgress {
        step_id: "model_download".into(),
        status: "active".into(),
        detail: Some(format!("Pulling {}", model)),
        ..Default::default()
    });
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let mut cmd = Command::new(&ollama_cmd());
    cmd.args(["pull", &model]).stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("Model pull spawn failed: {}", e))?;
    let stdout = child.stdout.take().ok_or_else(|| "no stdout pipe".to_string())?;
    let started = std::time::Instant::now();
    let mut last_emit = std::time::Instant::now();
    let pct_re = regex::Regex::new(r"(\d+(?:\.\d+)?)\s*%").ok();
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if last_emit.elapsed().as_millis() >= 250 {
            let pct = pct_re.as_ref().and_then(|r| r.captures(&line))
                .and_then(|c| c.get(1)).and_then(|m| m.as_str().parse::<f32>().ok());
            emit_wizard_progress(&window, WizardProgress {
                step_id: "model_download".into(),
                status: "active".into(),
                pct,
                detail: Some(line.chars().take(120).collect()),
                ..Default::default()
            });
            last_emit = std::time::Instant::now();
        }
    }
    let status = child.wait().map_err(|e| format!("Model pull wait failed: {}", e))?;
    if !status.success() {
        let _ = started; // suppress unused if no error path uses it
        emit_wizard_progress(&window, WizardProgress {
            step_id: "model_download".into(),
            status: "error".into(),
            error: Some(format!("ollama pull {} exited non-zero", model)),
            ..Default::default()
        });
        return Err(format!("ollama pull {} failed", model));
    }
    emit_wizard_progress(&window, WizardProgress {
        step_id: "model_download".into(),
        status: "done".into(),
        detail: Some(format!("{} downloaded", model)),
        ..Default::default()
    });
}
```

Confirm `regex` is already in `Cargo.toml` (it is — used elsewhere). If not, add `regex = "1"`.

- [ ] **Step 2: Wrap final `/api/tags` verification with `model_verify` events**

Around lib.rs:3083 (the `body.contains(&model)` check):

Before the GET: emit `step_id: "model_verify"` `status: "active"` `detail: "Verifying model registered with Ollama"`.
After success: `status: "done"` `detail: "Model verified"`.
On failure: `status: "error"` with the error string.

- [ ] **Step 3: Build and commit**

```bash
cd src-tauri && cargo check 2>&1 | tail -15
git add src-tauri/src/lib.rs
git commit -m "feat(wizard): stream ollama pull + emit progress; add model_verify events"
```

---

## Task 6: Frontend invoke wrappers + types (TypeScript)

**Files:**
- Modify: `src/lib/api.ts`

- [ ] **Step 1: Add types and wrappers**

Append:

```ts
export interface WizardProgress {
  step_id: "ollama_download" | "ollama_install" | "model_download" | "model_verify" | string;
  status: "active" | "done" | "error";
  pct?: number | null;
  mb_done?: number | null;
  mb_total?: number | null;
  mbps?: number | null;
  eta_seconds?: number | null;
  detail?: string | null;
  error?: string | null;
}

export interface SpeedProbeResult {
  mbps: number | null;
  sample_bytes: number;
  elapsed_ms: number;
}

export interface ModelMetadata {
  display_name: string;
  size_gb: number | null;
}

export async function preInstallSpeedProbe(): Promise<SpeedProbeResult> {
  return invoke<SpeedProbeResult>("pre_install_speed_probe");
}

export async function getModelMetadata(modelId: string): Promise<ModelMetadata> {
  return invoke<ModelMetadata>("get_model_metadata", { modelId });
}
```

- [ ] **Step 2: Commit**

```bash
git add src/lib/api.ts
git commit -m "feat(wizard): add TS wrappers for speed probe + model metadata"
```

---

## Task 7: Restructure `Installing.tsx` to 8 steps + subscribe to events

**Files:**
- Modify: `src/components/Installing.tsx`

- [ ] **Step 1: Add imports**

```ts
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  // existing...
  preInstallSpeedProbe,
  getModelMetadata,
  type WizardProgress,
} from "../lib/api";
```

- [ ] **Step 2: Replace `INITIAL_STEPS` with the 8-step list**

```ts
const INITIAL_STEPS: InstallStep[] = [
  { label: "Detecting hardware...", status: "done" },
  { label: "Speed test", detail: "", status: "pending" },
  { label: "Downloading Ollama", detail: "", status: "pending" },
  { label: "Installing Ollama", detail: "", status: "pending" },
  { label: "Downloading model", detail: "", status: "pending" },
  { label: "Loading model", detail: "", status: "pending" },
  { label: "Starting DCP daemon", detail: "", status: "pending" },
  { label: "Connecting to DCP network", detail: "", status: "pending" },
];
```

(For Apple Silicon — engine is MLX. Step labels remain accurate enough; consider a follow-up to swap "Ollama" → "MLX" via per-platform render, but for now Windows is where Fadi is. Apple flow already works.)

- [ ] **Step 3: Pre-flight: speed probe + model metadata before kicking off `fullStartProvider`**

Inside the `runInstall` async function, before the `markStep(0, ...)` call:

```ts
markStep(1, "active", "Measuring connection speed...");
const probe = await preInstallSpeedProbe();
let probedMbps: number | null = probe.mbps;
markStep(1, "done", probedMbps != null ? `${probedMbps.toFixed(1)} Mbps` : "skipped");

// Look up model metadata
const modelId = isApple
  ? (vramGb >= 16 ? "mlx-community/Qwen3-8B-4bit" : "mlx-community/Qwen3-4B-4bit")
  : (vramGb >= 20 ? "qwen3:30b-a3b" : vramGb >= 8 ? "qwen3:8b" : "qwen3:4b");
const meta = await getModelMetadata(modelId);
const sizeStr = meta.size_gb != null ? `${meta.size_gb.toFixed(1)} GB` : "size unknown";
const etaStr = (meta.size_gb != null && probedMbps != null)
  ? formatEta((meta.size_gb * 8000) / probedMbps)
  : null;
markStep(4, "pending", etaStr ? `${meta.display_name} • ${sizeStr} • ~${etaStr} @ ${probedMbps!.toFixed(0)} Mbps` : `${meta.display_name} • ${sizeStr}`);
```

Add helper at module top:

```ts
function formatEta(seconds: number): string {
  if (!isFinite(seconds) || seconds <= 0) return "—";
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return m > 0 ? `${m}m${s.toString().padStart(2, "0")}s` : `${s}s`;
}
```

- [ ] **Step 4: Subscribe to `wizard:progress` with throttling**

Inside `useEffect`, set up a listener that maps `step_id` → step index:

```ts
const stepIdToIndex: Record<string, number> = {
  ollama_download: 2,
  ollama_install: 3,
  model_download: 4,
  model_verify: 5,
};

let lastApply = 0;
let pending: WizardProgress | null = null;
let unlisten: UnlistenFn | undefined;

const apply = (p: WizardProgress) => {
  const idx = stepIdToIndex[p.step_id];
  if (idx == null) return;
  let detail = p.detail ?? "";
  if (p.pct != null) {
    const mb = p.mb_done != null && p.mb_total != null
      ? ` ${p.mb_done.toFixed(0)}/${p.mb_total.toFixed(0)} MB`
      : "";
    const mbps = p.mbps != null ? ` @ ${p.mbps.toFixed(1)} Mbps` : "";
    const eta = p.eta_seconds != null ? ` • ETA ${formatEta(p.eta_seconds)}` : "";
    detail = `${p.pct.toFixed(0)}%${mb}${mbps}${eta}`;
  }
  if (p.status === "error") {
    markStep(idx, "error", p.error ?? detail);
  } else if (p.status === "done") {
    markStep(idx, "done", detail || (p.detail ?? ""));
  } else {
    markStep(idx, "active", detail);
  }
};

listen<WizardProgress>("wizard:progress", (event) => {
  pending = event.payload;
  const now = Date.now();
  if (now - lastApply >= 250) {
    lastApply = now;
    if (pending) { apply(pending); pending = null; }
  } else {
    setTimeout(() => {
      if (pending) { apply(pending); pending = null; lastApply = Date.now(); }
    }, 250 - (now - lastApply));
  }
}).then((fn) => { unlisten = fn; });
```

In the `return () => { cancelled = true; }` cleanup, also call `unlisten?.()`.

- [ ] **Step 5: Adjust the post-`fullStartProvider` step indices**

After `fullStartProvider` returns successfully, the live events have already advanced steps 2-5. Then mark steps 6 (daemon) and 7 (network):

```ts
markStep(6, "done", parts.length >= 4 ? `Daemon running (PID ${parts[3]})` : "Daemon running");
markStep(7, "done", "Connected to api.dcp.sa");
```

- [ ] **Step 6: Build the frontend**

```bash
npm run build 2>&1 | tail -30
```

Expected: clean build.

- [ ] **Step 7: Commit**

```bash
git add src/components/Installing.tsx src/lib/api.ts
git commit -m "feat(wizard): 8-step install flow + live progress + ETA"
```

---

## Task 8: `hide_window()` sweep across all Windows `Command::new` sites

**Files:**
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: List every `Command::new` site**

Run: `grep -nE "Command::new" src-tauri/src/lib.rs`

Expected: 71 hits.

- [ ] **Step 2: For each hit not already followed by `hide_window`, wrap it**

Pattern transform — wherever you see:

```rust
let x = Command::new(...).args([...]).output()...;
```

Change to:

```rust
let mut cmd_x = Command::new(...);
cmd_x.args([...]);
hide_window(&mut cmd_x);
let x = cmd_x.output()...;
```

For builder-chain calls, this is a mechanical refactor. Skip sites already inside `hide_window(&mut cmd)` blocks. Skip the `kill_by_name` helper if it already wraps the spawn (verify by grep).

Specific known un-hidden sites to fix first:
- lib.rs:2858 (`Command::new("powershell")` Expand-Archive) — **highest user impact, fix first**
- lib.rs:2646 (`Command::new(&chosen_installer)` OllamaSetup.exe) — silent installer flags suppress the GUI but `hide_window` still applies for completeness
- lib.rs:2810 (`Command::new(&ollama_cmd()).args(["list"])`) — list output
- lib.rs:2907 (`Command::new(&python_exe).arg(&getpip_path)`)
- lib.rs:2918 (`Command::new(&python_exe).args(["-m", "pip", ...])`)

- [ ] **Step 3: Verify no Command::new on Windows path is naked**

Run: `grep -nE "Command::new" src-tauri/src/lib.rs | wc -l` and `grep -nE "hide_window\(" src-tauri/src/lib.rs | wc -l`

Goal: hide_window count ≥ Command::new count minus the obviously-Unix-only sites (e.g., `sh -c curl ...` install). Cross-check by re-reading any remaining naked sites and confirming they are `#[cfg(unix)]` only.

- [ ] **Step 4: Build**

```bash
cd src-tauri && cargo check 2>&1 | tail -10
```

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "fix(windows): hide_window sweep across all Command::new sites"
```

---

## Task 9: End-to-end verification

- [ ] **Step 1: Build full Tauri app**

```bash
cd /Users/pp/DC1-Platform/dcp-desktop-installer-fix
npm run build
cd src-tauri && cargo build --release 2>&1 | tail -10
```

- [ ] **Step 2: Manual smoke (Mac)**

Run `npm run tauri dev`. Walk the wizard. Confirm:
- Step 2 shows a Mbps number after probe.
- Step 5 shows model name + size + initial ETA before download starts.
- During Ollama download (or model pull on macOS), step details update with %, MB done, MB/s, ETA.
- No console flashes (Mac n/a but verify nothing breaks).

- [ ] **Step 3: Manual smoke (Windows VM or Fadi machine if available)**

Same checks plus:
- No PowerShell window flashes during Python embed extract.
- Confirm `%LOCALAPPDATA%\dcp\startup.log` contains `[wizard step=...]` lines for every event.

- [ ] **Step 4: Push branch + open PR**

```bash
git push -u origin peter/wizard-install-progress
gh pr create --title "wizard: live install progress + 8-step granularity + Windows console hide sweep" --body "$(cat <<'EOF'
## Summary
- Wizard's "Downloading AI model" goes from a single spinner to 8 granular sub-steps with live %, MB/s, and ETA.
- Hybrid speed test (option C): pre-flight 10 MB probe against the same CDN we're about to download from, then passive refinement from real download throughput.
- Per-substep error surface: download vs. install vs. verify each report independently.
- Windows console-window hide sweep across all \`Command::new\` sites — kills the PowerShell window pop during Python embed install.

## Spec
\`docs/2026-04-27-wizard-install-progress-spec.md\`

## Test plan
- [ ] Mac: \`npm run tauri dev\`, walk wizard end-to-end, confirm steps 2-5 show live data.
- [ ] Windows (Fadi RTX 3060 Ti): full uninstall → reinstall → wizard, confirm no PS window, confirm progress events.
- [ ] Tail \`%LOCALAPPDATA%\\dcp\\startup.log\`, confirm every \`[wizard step=...]\` line is present.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Notes for the executing subagent

- TDD is hard here because this is UI + Tauri events + Rust streaming — most behavior is observed at runtime, not unit-tested. Lean on `cargo check` after every Rust task and `npm run build` after every TS task.
- Do not amend commits. Make new commits per task, even small ones.
- If the Ollama installer download streaming refactor breaks `cargo check`, revert that single change and fall back to chunk-buffer-then-write (collect to `Vec<u8>` first, then `std::fs::write`) — still streams from the network but loses per-chunk emit. This is acceptable as a fallback.
- If `regex` is not in `Cargo.toml`, the `ollama pull` parse can use a simple `line.find('%')` substring scan instead — no external dep needed. Prefer the simpler approach.
- Skip the brainstorming/spec-review ceremony — spec is locked, plan is locked, execute.

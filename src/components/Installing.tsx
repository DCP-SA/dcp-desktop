import { useState, useEffect } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  startDaemon,
  getEstimatedEarnings,
  fullStartProvider,
  preInstallSpeedProbe,
  getModelMetadata,
  type WizardProgress,
} from "../lib/api";
import type { GpuInfo, DaemonConfig } from "../lib/api";

interface InstallingProps {
  apiKey: string;
  config: DaemonConfig;
  gpu: GpuInfo | null;
  onComplete: () => void;
}

interface InstallStep {
  label: string;
  detail?: string;
  status: "pending" | "active" | "done" | "error";
}

const INITIAL_STEPS: InstallStep[] = [
  { label: "Detecting hardware", status: "done" },
  { label: "Speed test", detail: "", status: "pending" },
  { label: "Downloading engine", detail: "", status: "pending" },
  { label: "Installing engine", detail: "", status: "pending" },
  { label: "Downloading AI model", detail: "", status: "pending" },
  { label: "Verifying model", detail: "", status: "pending" },
  { label: "Starting DCP daemon", detail: "", status: "pending" },
  { label: "Connecting to DCP network", detail: "", status: "pending" },
];

function formatEta(seconds: number): string {
  if (!isFinite(seconds) || seconds <= 0) return "—";
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return m > 0 ? `${m}m${s.toString().padStart(2, "0")}s` : `${s}s`;
}

export function Installing({ apiKey, config, gpu, onComplete }: InstallingProps) {
  const [steps, setSteps] = useState<InstallStep[]>(INITIAL_STEPS);
  const [complete, setComplete] = useState(false);
  const [earnings, setEarnings] = useState(0);
  const [error, setError] = useState("");
  const [logLines, setLogLines] = useState<string[]>([]);
  const [showLog, setShowLog] = useState(false);

  useEffect(() => {
    let cancelled = false;

    // markStep is defined outside runInstall so the event listener can also use it
    const markStep = (index: number, status: InstallStep["status"], detail?: string) => {
      setSteps((prev) => {
        const next = [...prev];
        next[index] = { ...next[index], status, detail: detail ?? next[index].detail };
        return next;
      });
    };

    // ── Wizard progress event listener ──────────────────────────────
    const stepIdToIndex: Record<string, number> = {
      ollama_download: 2,
      ollama_install: 3,
      model_download: 4,
      model_verify: 5,
      daemon: 6,
      tunnel: 7,
    };

    let lastApply = 0;
    let pending: WizardProgress | null = null;
    let unlisten: UnlistenFn | undefined;

    const apply = (p: WizardProgress) => {
      const idx = stepIdToIndex[p.step_id];
      if (idx == null) return;
      let detail = p.detail ?? "";
      if (p.pct != null) {
        const mb =
          p.mb_done != null && p.mb_total != null
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
      // Accumulate log lines for the live log panel
      const p = event.payload;
      const timestamp = new Date().toLocaleTimeString();
      const line = `[${timestamp}] ${p.step_id}: ${p.detail || p.status}`;
      setLogLines(prev => [...prev, line]);

      pending = event.payload;
      const now = Date.now();
      if (now - lastApply >= 250) {
        lastApply = now;
        if (pending) {
          apply(pending);
          pending = null;
        }
      } else {
        setTimeout(() => {
          if (pending) {
            apply(pending);
            pending = null;
            lastApply = Date.now();
          }
        }, 250 - (now - lastApply));
      }
    }).then((fn) => {
      unlisten = fn;
    });

    async function runInstall() {
      // Defensive: if gpu detection failed, fall back to UA sniff so we don't
      // mislabel Windows/Linux as MLX. Only treat as Apple Silicon when the
      // backend says so AND the UA agrees we're on a Mac.
      const uaIsMac =
        typeof navigator !== "undefined" && /Mac/i.test(navigator.platform || "");
      const isApple = (gpu?.is_apple_silicon ?? false) && uaIsMac;
      const vramMb = gpu?.vram_mb ?? 0;
      const vramGb = Math.round(vramMb / 1024);

      // Step 1: Hardware (already done)
      markStep(0, "done", `${gpu?.name ?? "Unknown"} • ${vramGb} GB`);

      // Step 2: Speed probe
      markStep(1, "active", "Measuring connection speed...");
      let probedMbps: number | null = null;
      try {
        const probe = await preInstallSpeedProbe();
        probedMbps = probe.mbps;
      } catch {
        // Non-fatal — proceed without speed data
      }
      markStep(
        1,
        "done",
        probedMbps != null ? `${probedMbps.toFixed(1)} Mbps` : "skipped"
      );

      // Determine model ID matching backend logic
      const modelId = isApple
        ? vramGb >= 16
          ? "mlx-community/Qwen3-8B-4bit"
          : "mlx-community/Qwen3-4B-4bit"
        : vramGb >= 20
        ? "qwen3:30b-a3b"
        : vramGb >= 8
        ? "qwen3:8b"
        : "qwen3:4b";

      // Look up model metadata for size + ETA display
      let metaDisplayName = modelId;
      let metaSizeGb: number | null = null;
      try {
        const meta = await getModelMetadata(modelId);
        metaDisplayName = meta.display_name;
        metaSizeGb = meta.size_gb;
      } catch {
        // Non-fatal
      }
      const sizeStr =
        metaSizeGb != null ? `${metaSizeGb.toFixed(1)} GB` : "size unknown";
      const etaStr =
        metaSizeGb != null && probedMbps != null
          ? formatEta((metaSizeGb * 8000) / probedMbps)
          : null;
      markStep(
        4,
        "pending",
        etaStr
          ? `${metaDisplayName} • ${sizeStr} • ~${etaStr} @ ${probedMbps!.toFixed(0)} Mbps`
          : `${metaDisplayName} • ${sizeStr}`
      );

      if (cancelled) return;

      try {
        // fullStartProvider handles: kill old procs → engine → model → server → daemon
        // Events from Rust update steps 2-7 in real time via the listener above.
        // Daemon (step 6) and tunnel (step 7) now emit wizard:progress from Rust.
        await fullStartProvider(apiKey);
      } catch (e) {
        const errMsg = String(e);
        // Error events from Rust already mark the specific step; also set the
        // component-level error string so the user sees it below the list.
        if (
          errMsg.includes("MLX") ||
          errMsg.includes("Ollama") ||
          errMsg.includes("install")
        ) {
          markStep(2, "error", errMsg);
        } else if (
          errMsg.includes("model") ||
          errMsg.includes("pull") ||
          errMsg.includes("download")
        ) {
          markStep(4, "error", errMsg);
        } else if (errMsg.includes("daemon") || errMsg.includes("spawn")) {
          markStep(6, "error", errMsg);
        } else {
          markStep(4, "error", errMsg);
        }
        setError(errMsg);
        return;
      }

      if (cancelled) return;

      // Save config
      try {
        await startDaemon(apiKey, config);
      } catch {
        // Config save — non-fatal
      }

      // Get earnings estimate
      try {
        const est = await getEstimatedEarnings(vramMb, isApple);
        setEarnings(est);
      } catch {
        setEarnings(isApple ? 35 : 50);
      }

      // Wait for all wizard:progress events to flush through the 250ms throttle
      await new Promise((r) => setTimeout(r, 400));

      // Only show completion when every step is "done"
      setSteps((prev) => {
        const allDone = prev.every((s) => s.status === "done");
        if (allDone) {
          setComplete(true);
        } else {
          // Force any remaining pending steps to done (Rust already succeeded)
          const fixed = prev.map((s) =>
            s.status === "pending" || s.status === "active"
              ? { ...s, status: "done" as const }
              : s
          );
          setComplete(true);
          return fixed;
        }
        return prev;
      });
    }

    runInstall().catch((err) => {
      if (!cancelled) setError(String(err));
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [apiKey, config, gpu]);

  const progress = Math.round(
    (steps.filter((s) => s.status === "done").length / steps.length) * 100
  );

  return (
    <div className="step-container">
      {!complete ? (
        <>
          <h2 className="step-title">Installing DCP Provider</h2>
          <p className="step-subtitle">First install takes 2-5 minutes (downloading AI model)</p>

          <div className="install-steps">
            {steps.map((step, i) => (
              <div
                className={`install-step ${step.status === "active" ? "active" : ""} ${step.status === "done" ? "done" : ""}`}
                key={i}
              >
                <div className="install-step-icon">
                  {step.status === "done" && (
                    <span className="checkmark" aria-label="complete">&#10003;</span>
                  )}
                  {step.status === "active" && <span className="spinner" />}
                  {step.status === "pending" && <span className="pending" />}
                  {step.status === "error" && (
                    <span className="error-icon" aria-label="error">&#10007;</span>
                  )}
                </div>
                <span className="install-step-label">
                  {step.label}
                  {step.detail && step.status !== "pending" && (
                    <> &mdash; {step.detail}</>
                  )}
                </span>
              </div>
            ))}
          </div>

          <div className="install-log-toggle" onClick={() => setShowLog(!showLog)} style={{
            cursor: 'pointer', fontSize: '12px', color: '#6b7280', marginTop: '12px',
            userSelect: 'none'
          }}>
            {showLog ? "\u25BC Hide Log" : "\u25B6 Show Install Log"} ({logLines.length} entries)
          </div>
          {showLog && (
            <div className="install-log-panel" style={{
              maxHeight: '200px', overflow: 'auto', background: 'rgba(0,0,0,0.3)',
              borderRadius: '8px', padding: '12px', margin: '12px 0',
              fontFamily: 'monospace', fontSize: '11px', lineHeight: '1.6',
              color: '#9ca3af', userSelect: 'text'
            }}>
              {logLines.map((line, i) => <div key={i}>{line}</div>)}
            </div>
          )}

          <div className="progress-bar-container">
            <div className="progress-bar-track">
              <div
                className="progress-bar-fill"
                style={{ width: `${progress}%` }}
                role="progressbar"
                aria-valuenow={progress}
                aria-valuemin={0}
                aria-valuemax={100}
              />
            </div>
          </div>
        </>
      ) : (
        <>
          <div className="completion-card">
            <div className="completion-icon">&#10003;</div>
            <div className="completion-title">You're earning!</div>
            <div className="completion-earnings">
              ~${earnings}/month
            </div>
            <div className="completion-subtitle">
              Estimated earnings based on your hardware and current network demand
            </div>
          </div>

          <p className="text-sm text-muted mt-16 text-center">
            DCP is running in the background. Check the menu bar icon for status.
          </p>

          <div className="step-actions mt-16">
            <button
              className="btn btn-primary btn-large"
              onClick={onComplete}
            >
              Open Dashboard
            </button>
          </div>
        </>
      )}

      {error && (
        <div className="input-error mt-12">
          <p>Installation error: {error}</p>
          <p className="text-sm text-muted mt-4">Check ~/.dcp/startup.log for details</p>
          <p style={{fontSize: '11px', color: '#6b7280', marginTop: '8px'}}>
            Detailed log: ~/.dcp/startup.log
          </p>
        </div>
      )}
    </div>
  );
}

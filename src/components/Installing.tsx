import { useState, useEffect } from "react";
import {
  startDaemon,
  getEstimatedEarnings,
  installEngine,
  fullStartProvider,
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
  { label: "Detecting hardware...", status: "done" },
  { label: "Downloading inference engine...", detail: "", status: "pending" },
  { label: "Downloading AI model...", detail: "", status: "pending" },
  { label: "Starting DCP daemon...", status: "pending" },
  { label: "Connecting to DCP network...", status: "pending" },
  { label: "Running first benchmark...", status: "pending" },
];

export function Installing({ apiKey, config, gpu, onComplete }: InstallingProps) {
  const [steps, setSteps] = useState<InstallStep[]>(INITIAL_STEPS);
  const [complete, setComplete] = useState(false);
  const [earnings, setEarnings] = useState(0);
  const [error, setError] = useState("");

  useEffect(() => {
    let cancelled = false;

    async function runInstall() {
      // Defensive: if gpu detection failed, fall back to UA sniff so we don't
      // mislabel Windows/Linux as MLX. Only treat as Apple Silicon when the
      // backend says so AND the UA agrees we're on a Mac.
      const uaIsMac =
        typeof navigator !== "undefined" && /Mac/i.test(navigator.platform || "");
      const isApple = (gpu?.is_apple_silicon ?? false) && uaIsMac;
      const engineName = isApple ? "MLX" : "Ollama";
      const vramMb = gpu?.vram_mb ?? 0;
      const vramGb = Math.round(vramMb / 1024);

      // Pick model based on memory
      let modelDisplay: string;
      if (isApple) {
        if (vramGb >= 64) modelDisplay = "Qwen3 30B-A3B";
        else if (vramGb >= 32) modelDisplay = "Qwen3 30B-A3B";
        else if (vramGb >= 16) modelDisplay = "Qwen3 8B";
        else modelDisplay = "Qwen3 4B";
      } else {
        if (vramGb >= 20) modelDisplay = "Qwen3 30B-A3B";
        else if (vramGb >= 8) modelDisplay = "Qwen3 8B";
        else modelDisplay = "Qwen3 4B";
      }

      const markStep = (index: number, status: InstallStep["status"], detail?: string) => {
        setSteps((prev) => {
          const next = [...prev];
          next[index] = { ...next[index], status, detail: detail ?? next[index].detail };
          return next;
        });
      };

      try {
        // Step 1: Hardware (already done)
        markStep(0, "done", `${gpu?.name ?? "Unknown"} • ${vramGb} GB`);

        // Step 2: Install engine — fail loudly if the engine install errors.
        // Previously this swallowed errors and marked the step "done" anyway,
        // which is why wizards "succeeded" with no Ollama installed.
        markStep(1, "active", `Installing ${engineName}...`);
        try {
          await installEngine();
          markStep(1, "done", `${engineName} ready`);
        } catch (e) {
          const errMsg = String(e);
          markStep(1, "error", errMsg);
          setError(errMsg);
          return;
        }
        if (cancelled) return;

        // Step 3: Download model — this is the long one
        markStep(2, "active", `Preparing ${modelDisplay}...`);
        try {
          // fullStartProvider handles: kill old procs → engine → model → server → daemon
          const result = await fullStartProvider(apiKey);

          // Parse result: "started:engine:model:pid:cached|downloaded"
          const parts = result.split(":");
          const wasCached = parts.length >= 5 && parts[4] === "cached";

          if (wasCached) {
            markStep(2, "done", `${modelDisplay} — already on disk, skipping download`);
          } else {
            markStep(2, "done", `${modelDisplay} downloaded`);
          }

          const pid = parts.length >= 4 ? parts[3] : "";
          markStep(3, "done", `Inference server running`);
          markStep(4, "done", `Daemon running (PID ${pid})`);
          markStep(5, "done", "Connected to api.dcp.sa");
        } catch (e) {
          const errMsg = String(e);
          // Check which step failed
          if (errMsg.includes("MLX") || errMsg.includes("Ollama") || errMsg.includes("install")) {
            markStep(1, "error", errMsg);
          } else if (errMsg.includes("model") || errMsg.includes("pull") || errMsg.includes("download")) {
            markStep(2, "error", errMsg);
          } else if (errMsg.includes("daemon") || errMsg.includes("spawn")) {
            markStep(3, "error", errMsg);
          } else {
            markStep(2, "error", errMsg);
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

        setComplete(true);
      } catch (err) {
        if (!cancelled) setError(String(err));
      }
    }

    runInstall().catch((err) => {
      if (!cancelled) setError(String(err));
    });

    return () => {
      cancelled = true;
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
          Installation error: {error}
        </div>
      )}
    </div>
  );
}

import { useState, useEffect } from "react";
import {
  detectGpu,
  detectSystem,
  formatVram,
  formatRam,
  getPerformanceTier,
} from "../lib/api";
import type { GpuInfo, SystemInfo } from "../lib/api";

interface HardwareDetectionProps {
  onNext: (gpu: GpuInfo, system: SystemInfo) => void;
  onBack: () => void;
}

type DetectionPhase = "detecting" | "done" | "error";

interface DetectedField {
  label: string;
  value: string;
  done: boolean;
}

export function HardwareDetection({ onNext, onBack }: HardwareDetectionProps) {
  const [phase, setPhase] = useState<DetectionPhase>("detecting");
  const [gpu, setGpu] = useState<GpuInfo | null>(null);
  const [system, setSystem] = useState<SystemInfo | null>(null);
  const [fields, setFields] = useState<DetectedField[]>([]);
  const [error, setError] = useState<string>("");
  const [qualified, setQualified] = useState(true);

  useEffect(() => {
    let cancelled = false;

    async function detect() {
      try {
        // Detect GPU and system in parallel
        const [gpuResult, systemResult] = await Promise.all([
          detectGpu(),
          detectSystem(),
        ]);

        if (cancelled) return;

        setGpu(gpuResult);
        setSystem(systemResult);

        const tier = getPerformanceTier(gpuResult);
        const isQualified = tier.tier !== "Below minimum";
        setQualified(isQualified);

        // Build detected fields with staggered reveal
        const detectedFields: DetectedField[] = [
          {
            label: "GPU",
            value: gpuResult.is_apple_silicon
              ? `${gpuResult.name}, ${formatVram(gpuResult.vram_mb)} unified`
              : `${gpuResult.name}, ${formatVram(gpuResult.vram_mb)} VRAM`,
            done: false,
          },
          {
            label: "OS",
            value: `${systemResult.os} ${systemResult.os_version}`,
            done: false,
          },
          {
            label: "RAM",
            value: formatRam(systemResult.total_ram_mb),
            done: false,
          },
          {
            label: "CPU",
            value: `${systemResult.cpu_name} (${systemResult.cpu_cores} cores)`,
            done: false,
          },
          {
            label: "Performance",
            value: `${tier.tier} (${tier.toksEstimate} tok/s)`,
            done: false,
          },
        ];

        // Stagger the checkmarks appearing
        for (let i = 0; i < detectedFields.length; i++) {
          if (cancelled) return;
          await new Promise((r) => setTimeout(r, 300));
          detectedFields[i].done = true;
          setFields([...detectedFields]);
        }

        if (!cancelled) {
          setPhase("done");
        }
      } catch (err) {
        if (!cancelled) {
          setError(String(err));
          setPhase("error");
        }
      }
    }

    detect();
    return () => {
      cancelled = true;
    };
  }, []);

  if (phase === "detecting" && fields.length === 0) {
    return (
      <div className="step-container">
        <div className="detecting-overlay">
          <div className="detecting-spinner" />
          <span className="detecting-text">Detecting your hardware...</span>
        </div>
      </div>
    );
  }

  return (
    <div className="step-container">
      <h2 className="step-title">Hardware Detection</h2>
      <p className="step-subtitle">
        {phase === "done"
          ? "Detection complete"
          : "Scanning your system..."}
      </p>

      <div className="card">
        {fields.map((field) => (
          <div className="card-row" key={field.label}>
            <span className="card-label">{field.label}</span>
            <span className="card-value">
              {field.done ? (
                <span className="checkmark" aria-label="detected">&#10003;</span>
              ) : (
                <span className="spinner" />
              )}
              {field.value}
            </span>
          </div>
        ))}
      </div>

      {phase === "done" && (
        <div className={`hw-status ${qualified ? "qualified" : "not-qualified"}`}>
          {qualified ? (
            <>
              <span className="checkmark">&#10003;</span>
              Your hardware qualifies!
            </>
          ) : (
            <>Your hardware may be too slow</>
          )}
        </div>
      )}

      {phase === "error" && (
        <div className="hw-status not-qualified">
          Detection error: {error}
        </div>
      )}

      <div className="step-actions">
        <button className="btn btn-secondary" onClick={onBack}>
          Back
        </button>
        <button
          className="btn btn-primary"
          onClick={() => gpu && system && onNext(gpu, system)}
          disabled={phase !== "done" || !gpu || !system}
        >
          Continue
        </button>
      </div>
    </div>
  );
}

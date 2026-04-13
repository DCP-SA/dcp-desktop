import { useState } from "react";
import type { DaemonConfig } from "../lib/api";

interface ConfigurationProps {
  onNext: (config: DaemonConfig) => void;
  onBack: () => void;
}

type RunMode = "always" | "idle" | "scheduled";

const RUN_MODE_LABELS: Record<RunMode, string> = {
  always: "Always On",
  idle: "When Idle",
  scheduled: "Scheduled",
};

export function Configuration({ onNext, onBack }: ConfigurationProps) {
  const [runMode, setRunMode] = useState<RunMode>("idle");
  const [gpuCap, setGpuCap] = useState(80);
  const [tempLimit, setTempLimit] = useState(85);
  const [startOnBoot, setStartOnBoot] = useState(true);

  function handleInstall() {
    onNext({
      run_mode: runMode,
      gpu_usage_cap: gpuCap,
      temp_limit: tempLimit,
      start_on_boot: startOnBoot,
    });
  }

  return (
    <div className="step-container">
      <h2 className="step-title">Configuration</h2>
      <p className="step-subtitle">Customize how DCP uses your hardware</p>

      {/* Run Mode */}
      <div className="config-section">
        <div className="config-label">Run Mode</div>
        <div className="radio-group">
          {(Object.keys(RUN_MODE_LABELS) as RunMode[]).map((mode) => (
            <button
              key={mode}
              className={`radio-option ${runMode === mode ? "selected" : ""}`}
              onClick={() => setRunMode(mode)}
              aria-pressed={runMode === mode}
            >
              {RUN_MODE_LABELS[mode]}
            </button>
          ))}
        </div>
      </div>

      {/* GPU Usage Cap */}
      <div className="config-section">
        <div className="config-label">GPU Usage Cap</div>
        <div className="config-sublabel">Maximum GPU utilization for DCP jobs</div>
        <div className="slider-container">
          <input
            type="range"
            className="slider"
            min={50}
            max={100}
            step={5}
            value={gpuCap}
            onChange={(e) => setGpuCap(Number(e.target.value))}
            aria-label="GPU usage cap percentage"
          />
          <span className="slider-value">{gpuCap}%</span>
        </div>
      </div>

      {/* Temperature Limit */}
      <div className="config-section">
        <div className="config-label">Temperature Limit</div>
        <div className="config-sublabel">Pause jobs if GPU exceeds this temperature</div>
        <div className="slider-container">
          <input
            type="range"
            className="slider"
            min={70}
            max={90}
            step={1}
            value={tempLimit}
            onChange={(e) => setTempLimit(Number(e.target.value))}
            aria-label="GPU temperature limit in celsius"
          />
          <span className="slider-value">{tempLimit}&deg;C</span>
        </div>
      </div>

      {/* Start on Boot */}
      <div className="config-section">
        <div
          className="checkbox-row"
          onClick={() => setStartOnBoot(!startOnBoot)}
          role="checkbox"
          aria-checked={startOnBoot}
          tabIndex={0}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              setStartOnBoot(!startOnBoot);
            }
          }}
        >
          <div className={`checkbox ${startOnBoot ? "checked" : ""}`} />
          <span className="checkbox-label">Start DCP when computer boots</span>
        </div>
      </div>

      <div className="step-actions">
        <button className="btn btn-secondary" onClick={onBack}>
          Back
        </button>
        <button className="btn btn-primary" onClick={handleInstall}>
          Install
        </button>
      </div>
    </div>
  );
}

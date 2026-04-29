import { useState, useEffect } from "react";
import { getVersion } from "@tauri-apps/api/app";
import type { DaemonConfig } from "../lib/api";
import { rollbackDaemon } from "../lib/api";

interface SettingsProps {
  onClose: () => void;
  apiKey: string;
  config: DaemonConfig;
  onSave: (config: DaemonConfig) => void;
}

export function Settings({ onClose, apiKey, config, onSave }: SettingsProps) {
  const [runMode, setRunMode] = useState<DaemonConfig["run_mode"]>(config.run_mode);
  const [gpuCap, setGpuCap] = useState(config.gpu_usage_cap);
  const [tempLimit, setTempLimit] = useState(config.temp_limit);
  const [startOnBoot, setStartOnBoot] = useState(config.start_on_boot);
  const [copied, setCopied] = useState(false);
  const [appVersion, setAppVersion] = useState("…");

  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => setAppVersion("unknown"));
  }, []);

  function handleSave() {
    onSave({
      run_mode: runMode,
      gpu_usage_cap: gpuCap,
      temp_limit: tempLimit,
      start_on_boot: startOnBoot,
    });
    onClose();
  }

  function copyKey() {
    navigator.clipboard.writeText(apiKey).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }).catch(() => {
      // clipboard not available
    });
  }

  const [rollbackStatus, setRollbackStatus] = useState<string | null>(null);
  const [rollbackLoading, setRollbackLoading] = useState(false);

  async function handleRollback() {
    if (!window.confirm(
      "This will restore the previous daemon version and restart. This cannot be undone. Continue?"
    )) {
      return;
    }
    setRollbackLoading(true);
    setRollbackStatus(null);
    try {
      const result = await rollbackDaemon();
      const version = result.startsWith("rolled_back:")
        ? result.replace("rolled_back:", "")
        : result;
      setRollbackStatus(`Restored version ${version}`);
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      setRollbackStatus(`Error: ${msg}`);
    } finally {
      setRollbackLoading(false);
    }
  }

  function maskKey(key: string): string {
    if (key.length <= 8) return key;
    return "****" + key.slice(-8);
  }

  return (
    <div className="settings-overlay" onClick={onClose}>
      <div className="settings-panel" onClick={(e) => e.stopPropagation()}>
        <div className="settings-header">
          <h2 className="settings-title">Settings</h2>
          <button
            className="settings-close-btn"
            onClick={onClose}
            aria-label="Close settings"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        <div className="settings-body">
          {/* Run Mode */}
          <div className="settings-section">
            <label className="settings-label">Run Mode</label>
            <div className="radio-group">
              {(["always", "idle", "scheduled"] as const).map((mode) => (
                <button
                  key={mode}
                  className={`radio-option ${runMode === mode ? "selected" : ""}`}
                  onClick={() => setRunMode(mode)}
                >
                  {mode === "always" ? "Always On" : mode === "idle" ? "When Idle" : "Scheduled"}
                </button>
              ))}
            </div>
          </div>

          {/* GPU Cap */}
          <div className="settings-section">
            <label className="settings-label">GPU Cap</label>
            <div className="slider-container">
              <input
                type="range"
                className="slider"
                min={50}
                max={100}
                step={5}
                value={gpuCap}
                onChange={(e) => setGpuCap(Number(e.target.value))}
              />
              <span className="slider-value">{gpuCap}%</span>
            </div>
          </div>

          {/* Temperature Limit */}
          <div className="settings-section">
            <label className="settings-label">Temperature Limit</label>
            <div className="slider-container">
              <input
                type="range"
                className="slider"
                min={70}
                max={90}
                step={1}
                value={tempLimit}
                onChange={(e) => setTempLimit(Number(e.target.value))}
              />
              <span className="slider-value">{tempLimit}&deg;C</span>
            </div>
          </div>

          {/* Start on Boot */}
          <div className="settings-section">
            <div
              className="checkbox-row"
              onClick={() => setStartOnBoot(!startOnBoot)}
            >
              <div className={`checkbox ${startOnBoot ? "checked" : ""}`} />
              <span className="checkbox-label">Start on boot</span>
            </div>
          </div>

          {/* API Key */}
          <div className="settings-section">
            <label className="settings-label">API Key</label>
            <div className="settings-key-row">
              <code className="settings-key-display">{maskKey(apiKey)}</code>
              <button className="btn btn-secondary settings-copy-btn" onClick={copyKey}>
                {copied ? "Copied" : "Copy"}
              </button>
            </div>
          </div>

          {/* About */}
          <div className="settings-section settings-about">
            <span className="settings-about-text">DCP Provider v{appVersion}</span>
          </div>

          {/* Rollback Daemon */}
          <div className="settings-section">
            <label className="settings-label">Daemon Recovery</label>
            <button
              className="btn btn-secondary"
              onClick={handleRollback}
              disabled={rollbackLoading}
              style={{ width: "100%" }}
            >
              {rollbackLoading ? "Rolling back…" : "Rollback to Previous Version"}
            </button>
            {rollbackStatus && (
              <span
                className="settings-about-text"
                style={{
                  display: "block",
                  marginTop: "8px",
                  color: rollbackStatus.startsWith("Error") ? "#ef4444" : "#22c55e",
                }}
              >
                {rollbackStatus}
              </span>
            )}
          </div>
        </div>

        <div className="settings-footer">
          <button className="btn btn-secondary" onClick={onClose}>
            Close
          </button>
          <button className="btn btn-primary" onClick={handleSave}>
            Save
          </button>
        </div>
      </div>
    </div>
  );
}

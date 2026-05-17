import { useState, useEffect } from "react";
import { Welcome } from "./components/Welcome";
import { HardwareDetection } from "./components/HardwareDetection";
import { Account } from "./components/Account";
import { Configuration } from "./components/Configuration";
import { Installing } from "./components/Installing";
import { Dashboard } from "./components/Dashboard";
import { AgentChat } from "./components/AgentChat";
import { checkSetupComplete } from "./lib/api";
import type { GpuInfo, SystemInfo, DaemonConfig } from "./lib/api";
import "./components/agent-chat.css";

type AppView = "loading" | "wizard" | "dashboard" | "agent";
type WizardStep = "welcome" | "hardware" | "account" | "config" | "installing";

const STEP_ORDER: WizardStep[] = [
  "welcome",
  "hardware",
  "account",
  "config",
  "installing",
];

function App() {
  const [view, setView] = useState<AppView>("agent");
  const [step, setStep] = useState<WizardStep>("welcome");
  const [gpu, setGpu] = useState<GpuInfo | null>(null);
  const [_system, setSystem] = useState<SystemInfo | null>(null);
  const [apiKey, setApiKey] = useState("");
  const [config, setConfig] = useState<DaemonConfig>({
    run_mode: "idle",
    gpu_usage_cap: 80,
    temp_limit: 85,
    start_on_boot: true,
  });

  // Check on launch whether setup is already complete
  useEffect(() => {
    async function init() {
      const setupDone = await checkSetupComplete();
      setView(setupDone ? "agent" : "wizard");

      // Check for app updates (silent, non-blocking)
      try {
        const { check } = await import("@tauri-apps/plugin-updater");
        const update = await check();
        if (update?.available) {
          console.log(`Update available: ${update.version}`);
          await update.downloadAndInstall();
          // Relaunch after install
          const { relaunch } = await import("@tauri-apps/plugin-process");
          await relaunch();
        }
      } catch (e) {
        console.log("Update check skipped:", e);
      }
    }
    init();
  }, []);

  // Listen for tray "navigate" events
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    (async () => {
      try {
        const { listen } = await import("@tauri-apps/api/event");
        unlisten = await listen<string>("navigate", (event) => {
          if (event.payload === "agent") setView("agent");
          else if (event.payload === "dashboard") setView("dashboard");
        });
      } catch {
        // Not in Tauri context (dev browser)
      }
    })();
    return () => { unlisten?.(); };
  }, []);

  const stepIndex = STEP_ORDER.indexOf(step);

  function handleHardwareNext(detectedGpu: GpuInfo, detectedSystem: SystemInfo) {
    setGpu(detectedGpu);
    setSystem(detectedSystem);
    setStep("account");
  }

  function handleAccountNext(key: string) {
    setApiKey(key);
    setStep("config");
  }

  function handleConfigNext(cfg: DaemonConfig) {
    setConfig(cfg);
    setStep("installing");
  }

  function handleSetupComplete() {
    setView("dashboard");
  }

  // Loading state
  if (view === "loading") {
    return (
      <div className="app-loading">
        <div className="detecting-spinner" />
      </div>
    );
  }

  // Agent panel — the main view after setup
  if (view === "agent") {
    return <AgentChat />;
  }

  // Dashboard view — with agent chat button
  if (view === "dashboard") {
    return (
      <div style={{ height: "100%", display: "flex", flexDirection: "column" }}>
        <Dashboard />
        <button
          className="agent-fab"
          onClick={() => setView("agent")}
          title="Chat with Agent"
        >
          <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
          </svg>
        </button>
      </div>
    );
  }

  // Wizard view
  return (
    <div className="wizard-container">
      {/* Progress dots -- hidden on welcome and installing */}
      {step !== "welcome" && step !== "installing" && (
        <nav className="wizard-progress" aria-label="Setup progress">
          {STEP_ORDER.slice(1, 4).map((s, i) => {
            const sIdx = i + 1; // offset since we skip welcome
            return (
              <div key={s} className="wizard-progress-step">
                {i > 0 && (
                  <div
                    className={`wizard-progress-line ${stepIndex > sIdx ? "completed" : ""}`}
                  />
                )}
                <div
                  className={`wizard-progress-dot ${
                    stepIndex === sIdx
                      ? "active"
                      : stepIndex > sIdx
                        ? "completed"
                        : ""
                  }`}
                  aria-label={`Step ${i + 1}: ${s}`}
                />
              </div>
            );
          })}
        </nav>
      )}

      {/* Wizard steps */}
      <div className="wizard-step-wrapper" key={step}>
        {step === "welcome" && <Welcome onNext={() => setStep("hardware")} />}

        {step === "hardware" && (
          <HardwareDetection
            onNext={handleHardwareNext}
            onBack={() => setStep("welcome")}
          />
        )}

        {step === "account" && (
          <Account
            onNext={handleAccountNext}
            onBack={() => setStep("hardware")}
          />
        )}

        {step === "config" && (
          <Configuration
            onNext={handleConfigNext}
            onBack={() => setStep("account")}
          />
        )}

        {step === "installing" && (
          <Installing
            apiKey={apiKey}
            config={config}
            gpu={gpu}
            onComplete={handleSetupComplete}
          />
        )}
      </div>
    </div>
  );
}

export default App;

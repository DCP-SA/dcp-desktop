import { useState, useEffect } from "react";
import { Welcome } from "./components/Welcome";
import { HardwareDetection } from "./components/HardwareDetection";
import { Account } from "./components/Account";
import { Configuration } from "./components/Configuration";
import { Installing } from "./components/Installing";
import { Dashboard } from "./components/Dashboard";
import { checkSetupComplete } from "./lib/api";
import type { GpuInfo, SystemInfo, DaemonConfig } from "./lib/api";

type AppView = "loading" | "wizard" | "dashboard";
type WizardStep = "welcome" | "hardware" | "account" | "config" | "installing";

const STEP_ORDER: WizardStep[] = [
  "welcome",
  "hardware",
  "account",
  "config",
  "installing",
];

function App() {
  const [view, setView] = useState<AppView>("loading");
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
      setView(setupDone ? "dashboard" : "wizard");
    }
    init();
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

  // Dashboard view
  if (view === "dashboard") {
    return <Dashboard />;
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

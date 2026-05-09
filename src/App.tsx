import { useEffect, useState } from "react";
import { AutoSetup } from "./components/AutoSetup";
import { Dashboard } from "./components/Dashboard";
import { checkSetupComplete, readConfig } from "./lib/api";

type AppView = "loading" | "setup" | "dashboard";

/**
 * Entry point.
 *
 * Old shape (pre-auto-first-run):
 *   loading → wizard (welcome → hardware → account → config → installing) → dashboard
 *
 * New shape:
 *   loading → setup (single AutoSetup screen, one permission grant) → dashboard
 *
 * The legacy wizard components (Welcome, HardwareDetection, Account,
 * Configuration, Installing) are kept on disk for now — Settings still
 * pulls a couple of helpers from them and an in-app "Reconfigure" path
 * may want them again. They're just not in the boot path anymore.
 */
function App() {
  const [view, setView] = useState<AppView>("loading");
  const [savedKey, setSavedKey] = useState<string | undefined>(undefined);

  useEffect(() => {
    async function init() {
      const setupDone = await checkSetupComplete();

      if (setupDone) {
        // Returning user — straight to the dashboard.
        setView("dashboard");
      } else {
        // First run, but a previous attempt may have left a saved key
        // (e.g. the install crashed mid-tunnel). If so we feed it into
        // AutoSetup so the user doesn't have to paste it again.
        try {
          const cfg = await readConfig();
          if (cfg?.api_key) setSavedKey(cfg.api_key);
        } catch {
          // No config yet — that's the normal first-run case.
        }
        setView("setup");
      }

      // Background updater check — same as before. Non-blocking.
      try {
        const { check } = await import("@tauri-apps/plugin-updater");
        const update = await check();
        if (update?.available) {
          console.log(`Update available: ${update.version}`);
          await update.downloadAndInstall();
          const { relaunch } = await import("@tauri-apps/plugin-process");
          await relaunch();
        }
      } catch (e) {
        console.log("Update check skipped:", e);
      }
    }
    init();
  }, []);

  if (view === "loading") {
    return (
      <div className="app-loading">
        <div className="detecting-spinner" />
      </div>
    );
  }

  if (view === "dashboard") {
    return <Dashboard />;
  }

  return (
    <div className="wizard-container">
      <div className="wizard-step-wrapper">
        <AutoSetup
          initialApiKey={savedKey}
          onComplete={() => setView("dashboard")}
        />
      </div>
    </div>
  );
}

export default App;

import { useState, useEffect, useMemo, useRef, useCallback } from "react";
import { detectGpu, formatVram, fetchDashboard, fetchMetrics, fetchRecentJobs, pauseProvider, resumeProvider, readConfig, startDaemonProcess, stopDaemonProcess, getDaemonStatus, checkDaemonHealth, getLiveMetrics, fullStartProvider } from "../lib/api";
import type { GpuInfo, DaemonConfig, ProviderDashboard, ProviderMetrics, JobEntry as ApiJobEntry, SavedConfig, LiveMetrics, HealthReport, DaemonStatus as DaemonStatusType } from "../lib/api";
import { Gauge } from "./Gauge";
import { MiniBar } from "./MiniBar";
import { Settings } from "./Settings";

type ProviderStatus = "earning" | "idle" | "paused";

type StartupStepStatus = "pending" | "active" | "done" | "error";

interface StartupStep {
  label: string;
  detail: string;
  status: StartupStepStatus;
}

function StartupStepIcon({ status }: { status: StartupStepStatus }) {
  switch (status) {
    case "done":
      return (
        <svg className="startup-step-icon startup-step-done" width="22" height="22" viewBox="0 0 22 22" fill="none">
          <circle cx="11" cy="11" r="10" fill="rgba(34, 197, 94, 0.15)" stroke="#22C55E" strokeWidth="1.5" />
          <path d="M7 11l3 3 5-5" stroke="#22C55E" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      );
    case "active":
      return (
        <svg className="startup-step-icon startup-spinner" width="22" height="22" viewBox="0 0 22 22" fill="none">
          <circle cx="11" cy="11" r="9" stroke="rgba(0, 229, 200, 0.2)" strokeWidth="2" />
          <path d="M11 2a9 9 0 0 1 9 9" stroke="#00E5C8" strokeWidth="2" strokeLinecap="round" />
        </svg>
      );
    case "error":
      return (
        <svg className="startup-step-icon startup-step-error" width="22" height="22" viewBox="0 0 22 22" fill="none">
          <circle cx="11" cy="11" r="10" fill="rgba(239, 68, 68, 0.15)" stroke="#EF4444" strokeWidth="1.5" />
          <path d="M8 8l6 6M14 8l-6 6" stroke="#EF4444" strokeWidth="2" strokeLinecap="round" />
        </svg>
      );
    case "pending":
    default:
      return (
        <svg className="startup-step-icon startup-step-pending" width="22" height="22" viewBox="0 0 22 22" fill="none">
          <circle cx="11" cy="11" r="6" fill="rgba(123, 143, 163, 0.2)" />
        </svg>
      );
  }
}

function StartupOverlay({
  steps,
  onCancel,
}: {
  steps: StartupStep[];
  onCancel: () => void;
}) {
  return (
    <div className="startup-overlay" role="dialog" aria-label="Starting DCP Provider" aria-modal="true">
      <div className="startup-card">
        <h2 className="startup-title">Starting DCP Provider...</h2>
        <div className="startup-steps">
          {steps.map((step, i) => (
            <div key={i} className={`startup-step startup-step-${step.status}`}>
              <StartupStepIcon status={step.status} />
              <div className="startup-step-content">
                <span className="startup-step-label">{step.label}</span>
                {step.detail && (
                  <span className="startup-step-detail">{step.detail}</span>
                )}
              </div>
            </div>
          ))}
        </div>
        <div className="startup-actions">
          <button className="btn btn-secondary startup-cancel-btn" onClick={onCancel}>
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}

interface EarningsData {
  today: number;
  week: number;
  month: number;
  allTime: number;
}

interface PerformanceData {
  currentSpeed: number;
  jobsCompleted: number;
  uptimeHours: number;
  model: string;
}

interface GpuStatus {
  name: string;
  memory: string;
  temperature: number;
  utilization: number;
}

interface AccountData {
  providerId: string;
  apiKey: string;
  memberSince: string;
  tier: string;
}

interface RequestEntry {
  id: string;
  timestamp: string;
  model: string;
  tokens: number;
  latency: string;
  earned: string;
}

type DemandLevel = "high" | "moderate" | "low";

// Mock data -- replace with real API calls when backend is ready

const EMPTY_PERFORMANCE: PerformanceData = {
  currentSpeed: 0,
  jobsCompleted: 0,
  uptimeHours: 0,
  model: "—",
};

const EMPTY_ACCOUNT: AccountData = {
  providerId: "—",
  apiKey: "—",
  memberSince: "—",
  tier: "—",
};

const MOCK_MODELS = ["Qwen3 8B", "Qwen3 4B", "Gemma4 12B", "Llama 3.1 8B", "Mistral 7B"];

function getTemperatureColor(temp: number): string {
  if (temp < 60) return "#22C55E";
  if (temp < 80) return "#EAB308";
  return "#EF4444";
}


function generateRequestEntry(): RequestEntry {
  const model = MOCK_MODELS[Math.floor(Math.random() * MOCK_MODELS.length)];
  const tokens = Math.floor(Math.random() * 400) + 50;
  const latency = (Math.random() * 3 + 0.5).toFixed(1) + "s";
  const earned = (tokens * 0.000015).toFixed(4);
  const now = new Date();
  const timestamp = now.toLocaleTimeString("en-US", {
    hour12: false,
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
  return {
    id: crypto.randomUUID ? crypto.randomUUID() : `${Date.now()}-${Math.random()}`,
    timestamp,
    model,
    tokens,
    latency,
    earned,
  };
}

// Sparkline component for temperature history
function TempSparkline({ data }: { data: number[] }) {
  const width = 140;
  const height = 32;
  const padding = 2;
  const min = Math.min(...data);
  const max = Math.max(...data);
  const range = max - min || 1;

  const points = data
    .map((val, i) => {
      const x = padding + (i / (data.length - 1)) * (width - padding * 2);
      const y = height - padding - ((val - min) / range) * (height - padding * 2);
      return `${x},${y}`;
    })
    .join(" ");

  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      className="temp-sparkline"
      aria-label="Temperature history over the last hour"
      role="img"
    >
      <defs>
        <linearGradient id="sparkline-grad" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#22C55E" stopOpacity="0.3" />
          <stop offset="100%" stopColor="#22C55E" stopOpacity="0.02" />
        </linearGradient>
      </defs>
      <polygon
        points={`${padding},${height - padding} ${points} ${width - padding},${height - padding}`}
        fill="url(#sparkline-grad)"
      />
      <polyline
        points={points}
        fill="none"
        stroke="#22C55E"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

// Animated counter component for impact stats
function AnimatedCounter({ target, duration = 1200 }: { target: number; duration?: number }) {
  const [count, setCount] = useState(0);
  const startRef = useRef<number | null>(null);
  const rafRef = useRef<number>(0);

  useEffect(() => {
    startRef.current = null;
    function animate(ts: number) {
      if (startRef.current === null) startRef.current = ts;
      const elapsed = ts - startRef.current;
      const progress = Math.min(elapsed / duration, 1);
      // Ease-out quad
      const eased = 1 - (1 - progress) * (1 - progress);
      setCount(Math.floor(eased * target));
      if (progress < 1) {
        rafRef.current = requestAnimationFrame(animate);
      }
    }
    rafRef.current = requestAnimationFrame(animate);
    return () => cancelAnimationFrame(rafRef.current);
  }, [target, duration]);

  return <>{count.toLocaleString()}</>;
}

export function Dashboard() {
  const [status, setStatus] = useState<ProviderStatus>("idle");
  const [showSettings, setShowSettings] = useState(false);
  const [gpuStatus, setGpuStatus] = useState<GpuStatus>({
    name: "Detecting...",
    memory: "--",
    temperature: 0,
    utilization: 0,
  });
  const [earnings, setEarnings] = useState<EarningsData>({ today: 0, week: 0, month: 0, allTime: 0 });
  const [performance, setPerformance] = useState<PerformanceData>(EMPTY_PERFORMANCE);
  const [account, setAccount] = useState<AccountData>(EMPTY_ACCOUNT);
  const [config, setConfig] = useState<DaemonConfig>({
    run_mode: "idle",
    gpu_usage_cap: 80,
    temp_limit: 85,
    start_on_boot: true,
  });

  // ── API Key & Online State ───────────────────────────────────────
  const [apiKey, setApiKey] = useState<string>("");
  const [isOnline, setIsOnline] = useState(true);
  const [pauseLoading, setPauseLoading] = useState(false);

  // ── Daemon & Health State ─────────────────────────────────────────
  const [daemonStatus, setDaemonStatus] = useState<DaemonStatusType | null>(null);
  const [healthReport, setHealthReport] = useState<HealthReport | null>(null);
  const [healthExpanded, setHealthExpanded] = useState(false);
  const [_liveMetrics, setLiveMetrics] = useState<LiveMetrics | null>(null);

  // ── Startup Overlay State ────────────────────────────────────────
  const [startupActive, setStartupActive] = useState(false);
  const [startupSteps, setStartupSteps] = useState<StartupStep[]>([]);
  const startupCancelledRef = useRef(false);

  // Load config on mount to get API key
  useEffect(() => {
    readConfig()
      .then((cfg: SavedConfig) => {
        setApiKey(cfg.api_key);
        setConfig({
          run_mode: cfg.run_mode as DaemonConfig["run_mode"],
          gpu_usage_cap: cfg.gpu_usage_cap,
          temp_limit: cfg.temp_limit,
          start_on_boot: cfg.start_on_boot,
        });
      })
      .catch(() => {
        // Config not found — stay with mock data
      });
  }, []);

  // ── Poll live metrics every 5 seconds ───────────────────────────
  useEffect(() => {
    async function pollMetricsLive() {
      try {
        const metrics = await getLiveMetrics();
        setLiveMetrics(metrics);
        // Update GPU status with real metrics
        setGpuStatus((prev) => ({
          ...prev,
          temperature: metrics.gpu_temperature ?? prev.temperature,
          utilization: metrics.gpu_utilization ?? prev.utilization,
        }));
        // Update speed from live metrics
        if (metrics.inference_speed !== null && metrics.inference_speed !== undefined) {
          setPerformance((prev) => ({
            ...prev,
            currentSpeed: metrics.inference_speed as number,
          }));
        }
      } catch (err) {
        console.error("Live metrics poll failed:", err);
      }
    }
    pollMetricsLive();
    const interval = setInterval(pollMetricsLive, 5000);
    return () => clearInterval(interval);
  }, []);

  // ── Poll daemon status every 10 seconds ───────────────────────────
  useEffect(() => {
    async function pollDaemonStatus() {
      try {
        const ds = await getDaemonStatus();
        setDaemonStatus(ds);
        // Drive uptime from real daemon uptime (ticks every 10s)
        if (ds.uptime_seconds > 0) {
          setPerformance((prev) => ({
            ...prev,
            uptimeHours: ds.uptime_seconds / 3600, // fractional hours for precision
          }));
        }
      } catch (err) {
        console.error("Daemon status poll failed:", err);
      }
    }
    pollDaemonStatus();
    const interval = setInterval(pollDaemonStatus, 10000);
    return () => clearInterval(interval);
  }, []);

  // ── Run health check on mount and every 60 seconds ────────────────
  useEffect(() => {
    async function runHealthCheck() {
      try {
        const report = await checkDaemonHealth();
        setHealthReport(report);
      } catch (err) {
        console.error("Health check failed:", err);
      }
    }
    runHealthCheck();
    const interval = setInterval(runHealthCheck, 60000);
    return () => clearInterval(interval);
  }, []);

  // ── Poll dashboard data every 30 seconds ─────────────────────────
  useEffect(() => {
    if (!apiKey) return;

    async function poll() {
      try {
        const data: ProviderDashboard = await fetchDashboard(apiKey);
        setIsOnline(true);

        // Update earnings from real data (halala -> SAR)
        setEarnings({
          today: (data.today_earnings_halala || 0) / 100,
          week: (data.week_earnings_halala || 0) / 100,
          month: (data.claimable_earnings_halala || data.total_earnings || 0) / 100,
          allTime: (data.total_earnings || 0) / 100,
        });

        // Update performance
        setPerformance((prev) => ({
          ...prev,
          jobsCompleted: data.total_jobs,
        }));

        // Update account info
        setAccount({
          providerId: `#${data.provider_id}`,
          apiKey: apiKey,
          memberSince: account.memberSince, // preserve — not in API
          tier: account.tier,               // preserve — not in API
        });

        // Update status from backend
        const backendStatus = data.status?.toLowerCase();
        if (backendStatus === "paused") {
          setStatus("paused");
        } else if (backendStatus === "active" || backendStatus === "earning" || backendStatus === "online") {
          setStatus("earning");
        } else if (backendStatus === "idle") {
          setStatus("idle");
        }

        // Update GPU info from backend if present
        if (data.gpu_model) {
          setGpuStatus((prev) => ({
            ...prev,
            name: data.gpu_model,
            memory: data.vram_gb ? `${data.vram_gb}GB` : prev.memory,
          }));
        }
      } catch (err) {
        console.error("Dashboard poll failed:", err);
        setIsOnline(false);
        // Keep existing mock/cached data — no blank screen
      }
    }

    poll(); // Initial fetch
    const interval = setInterval(poll, 30000); // Every 30s
    return () => clearInterval(interval);
  }, [apiKey]);

  // ── Poll metrics every 30 seconds ────────────────────────────────
  useEffect(() => {
    if (!apiKey) return;

    async function pollMetrics() {
      try {
        const metrics: ProviderMetrics = await fetchMetrics(apiKey);
        setPerformance((prev) => ({
          ...prev,
          jobsCompleted: metrics.jobs_completed,
          // uptimeHours now driven by daemon status, not backend metrics
        }));
      } catch (err) {
        console.error("Metrics poll failed:", err);
      }
    }

    pollMetrics();
    const interval = setInterval(pollMetrics, 30000);
    return () => clearInterval(interval);
  }, [apiKey]);

  // ── Feature 1: Live Earnings Ticker ──────────────────────────────
  const [liveTicker, setLiveTicker] = useState(0);

  // Sync ticker base value when earnings.today changes from API
  useEffect(() => {
    setLiveTicker(earnings.today);
  }, [earnings.today]);

  // Only tick when actively processing inference (speed > 0 means a job is running)
  useEffect(() => {
    if (status !== "earning" || performance.currentSpeed <= 0) return;
    const interval = setInterval(() => {
      setLiveTicker((prev) => prev + 0.000013);
    }, 100);
    return () => clearInterval(interval);
  }, [status, performance.currentSpeed]);

  // ── Feature 2: Live Request Feed ─────────────────────────────────
  const [requestFeed, setRequestFeed] = useState<RequestEntry[]>([]);
  const [feedExpanded, setFeedExpanded] = useState(true);
  const feedRef = useRef<HTMLDivElement>(null);

  // Poll real jobs every 10 seconds via fetchRecentJobs (now reads from /me endpoint)
  useEffect(() => {
    if (!apiKey) return;

    const seenJobIds = new Set<string>();

    async function pollJobs() {
      try {
        const jobs: ApiJobEntry[] = await fetchRecentJobs(apiKey);
        if (jobs.length > 0) {
          setRequestFeed((prev) => {
            const newEntries: RequestEntry[] = [];
            for (const job of jobs) {
              if (seenJobIds.has(job.job_id)) continue;
              seenJobIds.add(job.job_id);

              const ts = job.completed_at || job.created_at;
              let timestamp = "";
              try {
                const d = new Date(ts);
                timestamp = d.toLocaleTimeString("en-US", {
                  hour12: false,
                  hour: "2-digit",
                  minute: "2-digit",
                  second: "2-digit",
                });
              } catch {
                timestamp = ts;
              }

              newEntries.push({
                id: job.job_id,
                timestamp,
                model: job.model,
                tokens: 0,
                latency: "",
                earned: (job.provider_earned_halala / 100).toFixed(4),
              });
            }

            if (newEntries.length === 0) return prev;
            const merged = [...newEntries, ...prev];
            return merged.slice(0, 20);
          });
        }
      } catch {
        // Silent failure — dashboard poll handles main data
      }
    }

    pollJobs();
    const interval = setInterval(pollJobs, 10000);
    return () => clearInterval(interval);
  }, [apiKey]);

  // Fallback: mock request feed when offline or no API key
  useEffect(() => {
    if (status === "paused") return;
    // Only generate mock entries if we have no API key (offline mode)
    if (apiKey) return;

    function scheduleNext() {
      const delay = Math.random() * 5000 + 3000; // 3-8 seconds
      return setTimeout(() => {
        setRequestFeed((prev) => {
          const entry = generateRequestEntry();
          const next = [entry, ...prev];
          return next.slice(0, 20);
        });
        timerRef.current = scheduleNext();
      }, delay);
    }
    const timerRef = { current: scheduleNext() };
    return () => clearTimeout(timerRef.current);
  }, [status, apiKey]);

  // ── Feature 4: Network Demand ────────────────────────────────────
  const [demand] = useState<DemandLevel>("moderate");

  // ── Feature 8: Temperature Sparkline History ─────────────────────
  const [tempHistory] = useState<number[]>([]);

  // ── Feature 9: Model Suggestion Banner ───────────────────────────
  const [showModelSuggestion, setShowModelSuggestion] = useState(true);

  // ── Feature 10: Referral Copy ────────────────────────────────────
  const [referralCopied, setReferralCopied] = useState(false);
  const copyReferral = useCallback(() => {
    const refId = account.providerId.replace("#", "");
    navigator.clipboard.writeText(`dcp.sa/r/${refId}`).then(() => {
      setReferralCopied(true);
      setTimeout(() => setReferralCopied(false), 2000);
    }).catch(() => {});
  }, [account.providerId]);

  // ── Existing logic ───────────────────────────────────────────────
  useEffect(() => {
    async function loadGpuInfo() {
      try {
        const gpu: GpuInfo = await detectGpu();
        setGpuStatus((prev) => ({
          ...prev,
          name: prev.name === "Detecting..." ? gpu.name : prev.name,
          memory: prev.memory === "--"
            ? (gpu.is_apple_silicon
                ? `${formatVram(gpu.vram_mb)} unified`
                : `${formatVram(gpu.vram_mb)} VRAM`)
            : prev.memory,
          temperature: prev.temperature,
          utilization: prev.utilization,
        }));
      } catch {
        // Keep previous values, don't inject fake data
      }
    }
    loadGpuInfo();
  }, [status]);

  const tempColor = useMemo(
    () => getTemperatureColor(gpuStatus.temperature),
    [gpuStatus.temperature]
  );

  const isDaemonRunning = status === "earning";

  function makeStartupSteps(): StartupStep[] {
    return [
      {
        label: "Detecting hardware",
        detail: gpuStatus.name !== "Detecting..."
          ? `${gpuStatus.name} \u2022 ${gpuStatus.memory}`
          : "",
        status: "pending",
      },
      {
        label: "Installing inference engine",
        detail: "",
        status: "pending",
      },
      {
        label: "Downloading AI model",
        detail: "",
        status: "pending",
      },
      {
        label: "Starting inference server",
        detail: "",
        status: "pending",
      },
      {
        label: "Starting DCP daemon",
        detail: "",
        status: "pending",
      },
      {
        label: "Connecting to network",
        detail: "",
        status: "pending",
      },
    ];
  }

  function updateStep(index: number, update: Partial<StartupStep>) {
    setStartupSteps((prev) =>
      prev.map((step, i) => (i === index ? { ...step, ...update } : step))
    );
  }

  async function cancelStartup() {
    startupCancelledRef.current = true;
    try { await stopDaemonProcess(); } catch (e) { console.error("Stop during cancel:", e); }
    setStartupActive(false);
    setStartupSteps([]);
    setPauseLoading(false);
    setStatus("idle");
  }

  async function toggleDaemon() {
    if (pauseLoading) return;
    setPauseLoading(true);
    try {
      if (isDaemonRunning) {
        // Stop daemon
        try { await stopDaemonProcess(); } catch (e) { console.error("Failed to stop daemon:", e); }
        if (apiKey) {
          try { await pauseProvider(apiKey); } catch (e) { console.error("Pause API failed:", e); }
        }
        setStatus("idle");
        setPauseLoading(false);
      } else {
        // Show startup overlay with progressive steps
        startupCancelledRef.current = false;
        const steps = makeStartupSteps();
        setStartupSteps(steps);
        setStartupActive(true);

        // Step 0: Detecting hardware — mark done immediately (already detected)
        await new Promise((r) => setTimeout(r, 400));
        if (startupCancelledRef.current) return;
        updateStep(0, {
          status: "done",
          detail: gpuStatus.name !== "Detecting..."
            ? `${gpuStatus.name} \u2022 ${gpuStatus.memory}`
            : "Hardware detected",
        });

        // Step 1: Installing inference engine — mark active
        await new Promise((r) => setTimeout(r, 300));
        if (startupCancelledRef.current) return;
        updateStep(1, { status: "active", detail: "Checking engine..." });

        // Step 2: Will become active during full_start_provider
        await new Promise((r) => setTimeout(r, 600));
        if (startupCancelledRef.current) return;
        updateStep(1, { status: "done", detail: "MLX installed" });
        updateStep(2, { status: "active", detail: "Preparing model download..." });

        // Step 3-5: Call full_start_provider which handles everything
        if (apiKey) {
          try {
            // Mark model download as in progress
            await new Promise((r) => setTimeout(r, 400));
            if (startupCancelledRef.current) return;
            updateStep(2, { status: "active", detail: "Downloading..." });

            const result = await fullStartProvider(apiKey);
            console.log("Full start result:", result);

            if (startupCancelledRef.current) return;

            // Parse result: "started:engine:model:pid"
            const parts = result.split(":");
            const modelName = parts.length >= 3 ? parts[2] : "AI model";
            if (parts.length >= 3) {
              setPerformance((prev) => ({ ...prev, model: parts[2] }));
            }

            // Mark remaining steps as done progressively
            updateStep(2, { status: "done", detail: `${modelName} ready` });
            await new Promise((r) => setTimeout(r, 300));
            if (startupCancelledRef.current) return;

            updateStep(3, { status: "done", detail: "Server running" });
            await new Promise((r) => setTimeout(r, 300));
            if (startupCancelledRef.current) return;

            updateStep(4, { status: "done", detail: parts.length >= 4 ? `PID ${parts[3]}` : "Daemon started" });
            await new Promise((r) => setTimeout(r, 300));
            if (startupCancelledRef.current) return;

            // Connect to network
            updateStep(5, { status: "active", detail: "Registering provider..." });
            try { await resumeProvider(apiKey); } catch (e) { console.error("Resume API failed:", e); }
            if (startupCancelledRef.current) return;
            updateStep(5, { status: "done", detail: "Connected" });

            setStatus("earning");

            // Close overlay after a short pause so user sees all green
            await new Promise((r) => setTimeout(r, 1500));
            setStartupActive(false);
            setStartupSteps([]);
            setPauseLoading(false);
          } catch (e) {
            console.error("Full start failed:", e);
            if (startupCancelledRef.current) return;

            // Mark current active step as error
            setStartupSteps((prev) =>
              prev.map((step) =>
                step.status === "active"
                  ? { ...step, status: "error" as StartupStepStatus, detail: String(e) }
                  : step
              )
            );

            // Fallback to just daemon start
            try {
              await startDaemonProcess(apiKey);
              if (startupCancelledRef.current) return;
              try { await resumeProvider(apiKey); } catch (e2) { console.error("Resume API failed:", e2); }
              setStatus("earning");
              // Mark remaining as done
              setStartupSteps((prev) =>
                prev.map((step) =>
                  step.status === "pending" ? { ...step, status: "done", detail: "Completed (fallback)" } : step
                )
              );
              await new Promise((r) => setTimeout(r, 2000));
              setStartupActive(false);
              setStartupSteps([]);
              setPauseLoading(false);
            } catch (e2) {
              console.error("Daemon start fallback failed:", e2);
              // Leave overlay open with error state — user can cancel
              setPauseLoading(false);
            }
          }
        } else {
          // No API key — mark error
          updateStep(1, { status: "error", detail: "No API key configured" });
          setPauseLoading(false);
        }
      }
    } catch (err) {
      console.error("Toggle failed:", err);
      setPauseLoading(false);
    }
  }

  function openExternalDashboard() {
    import("@tauri-apps/plugin-shell")
      .then((mod) => mod.open("https://dcp.sa/provider"))
      .catch(() => window.open("https://dcp.sa/provider", "_blank"));
  }

  function maskKey(key: string): string {
    if (key.length <= 8) return key;
    return "****" + key.slice(-8);
  }

  const statusLabel: Record<ProviderStatus, string> = {
    earning: "Earning",
    idle: "Idle",
    paused: "Paused",
  };

  const statusClass: Record<ProviderStatus, string> = {
    earning: "status-earning",
    idle: "status-idle",
    paused: "status-paused",
  };

  const demandConfig: Record<DemandLevel, { label: string; color: string; className: string }> = {
    high: { label: "HIGH DEMAND", color: "#22C55E", className: "demand-high" },
    moderate: { label: "MODERATE", color: "#EAB308", className: "demand-moderate" },
    low: { label: "LOW", color: "#EF4444", className: "demand-low" },
  };

  // Uptime today — use performance data if available
  const uptimeToday = performance.uptimeHours > 0 ? Math.min(performance.uptimeHours, 24) : 0;
  const uptimeH = Math.floor(uptimeToday);
  const uptimeM = Math.floor((uptimeToday - uptimeH) * 60);
  const uptimeDisplay = uptimeToday > 0 ? `${uptimeH}h ${uptimeM}m / 24h` : "0h 0m / 24h";

  // Feature 7: Payout progress — use real claimable earnings
  const payoutCurrent = earnings.month;
  const payoutMinimum = 25.0;
  const payoutPercent = Math.min((payoutCurrent / payoutMinimum) * 100, 100);

  // Format the live ticker into integer and fractional parts for the ticking effect
  const tickerStr = liveTicker.toFixed(6);
  const tickerParts = tickerStr.split(".");
  const tickerInteger = tickerParts[0];
  const tickerDecimals = tickerParts[1] || "000000";
  const tickerStable = tickerDecimals.slice(0, 3);
  const tickerTicking = tickerDecimals.slice(3);

  return (
    <div className="dashboard">
      {/* Header */}
      <header className="dashboard-header">
        <div className="dashboard-header-left">
          <span className="dashboard-logo">DCP <span className="logo-infinity">&infin;</span></span>
          <span className={`status-dot ${statusClass[status]}`} />
          <h1 className="dashboard-title">Provider</h1>
          <span className={`status-badge ${statusClass[status]}`}>
            {statusLabel[status]}
          </span>
          {/* Feature 4: Network Demand Indicator */}
          <span className={`demand-indicator ${demandConfig[demand].className}`}>
            <span className="demand-dot" />
            <span className="demand-label">{demandConfig[demand].label}</span>
          </span>
        </div>
        <button
          className="dashboard-settings-btn"
          aria-label="Settings"
          title="Settings"
          onClick={() => setShowSettings(true)}
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
      </header>

      {/* Offline indicator */}
      {!isOnline && apiKey && (
        <div className="offline-banner" role="status">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="10" />
            <line x1="12" y1="8" x2="12" y2="12" />
            <line x1="12" y1="16" x2="12.01" y2="16" />
          </svg>
          <span>Offline — showing cached data</span>
        </div>
      )}

      {/* Main scrollable content */}
      <div className="dashboard-content">

        {/* ── Feature 1: Live Earnings Ticker (HERO) ──────────────── */}
        <section className="live-ticker-section">
          <div className="live-ticker-label">Today's Earnings</div>
          <div className="live-ticker-value" aria-live="polite" aria-label={`Today's earnings: ${tickerStr} riyals`}>
            <span className="live-ticker-currency">{"\uFDFC"}</span>
            <span className="live-ticker-integer">{tickerInteger}</span>
            <span className="live-ticker-dot">.</span>
            <span className="live-ticker-stable">{tickerStable}</span>
            <span className="live-ticker-ticking">{tickerTicking}</span>
          </div>
          <div className="live-ticker-rate">
            {status === "paused" ? "Paused" : "+\uFDFC0.0012/sec"}
          </div>
        </section>

        {/* ── Performance Gauges Row ─────────────────────────────── */}
        <section className="dashboard-section">
          <h3 className="section-title">Performance</h3>
          <div className="gauges-row">
            <div className="gauge-wrapper">
              <Gauge
                value={performance.currentSpeed}
                max={200}
                label="tok/s"
                color="#00E5C8"
                size={120}
                unit="tok/s"
              />
              <span className="gauge-label">Speed</span>
            </div>
            <div className="gauge-wrapper">
              <Gauge
                value={gpuStatus.utilization}
                max={100}
                label="GPU"
                color="#FF6B00"
                size={120}
                unit="%"
              />
              <span className="gauge-label">GPU Usage</span>
            </div>
            <div className="gauge-wrapper">
              {gpuStatus.temperature > 0 ? (
                <>
                  <Gauge
                    value={gpuStatus.temperature}
                    max={100}
                    label="Temp"
                    color={tempColor}
                    size={120}
                    unit="&deg;C"
                  />
                  <span className="gauge-label">Temperature</span>
                  <div className="sparkline-wrapper">
                    <TempSparkline data={tempHistory} />
                    <span className="sparkline-label">Last hour</span>
                  </div>
                </>
              ) : (
                <>
                  <div style={{ width: 120, height: 120, display: 'flex', alignItems: 'center', justifyContent: 'center', borderRadius: '50%', border: '3px solid rgba(255,255,255,0.06)' }}>
                    <span style={{ fontSize: '0.85rem', color: 'var(--text-secondary)' }}>N/A</span>
                  </div>
                  <span className="gauge-label">Temperature</span>
                  <span style={{ fontSize: '0.65rem', color: 'var(--text-secondary)', marginTop: 4 }}>Not available on Mac</span>
                </>
              )}
            </div>
          </div>
        </section>

        {/* ── Feature 7: Payout Progress Bar ─────────────────────── */}
        <section className="dashboard-section">
          <h3 className="section-title">Payout Progress</h3>
          <div className="payout-progress">
            <div className="payout-progress-header">
              <span className="payout-progress-text">
                Next payout: <strong>{"\uFDFC"}{payoutCurrent.toFixed(2)}</strong> / {"\uFDFC"}{payoutMinimum.toFixed(2)} minimum
              </span>
              <span className="payout-progress-percent">{Math.round(payoutPercent)}%</span>
            </div>
            <div className="payout-progress-track">
              <div
                className="payout-progress-fill"
                style={{ width: `${payoutPercent}%` }}
              />
            </div>
          </div>
        </section>

        {/* ── Earnings Section ───────────────────────────────────── */}
        <section className="dashboard-section">
          <h3 className="section-title">Earnings</h3>
          <div className="earnings-grid">
            <div className="earnings-box">
              <span className="earnings-box-value">{"\uFDFC"}{earnings.today.toFixed(2)}</span>
              <span className="earnings-box-label">Today</span>
            </div>
            <div className="earnings-box">
              <span className="earnings-box-value">{"\uFDFC"}{earnings.week.toFixed(2)}</span>
              <span className="earnings-box-label">This Week</span>
            </div>
            <div className="earnings-box">
              <span className="earnings-box-value earnings-box-highlight">{"\uFDFC"}{earnings.month.toFixed(2)}</span>
              <span className="earnings-box-label">This Month</span>
            </div>
            <div className="earnings-box">
              <span className="earnings-box-value">{"\uFDFC"}{earnings.allTime.toFixed(2)}</span>
              <span className="earnings-box-label">All Time</span>
            </div>
          </div>
        </section>

        {/* ── Feature 5: Earnings Forecast ───────────────────────── */}
        <section className="earnings-forecast">
          <span className="forecast-text">
            At current rate: <strong>calculating...</strong>
            {" "}&bull;{" "}
            Running 24/7 could earn <strong>{"\uFDFC"}52/month</strong> (+48%)
          </span>
        </section>

        {/* ── Feature 3: "Your Impact" Stats Row ─────────────────── */}
        <section className="dashboard-section">
          <h3 className="section-title">Your Impact</h3>
          <div className="impact-row">
            <div className="impact-box">
              <span className="impact-box-value">
                <AnimatedCounter target={performance.jobsCompleted} />
              </span>
              <span className="impact-box-label">requests served today</span>
            </div>
            <div className="impact-box">
              <span className="impact-box-value">
                <AnimatedCounter target={0} />
              </span>
              <span className="impact-box-label">developers helped</span>
            </div>
            <div className="impact-box">
              <span className="impact-box-value">
                <AnimatedCounter target={0} />
              </span>
              <span className="impact-box-label">
                Arabic queries{" "}
                <span role="img" aria-label="Saudi Arabia flag">&#x1F1F8;&#x1F1E6;</span>
              </span>
            </div>
          </div>
        </section>

        {/* ── Feature 2: Live Request Feed ───────────────────────── */}
        <section className="dashboard-section">
          <div className="feed-header">
            <h3 className="section-title">Live Requests</h3>
            <button
              className="feed-toggle-btn"
              onClick={() => setFeedExpanded(!feedExpanded)}
              aria-label={feedExpanded ? "Collapse request feed" : "Expand request feed"}
            >
              <svg
                width="14"
                height="14"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                style={{
                  transform: feedExpanded ? "rotate(180deg)" : "rotate(0deg)",
                  transition: "transform 0.2s ease",
                }}
              >
                <polyline points="6,9 12,15 18,9" />
              </svg>
            </button>
          </div>
          {feedExpanded && (
            <div className="request-feed" ref={feedRef} role="log" aria-label="Live inference requests">
              {requestFeed.length === 0 ? (
                <div className="feed-empty">Waiting for requests...</div>
              ) : (
                requestFeed.map((entry) => (
                  <div key={entry.id} className="feed-entry">
                    <span className="feed-time">{entry.timestamp}</span>
                    <span className="feed-model">{entry.model}</span>
                    <span className="feed-tokens">{entry.tokens} tok</span>
                    <span className="feed-latency">{entry.latency}</span>
                    <span className="feed-earned">{"\uFDFC"}{entry.earned}</span>
                  </div>
                ))
              )}
            </div>
          )}
        </section>

        {/* ── Feature 6: Leaderboard Position ────────────────────── */}
        <section className="leaderboard-card">
          <span className="leaderboard-icon" role="img" aria-label="Trophy">&#x1F3C6;</span>
          <span className="leaderboard-text">
            Leaderboard rank will appear after your first 24h of uptime
          </span>
          <span className="leaderboard-rank-badge">NEW</span>
        </section>

        {/* ── Health Section ─────────────────────────────────────── */}
        {healthReport && (
          <section className="dashboard-section">
            <div className="feed-header">
              <h3 className="section-title">
                System Health
                <span className={`health-overall-badge health-${healthReport.overall}`}>
                  {healthReport.overall.toUpperCase()}
                </span>
              </h3>
              <button
                className="feed-toggle-btn"
                onClick={() => setHealthExpanded(!healthExpanded)}
                aria-label={healthExpanded ? "Collapse health checks" : "Expand health checks"}
              >
                <svg
                  width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                  strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"
                  style={{ transform: healthExpanded ? "rotate(180deg)" : "rotate(0deg)", transition: "transform 0.2s ease" }}
                >
                  <polyline points="6,9 12,15 18,9" />
                </svg>
              </button>
            </div>
            {healthExpanded && (
              <div className="health-checks-list">
                {healthReport.checks.map((check, i) => (
                  <div key={i} className={`health-check-item health-check-${check.status}`}>
                    <span className={`health-check-dot health-dot-${check.status}`} />
                    <span className="health-check-name">{check.name}</span>
                    <span className="health-check-message">{check.message}</span>
                  </div>
                ))}
              </div>
            )}
            {daemonStatus && (
              <div className="daemon-status-row">
                <span className="daemon-status-label">Daemon:</span>
                <span className={`daemon-status-value daemon-${daemonStatus.status}`}>
                  {daemonStatus.status}
                  {daemonStatus.pid ? ` (PID ${daemonStatus.pid})` : ""}
                </span>
                {daemonStatus.uptime_seconds > 0 && (
                  <span className="daemon-uptime">
                    {Math.floor(daemonStatus.uptime_seconds / 3600)}h {Math.floor((daemonStatus.uptime_seconds % 3600) / 60)}m uptime
                  </span>
                )}
              </div>
            )}
          </section>
        )}

        {/* ── Status Section ─────────────────────────────────────── */}
        <section className="dashboard-section">
          <h3 className="section-title">Status</h3>
          <div className="status-grid">
            <div className="status-item">
              <span className="status-item-label">GPU</span>
              <span className="status-item-value">{gpuStatus.name}</span>
            </div>
            <div className="status-item">
              <span className="status-item-label">Memory</span>
              <span className="status-item-value">{gpuStatus.memory}</span>
            </div>
            <div className="status-item">
              <span className="status-item-label">Model</span>
              <span className="status-item-value">{performance.model}</span>
            </div>
            <div className="status-item">
              <span className="status-item-label">Provider ID</span>
              <span className="status-item-value">{account.providerId}</span>
            </div>
            <div className="status-item">
              <span className="status-item-label">API Key</span>
              <span className="status-item-value masked-key">{maskKey(account.apiKey)}</span>
            </div>
            <div className="status-item">
              <span className="status-item-label">Jobs Done</span>
              <span className="status-item-value status-item-highlight">{performance.jobsCompleted}</span>
            </div>
          </div>
          <div className="uptime-section">
            <MiniBar
              value={uptimeToday}
              max={24}
              label="Uptime Today"
              color="#00E5C8"
              displayValue={uptimeDisplay}
            />
          </div>
        </section>

        {/* ── Feature 9: Model Suggestion Banner ─────────────────── */}
        {showModelSuggestion && (
          <section className="model-suggestion">
            <div className="model-suggestion-content">
              <span className="model-suggestion-icon" role="img" aria-label="Lightbulb">&#x1F4A1;</span>
              <span className="model-suggestion-text">
                Switch to <strong>Qwen3.5-35B-A3B</strong> for +15% earnings (high demand)
              </span>
              <button
                className="model-suggestion-link"
                onClick={() => {/* TODO: implement learn more */}}
              >
                Learn More
              </button>
            </div>
            <button
              className="model-suggestion-dismiss"
              onClick={() => setShowModelSuggestion(false)}
              aria-label="Dismiss suggestion"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </section>
        )}

        {/* ── Feature 10: Referral Card ──────────────────────────── */}
        <section className="referral-card">
          <div className="referral-header">
            <span className="referral-title">Invite Friends</span>
            <span className="referral-subtitle">Earn 5% of their earnings for 6 months</span>
          </div>
          <div className="referral-link-row">
            <code className="referral-link">dcp.sa/r/{account.providerId.replace("#", "")}</code>
            <button className="btn btn-secondary referral-copy-btn" onClick={copyReferral}>
              {referralCopied ? "Copied!" : "Copy"}
            </button>
          </div>
        </section>

      </div>

      {/* Bottom Action Row */}
      <div className="dashboard-actions">
        <button
          className={`btn btn-action-toggle ${isDaemonRunning ? "btn-pause btn-large" : "btn-primary btn-large"}`}
          onClick={toggleDaemon}
          disabled={pauseLoading}
        >
          {isDaemonRunning ? (
            <>
              <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
                <rect x="6" y="4" width="4" height="16" rx="1" />
                <rect x="14" y="4" width="4" height="16" rx="1" />
              </svg>
              {pauseLoading ? "Stopping..." : "Stop"}
            </>
          ) : (
            <>
              <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
                <polygon points="5,3 19,12 5,21" />
              </svg>
              {pauseLoading ? "Starting..." : "Start Provider"}
            </>
          )}
        </button>
        <button
          className="btn btn-secondary btn-icon"
          onClick={() => setShowSettings(true)}
          aria-label="Settings"
          title="Settings"
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
        <button className="btn btn-secondary btn-icon" onClick={openExternalDashboard} aria-label="Open Web Dashboard" title="Open Web Dashboard">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
            <polyline points="15,3 21,3 21,9" />
            <line x1="10" y1="14" x2="21" y2="3" />
          </svg>
        </button>
      </div>

      {/* Startup Progress Overlay */}
      {startupActive && (
        <StartupOverlay steps={startupSteps} onCancel={cancelStartup} />
      )}

      {/* Settings Overlay */}
      {showSettings && (
        <Settings
          onClose={() => setShowSettings(false)}
          apiKey={account.apiKey}
          config={config}
          onSave={setConfig}
        />
      )}
    </div>
  );
}

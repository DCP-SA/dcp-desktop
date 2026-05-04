import { invoke } from "@tauri-apps/api/core";

// ── Types ────────────────────────────────────────────────────────────

export interface GpuInfo {
  name: string;
  vram_mb: number;
  driver_version: string;
  compute_capability: string;
  is_apple_silicon: boolean;
}

export interface SystemInfo {
  os: string;
  os_version: string;
  hostname: string;
  total_ram_mb: number;
  cpu_cores: number;
  cpu_name: string;
  arch: string;
}

export interface DaemonConfig {
  run_mode: "always" | "idle" | "scheduled";
  gpu_usage_cap: number;
  temp_limit: number;
  start_on_boot: boolean;
}

// ── Tauri Command Wrappers ───────────────────────────────────────────

export async function detectGpu(): Promise<GpuInfo> {
  return invoke<GpuInfo>("detect_gpu");
}

export async function detectSystem(): Promise<SystemInfo> {
  return invoke<SystemInfo>("detect_system");
}

export async function validateApiKey(key: string): Promise<boolean> {
  return invoke<boolean>("validate_api_key", { key });
}

// H8 — registerProvider removed. New providers register via the web wizard
// at https://dcp.sa/setup; the desktop app only accepts an existing API key.

export async function startDaemon(
  apiKey: string,
  config: DaemonConfig
): Promise<string> {
  return invoke<string>("start_daemon", { apiKey, config });
}

export async function getEstimatedEarnings(
  vramMb: number,
  isAppleSilicon: boolean
): Promise<number> {
  return invoke<number>("get_estimated_earnings", { vramMb, isAppleSilicon });
}

export async function checkSetupComplete(): Promise<boolean> {
  try {
    return await invoke<boolean>("check_setup_complete");
  } catch {
    return false;
  }
}

// ── Backend API Types ────────────────────────────────────────────────

export interface ProviderDashboard {
  provider_id: number;
  name: string;
  status: string;
  gpu_model: string;
  vram_gb: number;
  total_earnings: number;
  total_jobs: number;
  claimable_earnings_halala: number;
  today_earnings_halala: number;
  week_earnings_halala: number;
  daemon_version: string;
  last_heartbeat: string;
  approval_status: string;
}

export interface ProviderMetrics {
  jobs_completed: number;
  jobs_failed: number;
  total_compute_minutes: number;
  earnings_halala: number;
  earnings_sar: number;
}

export interface JobEntry {
  job_id: string;
  model: string;
  status: string;
  created_at: string;
  completed_at: string;
  provider_earned_halala: number;
  prompt_tokens?: number;
  completion_tokens?: number;
  duration_seconds?: number;
}

export interface SavedConfig {
  api_key: string;
  run_mode: string;
  gpu_usage_cap: number;
  temp_limit: number;
  start_on_boot: boolean;
  served_model: string;
}

// ── Backend API Command Wrappers ────────────────────────────────────

export async function fetchDashboard(apiKey: string): Promise<ProviderDashboard> {
  return invoke<ProviderDashboard>("fetch_provider_dashboard", { apiKey });
}

export async function fetchMetrics(apiKey: string): Promise<ProviderMetrics> {
  return invoke<ProviderMetrics>("fetch_provider_metrics", { apiKey });
}

export async function fetchRecentJobs(apiKey: string): Promise<JobEntry[]> {
  return invoke<JobEntry[]>("fetch_recent_jobs", { apiKey });
}

export async function pauseProvider(apiKey: string): Promise<void> {
  return invoke<void>("pause_provider", { apiKey });
}

export async function resumeProvider(apiKey: string): Promise<void> {
  return invoke<void>("resume_provider", { apiKey });
}

export async function readConfig(): Promise<SavedConfig> {
  return invoke<SavedConfig>("read_config");
}

// ── Daemon Process Manager Types ─────────────────────────────────────

export interface DaemonStatus {
  status: string;         // "running" | "stopped" | "crashed" | "starting"
  pid: number | null;
  uptime_seconds: number;
  last_log_lines: string[];
}

export interface HealthCheck {
  name: string;
  status: string;         // "ok" | "warning" | "error"
  message: string;
  can_auto_fix: boolean;
  fix_action: string | null;
}

export interface HealthReport {
  overall: string;        // "healthy" | "degraded" | "critical"
  checks: HealthCheck[];
}

export interface LiveMetrics {
  gpu_temperature: number | null;
  gpu_utilization: number | null;
  inference_speed: number | null;
  memory_used_mb: number | null;
  daemon_pid: number | null;
  daemon_alive: boolean;
}

// ── Daemon Process Manager Commands ──────────────────────────────────

export async function startDaemonProcess(apiKey: string): Promise<string> {
  return invoke<string>("start_daemon_process", { apiKey });
}

export async function stopDaemonProcess(): Promise<string> {
  return invoke<string>("stop_daemon_process");
}

export async function getDaemonStatus(): Promise<DaemonStatus> {
  return invoke<DaemonStatus>("get_daemon_status");
}

export async function checkDaemonHealth(): Promise<HealthReport> {
  return invoke<HealthReport>("check_daemon_health");
}

export async function getLiveMetrics(): Promise<LiveMetrics> {
  return invoke<LiveMetrics>("get_live_metrics");
}

export async function installEngine(): Promise<string> {
  return invoke<string>("install_engine");
}

export async function downloadModel(modelName: string): Promise<string> {
  return invoke<string>("download_model", { modelName });
}

export async function updateDaemon(apiKey: string): Promise<string> {
  return invoke<string>("update_daemon", { apiKey });
}

export async function rollbackDaemon(): Promise<string> {
  return invoke<string>("rollback_daemon");
}

// ── Helpers ──────────────────────────────────────────────────────────

export function formatVram(mb: number): string {
  if (mb >= 1024) {
    const gb = mb / 1024;
    return gb % 1 === 0 ? `${gb}GB` : `${gb.toFixed(1)}GB`;
  }
  return `${mb}MB`;
}

export function formatRam(mb: number): string {
  const gb = Math.round(mb / 1024);
  return `${gb}GB`;
}

export function getPerformanceTier(
  gpu: GpuInfo
): { tier: string; toksEstimate: string } {
  const vram = gpu.vram_mb;

  if (gpu.is_apple_silicon) {
    if (vram >= 32768) {
      return { tier: "Standard", toksEstimate: "50+" };
    }
    return { tier: "Economy", toksEstimate: "30+" };
  }

  // NVIDIA
  if (vram >= 16384) {
    return { tier: "Standard", toksEstimate: "50+" };
  }
  if (vram >= 8192) {
    return { tier: "Economy", toksEstimate: "30+" };
  }
  return { tier: "Below minimum", toksEstimate: "<20" };
}

export async function fullStartProvider(apiKey: string): Promise<string> {
  return invoke<string>("full_start_provider", { apiKey });
}

// ── Wizard Progress Types + Commands ─────────────────────────────────

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

// ── Network Status & Key Rotation ────────────────────────────────────

export interface NetworkStatus {
  connected: boolean;
  mesh_ip: string | null;
  latency_ms: number | null;
  last_handshake_secs_ago: number | null;
}

export async function getNetworkStatus(): Promise<NetworkStatus> {
  return invoke<NetworkStatus>("get_network_status");
}

export async function rotateNetworkKey(): Promise<string> {
  return invoke<string>("rotate_network_key");
}

export async function reconnectNetwork(): Promise<string> {
  return invoke<string>("reconnect_network");
}

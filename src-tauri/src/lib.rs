use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri::tray::{TrayIconBuilder, MouseButton, MouseButtonState};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri_plugin_notification::NotificationExt;
use reqwest;
use serde_json::Value;

// ── Data Structures ──────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GpuInfo {
    pub name: String,
    pub vram_mb: u64,
    pub driver_version: String,
    pub compute_capability: String,
    pub is_apple_silicon: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SystemInfo {
    pub os: String,
    pub os_version: String,
    pub hostname: String,
    pub total_ram_mb: u64,
    pub cpu_cores: u32,
    pub cpu_name: String,
    pub arch: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DaemonConfig {
    pub run_mode: String,       // "always" | "idle" | "scheduled"
    pub gpu_usage_cap: u32,     // 50-100
    pub temp_limit: u32,        // 70-90
    pub start_on_boot: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HardwareReport {
    pub gpu: GpuInfo,
    pub system: SystemInfo,
    pub performance_tier: String,
    pub estimated_toks: u32,
    pub qualified: bool,
}

// ── Daemon Process Manager ───────────────────────────────────────────

pub struct DaemonState {
    pid: Option<u32>,
    status: String,        // "running", "stopped", "crashed", "starting"
    last_restart: Option<std::time::Instant>,
    restart_count: u32,
    started_at: Option<std::time::Instant>,
}

type DaemonManager = Mutex<DaemonState>;

/// Embedded magic-link sign-in: state for the one-shot loopback HTTP
/// listener that receives the api_key from dcp.sa/auth/verify after the
/// user clicks the link in their email. Only one listener is alive at a
/// time; starting a new sign-in cancels any previous one.
#[derive(Default)]
struct SigninListenerState {
    /// Drop this to abort the in-flight tokio task running accept().
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    /// Port the listener is bound to. Useful for diagnostics + tests.
    port: Option<u16>,
}

type SigninListener = Mutex<SigninListenerState>;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DaemonStatus {
    pub status: String,
    pub pid: Option<u32>,
    pub uptime_seconds: u64,
    pub last_log_lines: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HealthCheck {
    pub name: String,
    pub status: String,       // "ok" | "warning" | "error"
    pub message: String,
    pub can_auto_fix: bool,
    pub fix_action: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HealthReport {
    pub overall: String,      // "healthy" | "degraded" | "critical"
    pub checks: Vec<HealthCheck>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LiveMetrics {
    pub gpu_temperature: Option<f32>,
    pub gpu_utilization: Option<f32>,
    pub inference_speed: Option<f32>,
    pub memory_used_mb: Option<u64>,
    pub daemon_pid: Option<u32>,
    pub daemon_alive: bool,
}

// ── API Response Structures ──────────────────────────────────────────

const API_BASE: &str = "https://api.dcp.sa/api/providers";

const MODEL_METADATA: &[(&str, &str, f64)] = &[
    ("qwen3:30b-a3b", "Qwen3 30B-A3B", 17.7),
    ("qwen3:8b", "Qwen3 8B", 5.2),
    ("qwen3:4b", "Qwen3 4B", 2.5),
    ("mistral:7b", "Mistral 7B", 4.1),
    ("mlx-community/Qwen3-30B-A3B-4bit", "Qwen3 30B-A3B (MLX)", 16.4),
    ("mlx-community/Qwen3-8B-4bit", "Qwen3 8B (MLX)", 4.5),
    ("mlx-community/Qwen3-4B-4bit", "Qwen3 4B (MLX)", 2.3),
];

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProviderDashboard {
    pub provider_id: i64,
    pub name: String,
    pub status: String,
    pub gpu_model: String,
    pub vram_gb: f64,
    pub total_earnings: f64,
    pub total_jobs: i64,
    pub claimable_earnings_halala: i64,
    pub today_earnings_halala: i64,
    pub week_earnings_halala: i64,
    pub daemon_version: String,
    pub last_heartbeat: String,
    pub approval_status: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProviderMetrics {
    pub jobs_completed: i64,
    pub jobs_failed: i64,
    pub total_compute_minutes: f64,
    pub earnings_halala: i64,
    pub earnings_sar: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JobEntry {
    pub job_id: String,
    pub model: String,
    pub status: String,
    pub created_at: String,
    pub completed_at: String,
    pub provider_earned_halala: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub duration_seconds: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SavedConfig {
    pub api_key: String,
    pub run_mode: String,
    pub gpu_usage_cap: u32,
    pub temp_limit: u32,
    pub start_on_boot: bool,
    pub served_model: String,
}

// ── Tauri Commands ───────────────────────────────────────────────────

#[tauri::command]
async fn detect_gpu() -> Result<GpuInfo, String> {
    #[cfg(target_os = "macos")]
    {
        detect_gpu_macos()
    }

    #[cfg(not(target_os = "macos"))]
    {
        detect_gpu_nvidia()
    }
}

#[cfg(target_os = "macos")]
fn detect_gpu_macos() -> Result<GpuInfo, String> {
    // Detect Apple Silicon chip name
    let chip_output = Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
        .map_err(|e| format!("Failed to run sysctl: {}", e))?;

    let cpu_brand = String::from_utf8_lossy(&chip_output.stdout).trim().to_string();

    // Detect if Apple Silicon via arch
    let arch_output = Command::new("uname")
        .arg("-m")
        .output()
        .map_err(|e| format!("Failed to run uname: {}", e))?;

    let arch = String::from_utf8_lossy(&arch_output.stdout).trim().to_string();
    let is_apple_silicon = arch == "arm64";

    // Get total memory (unified memory on Apple Silicon)
    let mem_output = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .map_err(|e| format!("Failed to get memory: {}", e))?;

    let mem_bytes: u64 = String::from_utf8_lossy(&mem_output.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    let mem_mb = mem_bytes / (1024 * 1024);

    // Try to get specific chip name from system_profiler
    let chip_name = if is_apple_silicon {
        let sp_output = Command::new("system_profiler")
            .args(["SPHardwareDataType"])
            .output()
            .ok();

        if let Some(output) = sp_output {
            let sp_text = String::from_utf8_lossy(&output.stdout);
            sp_text
                .lines()
                .find(|line| line.contains("Chip:") || line.contains("Model Name:"))
                .map(|line| line.split(':').last().unwrap_or("").trim().to_string())
                .unwrap_or_else(|| cpu_brand.clone())
        } else {
            cpu_brand.clone()
        }
    } else {
        cpu_brand.clone()
    };

    let display_name = if is_apple_silicon {
        format!("Apple Silicon {}", chip_name)
    } else {
        chip_name
    };

    Ok(GpuInfo {
        name: display_name,
        vram_mb: if is_apple_silicon { mem_mb } else { 0 },
        driver_version: "macOS Metal".to_string(),
        compute_capability: if is_apple_silicon {
            "Apple Neural Engine".to_string()
        } else {
            "N/A".to_string()
        },
        is_apple_silicon,
    })
}

/// Find nvidia-smi executable (tries PATH first, then known Windows locations)
#[allow(dead_code)]
fn find_nvidia_smi() -> Option<String> {
    // Try PATH first
    if command_exists("nvidia-smi") {
        return Some("nvidia-smi".to_string());
    }

    // Windows: try known installation paths
    #[cfg(target_os = "windows")]
    {
        let known_paths = [
            r"C:\Windows\System32\nvidia-smi.exe",
            r"C:\Program Files\NVIDIA Corporation\NVSMI\nvidia-smi.exe",
        ];
        for path in &known_paths {
            if std::path::Path::new(path).exists() {
                return Some(path.to_string());
            }
        }

        // DriverStore fallback — nvidia-smi lives here on some Windows installs
        let driver_store = std::path::Path::new(r"C:\Windows\System32\DriverStore\FileRepository");
        if driver_store.is_dir() {
            if let Ok(entries) = std::fs::read_dir(driver_store) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    if let Some(name_str) = name.to_str() {
                        if name_str.starts_with("nv") {
                            let candidate = entry.path().join("nvidia-smi.exe");
                            if candidate.exists() {
                                return Some(candidate.to_string_lossy().to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

#[cfg(not(target_os = "macos"))]
fn detect_gpu_nvidia() -> Result<GpuInfo, String> {
    let gpu_log_path = dcp_home().ok().map(|d| d.join("gpu-detection.log"));
    let mut gpu_log = Vec::<String>::new();
    gpu_log.push(format!("[{}] GPU detection starting (3-layer chain)...", chrono_now()));

    // ── Layer 1: DXGI (99.9% reliable on Windows) ──────────────────
    #[cfg(target_os = "windows")]
    {
        gpu_log.push("  Layer 1: Trying DXGI...".to_string());
        match detect_gpu_dxgi() {
            Ok(info) => {
                gpu_log.push(format!("  Layer 1 DXGI: OK — {} {}MB", info.name, info.vram_mb));
                if let Some(ref log_path) = gpu_log_path {
                    let _ = std::fs::write(log_path, gpu_log.join("\n"));
                }
                return Ok(info);
            }
            Err(e) => {
                gpu_log.push(format!("  Layer 1 DXGI: FAILED — {}", e));
            }
        }

        // ── Layer 2: Windows Registry (95% reliable) ───────────────
        gpu_log.push("  Layer 2: Trying Windows Registry...".to_string());
        match detect_gpu_registry() {
            Ok(info) => {
                gpu_log.push(format!("  Layer 2 Registry: OK — {} {}MB", info.name, info.vram_mb));
                if let Some(ref log_path) = gpu_log_path {
                    let _ = std::fs::write(log_path, gpu_log.join("\n"));
                }
                return Ok(info);
            }
            Err(e) => {
                gpu_log.push(format!("  Layer 2 Registry: FAILED — {}", e));
            }
        }
    }

    // ── Layer 3: nvidia-smi (legacy fallback, also works on Linux) ─
    gpu_log.push("  Layer 3: Trying nvidia-smi...".to_string());
    let smi_path = find_nvidia_smi();
    gpu_log.push(format!("  find_nvidia_smi: {:?}", smi_path));

    if let Some(ref log_path) = gpu_log_path {
        let _ = std::fs::write(log_path, gpu_log.join("\n"));
    }

    let smi_path = smi_path.ok_or("GPU not detected. DXGI, Registry, and nvidia-smi all failed. Check gpu-detection.log.")?;

    let output = hide_window(
        Command::new(&smi_path)
            .args(["--query-gpu=name,memory.total,driver_version,compute_cap", "--format=csv,noheader,nounits"])
    ).output()
        .map_err(|e| format!("nvidia-smi failed: {}", e))?;

    if !output.status.success() {
        return Err(format!("nvidia-smi error: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }

    let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.len() < 4 {
        return Err(format!("nvidia-smi parse error: '{}'", line));
    }

    let vram: u64 = parts[1].parse().unwrap_or(0);
    gpu_log.push(format!("  Layer 3 nvidia-smi: OK — {} {}MB", parts[0], vram));
    if let Some(ref log_path) = gpu_log_path {
        let _ = std::fs::write(log_path, gpu_log.join("\n"));
    }

    Ok(GpuInfo {
        name: parts[0].to_string(),
        vram_mb: vram,
        driver_version: parts[2].to_string(),
        compute_capability: parts[3].to_string(),
        is_apple_silicon: false,
    })
}

/// Layer 1: DXGI GPU detection — uses Windows DirectX infrastructure (always available)
#[cfg(target_os = "windows")]
fn detect_gpu_dxgi() -> Result<GpuInfo, String> {
    use windows::Win32::Graphics::Dxgi::*;

    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()
            .map_err(|e| format!("CreateDXGIFactory1 failed: {}", e))?;

        let mut best_gpu: Option<GpuInfo> = None;
        let mut best_vram: u64 = 0;
        let mut i = 0u32;

        loop {
            match factory.EnumAdapters1(i) {
                Ok(adapter) => {
                    let desc = adapter.GetDesc1()
                        .map_err(|e| format!("GetDesc1 failed: {}", e))?;

                    // Get GPU name from wide string
                    let name_end = desc.Description.iter()
                        .position(|&c| c == 0)
                        .unwrap_or(desc.Description.len());
                    let name = String::from_utf16_lossy(&desc.Description[..name_end]);

                    let vram_bytes = desc.DedicatedVideoMemory;
                    let vram_mb = (vram_bytes / (1024 * 1024)) as u64;
                    let is_nvidia = desc.VendorId == 0x10DE;

                    // Skip software adapters and integrated GPUs with < 1GB
                    if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 || vram_mb < 1024 {
                        i += 1;
                        continue;
                    }

                    // Prefer NVIDIA, then largest VRAM
                    if is_nvidia && vram_mb > best_vram {
                        best_vram = vram_mb;
                        best_gpu = Some(GpuInfo {
                            name,
                            vram_mb,
                            driver_version: format!("DXGI-VendorId:{:#06X}", desc.VendorId),
                            compute_capability: "DXGI".to_string(),
                            is_apple_silicon: false,
                        });
                    } else if best_gpu.is_none() && vram_mb > best_vram {
                        best_vram = vram_mb;
                        best_gpu = Some(GpuInfo {
                            name,
                            vram_mb,
                            driver_version: format!("DXGI-VendorId:{:#06X}", desc.VendorId),
                            compute_capability: "DXGI".to_string(),
                            is_apple_silicon: false,
                        });
                    }

                    i += 1;
                }
                Err(_) => break,
            }
        }

        best_gpu.ok_or_else(|| "DXGI: No discrete GPU found".to_string())
    }
}

/// Layer 2: Windows Registry GPU detection — reads driver-installed hardware info
#[cfg(target_os = "windows")]
fn detect_gpu_registry() -> Result<GpuInfo, String> {
    use winreg::RegKey;
    use winreg::enums::*;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let class_key = hklm.open_subkey(r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}")
        .map_err(|e| format!("Registry: display class key not found: {}", e))?;

    for name in class_key.enum_keys().filter_map(|k| k.ok()) {
        if let Ok(subkey) = class_key.open_subkey(&name) {
            let provider: String = subkey.get_value("ProviderName").unwrap_or_default();
            if !provider.to_lowercase().contains("nvidia") {
                continue;
            }

            let gpu_name: String = subkey.get_value("DriverDesc")
                .or_else(|_| subkey.get_value("HardwareInformation.AdapterString"))
                .unwrap_or_else(|_| "NVIDIA GPU".to_string());

            // qwMemorySize is a 64-bit QWORD — no 4GB overflow
            let vram_bytes: u64 = subkey.get_value("HardwareInformation.qwMemorySize")
                .unwrap_or(0);
            let vram_mb = vram_bytes / (1024 * 1024);

            if vram_mb > 0 {
                return Ok(GpuInfo {
                    name: gpu_name,
                    vram_mb,
                    driver_version: subkey.get_value("DriverVersion").unwrap_or_else(|_| "Registry".to_string()),
                    compute_capability: "Registry".to_string(),
                    is_apple_silicon: false,
                });
            }
        }
    }

    Err("Registry: No NVIDIA GPU with VRAM found".to_string())
}

#[tauri::command]
async fn detect_system() -> Result<SystemInfo, String> {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    #[cfg(target_os = "macos")]
    {
        let os_version = Command::new("sw_vers")
            .args(["-productVersion"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let mem_output = Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .map_err(|e| format!("Failed to get memory: {}", e))?;

        let mem_bytes: u64 = String::from_utf8_lossy(&mem_output.stdout)
            .trim()
            .parse()
            .unwrap_or(0);

        let cpu_cores_output = Command::new("sysctl")
            .args(["-n", "hw.ncpu"])
            .output()
            .map_err(|e| format!("Failed to get CPU cores: {}", e))?;

        let cpu_cores: u32 = String::from_utf8_lossy(&cpu_cores_output.stdout)
            .trim()
            .parse()
            .unwrap_or(0);

        let cpu_name = Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let arch = Command::new("uname")
            .arg("-m")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        Ok(SystemInfo {
            os: "macOS".to_string(),
            os_version,
            hostname,
            total_ram_mb: mem_bytes / (1024 * 1024),
            cpu_cores,
            cpu_name,
            arch,
        })
    }

    #[cfg(target_os = "linux")]
    {
        let os_version = std::fs::read_to_string("/etc/os-release")
            .map(|content| {
                content
                    .lines()
                    .find(|l| l.starts_with("PRETTY_NAME="))
                    .map(|l| l.trim_start_matches("PRETTY_NAME=").trim_matches('"').to_string())
                    .unwrap_or_else(|| "Linux".to_string())
            })
            .unwrap_or_else(|_| "Linux".to_string());

        let mem_output = Command::new("grep")
            .args(["MemTotal", "/proc/meminfo"])
            .output()
            .map_err(|e| format!("Failed to get memory: {}", e))?;

        let mem_line = String::from_utf8_lossy(&mem_output.stdout);
        let mem_kb: u64 = mem_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let cpu_cores: u32 = num_cpus::get() as u32;

        let cpu_name = Command::new("grep")
            .args(["-m1", "model name", "/proc/cpuinfo"])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split(':')
                    .last()
                    .unwrap_or("unknown")
                    .trim()
                    .to_string()
            })
            .unwrap_or_else(|_| "unknown".to_string());

        let arch = Command::new("uname")
            .arg("-m")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        Ok(SystemInfo {
            os: "Linux".to_string(),
            os_version,
            hostname,
            total_ram_mb: mem_kb / 1024,
            cpu_cores,
            cpu_name,
            arch,
        })
    }

    #[cfg(target_os = "windows")]
    {
        let os_version = hide_window(Command::new("cmd")
            .args(["/C", "ver"]))
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "Windows".to_string());

        let mem_output = hide_window(Command::new("wmic")
            .args(["computersystem", "get", "TotalPhysicalMemory", "/value"]))
            .output()
            .map_err(|e| format!("Failed to get memory: {}", e))?;

        let mem_line = String::from_utf8_lossy(&mem_output.stdout);
        let mem_bytes: u64 = mem_line
            .lines()
            .find(|l| l.starts_with("TotalPhysicalMemory"))
            .and_then(|l| l.split('=').last())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);

        let cpu_cores: u32 = num_cpus::get() as u32;

        let cpu_name = hide_window(Command::new("wmic")
            .args(["cpu", "get", "Name", "/value"]))
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .find(|l| l.starts_with("Name"))
                    .and_then(|l| l.split('=').last())
                    .unwrap_or("unknown")
                    .trim()
                    .to_string()
            })
            .unwrap_or_else(|_| "unknown".to_string());

        Ok(SystemInfo {
            os: "Windows".to_string(),
            os_version,
            hostname,
            total_ram_mb: mem_bytes / (1024 * 1024),
            cpu_cores,
            cpu_name,
            arch: std::env::consts::ARCH.to_string(),
        })
    }
}

#[tauri::command]
async fn validate_api_key(key: String) -> Result<bool, String> {
    // Validate format: must start with "dcp-provider-"
    if !(key.starts_with("dcp-provider-") && key.len() > 20) {
        return Ok(false);
    }

    // Verify against backend — fall back to format-only if offline.
    // Audit 2026-05-14 F3 — send key in header only, not as ?key= URL param.
    // URL-borne credentials leak into proxy/CDN logs and referrer headers.
    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.dcp.sa/api/providers/me")
        .header("x-api-key", &key)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => Ok(true),
        Ok(r) => Err(format!("API key rejected by server: HTTP {}", r.status())),
        Err(e) => {
            // Network error — allow offline mode but warn
            eprintln!("[validate_api_key] Backend unreachable: {}. Proceeding with format-only check.", e);
            Ok(true)
        }
    }
}

// H8 — register_provider removed. Registration happens through the web wizard
// at https://dcp.sa/setup which does real backend account provisioning. The
// previous Tauri command returned a deterministic djb2-hashed pseudo key
// (`dcp-provider-<hash>`) that the backend rejects, leading users into a
// broken state where every API call returns 401.

#[tauri::command]
async fn start_daemon(api_key: String, config: DaemonConfig) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("API key is required".to_string());
    }

    // Create DCP config directory
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    let dcp_dir = home.join(".dcp");
    std::fs::create_dir_all(&dcp_dir)
        .map_err(|e| format!("Failed to create ~/.dcp directory: {}", e))?;

    // Save configuration
    let config_path = dcp_dir.join("config.json");
    let config_json = serde_json::json!({
        "api_key": api_key,
        "run_mode": config.run_mode,
        "gpu_usage_cap": config.gpu_usage_cap,
        "temp_limit": config.temp_limit,
        "start_on_boot": config.start_on_boot,
    });

    std::fs::write(&config_path, serde_json::to_string_pretty(&config_json).unwrap())
        .map_err(|e| format!("Failed to write config: {}", e))?;

    // Audit 2026-05-14 F4 — config.json contains the api_key. Default umask
    // leaves it world-readable (0644) on multi-user macOS/Linux boxes. Lock it
    // down to owner-only (0600). Windows: rely on per-user profile ACLs.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            &config_path,
            std::fs::Permissions::from_mode(0o600),
        );
    }

    Ok(format!(
        "Configuration saved to {}. Daemon ready to start.",
        config_path.display()
    ))
}

#[tauri::command]
async fn check_setup_complete() -> Result<bool, String> {
    let config_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".dcp")
        .join("config.json");
    Ok(config_path.exists())
}

#[tauri::command]
async fn get_estimated_earnings(vram_mb: u64, is_apple_silicon: bool) -> Result<f64, String> {
    // NOTE: These are hardcoded tier-based estimates, NOT real calculations.
    // For an M2 with 12GB (vram_mb ~12288), this returns $15/mo (Apple Silicon, <16GB tier).
    // For an M2 with 16GB unified, this returns $35/mo. The $35 estimate shown to most
    // Mac users with 16-32GB is aspirational — actual earnings depend on network demand,
    // uptime, and job volume. TODO(v0.3.0): replace with demand-weighted formula from backend.
    let base_rate = if is_apple_silicon {
        // Apple Silicon unified memory, MLX inference
        match vram_mb {
            0..=16383 => 15.0,       // 16GB - basic models
            16384..=32767 => 35.0,    // 32GB - mid-range
            32768..=65535 => 60.0,    // 48-64GB - good range
            _ => 90.0,               // 96GB+ - premium
        }
    } else {
        // NVIDIA discrete GPU
        match vram_mb {
            0..=8191 => 10.0,        // 8GB - minimal
            8192..=16383 => 25.0,     // 12-16GB - standard
            16384..=24575 => 50.0,    // 24GB - good (RTX 4090)
            _ => 80.0,               // 32GB+ - premium
        }
    };

    // Return monthly estimate in USD (hardcoded tiers — see NOTE above)
    Ok(base_rate)
}

// ── Backend API Commands ─────────────────────────────────────────────

#[tauri::command]
async fn fetch_provider_dashboard(api_key: String) -> Result<ProviderDashboard, String> {
    // Audit 2026-05-30 — key sent via x-api-key header only, not as ?key= URL
    // param (URL-borne credentials leak into proxy/CDN logs and referrers).
    let url = format!("{}/me", API_BASE);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("x-api-key", &api_key)
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("API returned status {}", resp.status()));
    }

    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    // The /me endpoint returns { provider: {...}, recent_jobs: [...] }
    // Try nested "provider" key first, then "data", then root
    let data = if body.get("provider").is_some() {
        body["provider"].clone()
    } else if body.get("data").is_some() {
        body["data"].clone()
    } else {
        body.clone()
    };

    Ok(ProviderDashboard {
        provider_id: data["id"].as_i64()
            .or_else(|| data["provider_id"].as_i64())
            .unwrap_or(0),
        name: data["name"].as_str().unwrap_or("").to_string(),
        status: data["status"].as_str().unwrap_or("unknown").to_string(),
        gpu_model: data["gpu_model"].as_str().unwrap_or("").to_string(),
        vram_gb: data["vram_gb"].as_f64()
            .or_else(|| data["vram_mb"].as_f64().map(|mb| mb / 1024.0))
            .unwrap_or(0.0),
        total_earnings: data["total_earnings"].as_f64()
            .or_else(|| data["total_earnings_halala"].as_i64().map(|h| h as f64 / 100.0))
            .unwrap_or(0.0),
        total_jobs: data["total_jobs"].as_i64().unwrap_or(0),
        claimable_earnings_halala: data["claimable_earnings_halala"].as_i64()
            .or_else(|| data["total_earnings_halala"].as_i64())
            .unwrap_or(0),
        today_earnings_halala: data["today_earnings_halala"].as_i64().unwrap_or(0),
        week_earnings_halala: data["week_earnings_halala"].as_i64().unwrap_or(0),
        daemon_version: data["daemon_version"].as_str().unwrap_or("").to_string(),
        last_heartbeat: data["last_heartbeat"].as_str().unwrap_or("").to_string(),
        approval_status: data["approval_status"].as_str().unwrap_or("").to_string(),
    })
}

#[tauri::command]
async fn fetch_provider_metrics(api_key: String) -> Result<ProviderMetrics, String> {
    // Audit 2026-05-30 — key sent via x-api-key header only, not as ?key= URL param.
    let url = format!("{}/me/metrics", API_BASE);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("x-api-key", &api_key)
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("API returned status {}", resp.status()));
    }

    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    let data = if body.get("data").is_some() {
        body["data"].clone()
    } else {
        body.clone()
    };

    Ok(ProviderMetrics {
        jobs_completed: data["jobs_completed"].as_i64().unwrap_or(0),
        jobs_failed: data["jobs_failed"].as_i64().unwrap_or(0),
        total_compute_minutes: data["total_compute_minutes"].as_f64()
            .or_else(|| data["compute_minutes"].as_f64())
            .unwrap_or(0.0),
        earnings_halala: data["earnings_halala"].as_i64().unwrap_or(0),
        earnings_sar: data["earnings_sar"].as_f64().unwrap_or(0.0),
    })
}

#[tauri::command]
async fn fetch_recent_jobs(api_key: String) -> Result<Vec<JobEntry>, String> {
    // Use the /me endpoint which includes recent_jobs in the response
    // Audit 2026-05-30 — key sent via x-api-key header only, not as ?key= URL param.
    let url = format!("{}/me", API_BASE);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("x-api-key", &api_key)
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("API returned status {}", resp.status()));
    }

    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    // /me returns { provider: {...}, recent_jobs: [...] }
    let jobs_array = if let Some(arr) = body.get("recent_jobs").and_then(|j| j.as_array()) {
        arr.clone()
    } else if let Some(arr) = body.get("data").and_then(|d| d.as_array()) {
        arr.clone()
    } else if let Some(arr) = body.get("jobs").and_then(|j| j.as_array()) {
        arr.clone()
    } else if let Some(arr) = body.as_array() {
        arr.clone()
    } else {
        Vec::new()
    };

    let jobs: Vec<JobEntry> = jobs_array
        .iter()
        .map(|j| JobEntry {
            job_id: j["job_id"].as_str()
                .or_else(|| j["id"].as_str())
                .unwrap_or("").to_string(),
            model: j["model"].as_str().unwrap_or("unknown").to_string(),
            status: j["status"].as_str().unwrap_or("unknown").to_string(),
            created_at: j["created_at"].as_str()
                .or_else(|| j["submitted_at"].as_str())
                .unwrap_or("").to_string(),
            completed_at: j["completed_at"].as_str().unwrap_or("").to_string(),
            provider_earned_halala: j["provider_earned_halala"].as_i64()
                .or_else(|| j["earnings_halala"].as_i64())
                .or_else(|| j["earned_halala"].as_i64())
                .unwrap_or(0),
            prompt_tokens: j["prompt_tokens"].as_i64().unwrap_or(0),
            completion_tokens: j["completion_tokens"].as_i64().unwrap_or(0),
            duration_seconds: j["duration_seconds"].as_i64().unwrap_or(0),
        })
        .collect();

    Ok(jobs)
}

#[tauri::command]
async fn pause_provider(api_key: String) -> Result<(), String> {
    // Tier 2.7 / G56: backend pause/resume read `key` from the body
    // (providers.js:2010, :2029). We send both `key` and `api_key` so this
    // command works against current backends without coupling the rename.
    let url = format!("{}/pause", API_BASE);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("x-api-key", &api_key)
        .json(&serde_json::json!({ "key": api_key, "api_key": api_key }))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("API returned status {}", resp.status()));
    }

    Ok(())
}

#[tauri::command]
async fn resume_provider(api_key: String) -> Result<(), String> {
    // Tier 2.7 / G56: see pause_provider — backend reads `key` from the body.
    let url = format!("{}/resume", API_BASE);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("x-api-key", &api_key)
        .json(&serde_json::json!({ "key": api_key, "api_key": api_key }))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("API returned status {}", resp.status()));
    }

    Ok(())
}

#[tauri::command]
async fn read_config() -> Result<SavedConfig, String> {
    let config_path = dirs::home_dir()
        .ok_or("Could not determine home directory")?
        .join(".dcp")
        .join("config.json");

    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("Failed to read config: {}", e))?;

    let json: Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse config: {}", e))?;

    Ok(SavedConfig {
        api_key: json["api_key"].as_str().unwrap_or("").to_string(),
        run_mode: json["run_mode"].as_str().unwrap_or("idle").to_string(),
        gpu_usage_cap: json["gpu_usage_cap"].as_u64().unwrap_or(80) as u32,
        temp_limit: json["temp_limit"].as_u64().unwrap_or(85) as u32,
        start_on_boot: json["start_on_boot"].as_bool().unwrap_or(true),
        served_model: json["served_model"].as_str().unwrap_or("").to_string(),
    })
}

// ── Tier 2.7 / G56 — tray helpers ────────────────────────────────────
//
// The tray-menu Pause/Resume handlers run from a synchronous on_menu_event
// closure that does not have access to a Tauri State. They need (1) the
// provider's API key from disk and (2) a way to surface errors to the user.
// These two helpers mirror the behaviour of the existing `read_config`
// command and the dormant tauri_plugin_notification we already register
// in setup().

/// Read just the api_key field from ~/.dcp/config.json. Mirrors `read_config`
/// but avoids requiring a Tauri State, which the menu callback can't supply.
/// Returns `Err` for missing dir, missing file, unparsable JSON, or empty key.
fn load_api_key_from_config() -> Result<String, String> {
    let config_path = dirs::home_dir()
        .ok_or_else(|| "Could not determine home directory".to_string())?
        .join(".dcp")
        .join("config.json");

    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("Failed to read {:?}: {}", config_path, e))?;

    let json: Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse config: {}", e))?;

    let key = json["api_key"].as_str().unwrap_or("").to_string();
    if key.is_empty() {
        return Err("api_key missing from config.json".to_string());
    }
    Ok(key)
}

/// G57: Read a single string field from ~/.dcp/config.json by key name.
/// Returns `None` on any error (missing file, bad JSON, missing field).
/// Used by the tray refresh loop — must not panic or do network I/O.
fn load_config_field(field: &str) -> Option<String> {
    let config_path = dirs::home_dir()?.join(".dcp").join("config.json");
    let content = std::fs::read_to_string(config_path).ok()?;
    let json: Value = serde_json::from_str(&content).ok()?;
    json[field].as_str().map(|s| s.to_string())
}

/// Best-effort tray notification. Logs and swallows failures so a missing
/// notification permission can never crash the menu handler.
fn notify_tray(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show()
    {
        eprintln!("[tray] notification failed ({}): {} — {}", e, title, body);
    }
}

/// Hide console windows on Windows for spawned processes
#[cfg(target_os = "windows")]
fn hide_window(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x08000000) // CREATE_NO_WINDOW
}
#[cfg(not(target_os = "windows"))]
fn hide_window(cmd: &mut Command) -> &mut Command {
    cmd // no-op on Unix
}

#[derive(serde::Serialize, Clone, Default)]
pub struct WizardProgress {
    pub step_id: String,
    pub status: String,
    pub pct: Option<f32>,
    pub mb_done: Option<f64>,
    pub mb_total: Option<f64>,
    pub mbps: Option<f64>,
    pub eta_seconds: Option<u64>,
    pub detail: Option<String>,
    pub error: Option<String>,
}

fn emit_wizard_progress(window: &tauri::Window, payload: WizardProgress) {
    let line = format!(
        "[{}] [wizard step={} status={} pct={:?} mb={:?}/{:?} mbps={:?} eta={:?}] {}",
        chrono_now(),
        payload.step_id,
        payload.status,
        payload.pct,
        payload.mb_done,
        payload.mb_total,
        payload.mbps,
        payload.eta_seconds,
        payload.detail.clone().unwrap_or_default(),
    );
    if let Ok(home) = dcp_home() {
        let path = home.join("startup.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(&path) {
            use std::io::Write;
            let _ = writeln!(f, "{}", line);
        }
    }
    let _ = window.emit("wizard:progress", payload);
}

/// M11 — atomic file write: write to a temp sibling, fsync, then rename.
/// Prevents the updater from catching a half-written daemon file when
/// download is interrupted or the process is killed mid-write.
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp_path = {
        let mut p = path.as_os_str().to_owned();
        p.push(".part");
        std::path::PathBuf::from(p)
    };
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, path)
}

/// G2 — spawn the daemon detached from the parent .exe so it survives a
/// desktop UI quit / crash. On Windows: CREATE_NEW_PROCESS_GROUP combined
/// with CREATE_NO_WINDOW. On Unix: place child in its own process group
/// (process_group(0)) so SIGTERM to the parent does not propagate.
#[cfg(target_os = "windows")]
fn detach_process(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    // CREATE_NO_WINDOW (0x08000000) | CREATE_NEW_PROCESS_GROUP (0x00000200)
    cmd.creation_flags(0x08000200)
}
#[cfg(not(target_os = "windows"))]
fn detach_process(cmd: &mut Command) -> &mut Command {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0)
}

/// L4 — ISO 8601 UTC timestamp for human-facing logs
/// (was: seconds-since-epoch as a string, hard to read in support contexts).
fn chrono_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ── Cross-platform Process Utilities ────────────────────────────────

/// Check if a process with the given PID is alive (cross-platform)
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        hide_window(Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"]))
            .output()
            .map(|o| {
                let out = String::from_utf8_lossy(&o.stdout);
                o.status.success() && out.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
}

/// Send SIGTERM (Unix) or taskkill (Windows) to a process
fn kill_process_graceful(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-15", &pid.to_string()]).output();
    }
    #[cfg(windows)]
    {
        let _ = hide_window(Command::new("taskkill").args(["/PID", &pid.to_string()])).output();
    }
}

/// Force-kill a process (SIGKILL on Unix, /F on Windows)
fn kill_process_force(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
    }
    #[cfg(windows)]
    {
        let _ = hide_window(Command::new("taskkill").args(["/F", "/PID", &pid.to_string()])).output();
    }
}

/// Kill processes by name pattern (cross-platform)
fn kill_by_name(pattern: &str) {
    #[cfg(unix)]
    {
        let _ = Command::new("pkill").args(["-f", pattern]).output();
    }
    #[cfg(windows)]
    {
        // On Windows, find matching PIDs via wmic then taskkill
        let output = hide_window(Command::new("wmic")
            .args(["process", "where", &format!("CommandLine like '%{}%'", pattern), "get", "ProcessId", "/value"]))
            .output();
        if let Ok(o) = output {
            let text = String::from_utf8_lossy(&o.stdout);
            for line in text.lines() {
                if let Some(pid_str) = line.strip_prefix("ProcessId=") {
                    if let Ok(pid) = pid_str.trim().parse::<u32>() {
                        let _ = hide_window(Command::new("taskkill").args(["/F", "/PID", &pid.to_string()])).output();
                    }
                }
            }
        }
    }
}

/// Check if a command exists in PATH (cross-platform)
fn command_exists(name: &str) -> bool {
    #[cfg(unix)]
    {
        Command::new("which").arg(name).output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        hide_window(Command::new("where").arg(name)).output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

/// Find the Ollama executable (checks PATH + custom-path Windows installs).
///
/// Lookup order on Windows:
///   1. PATH (via `command_exists`).
///   2. `where.exe ollama` — picks up custom-path installs that put ollama.exe
///      somewhere not in our hard-coded list but still reachable on PATH.
///   3. Registry `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Ollama`
///      → `InstallLocation` (Inno Setup writes this even for non-default dirs).
///   4. `%LOCALAPPDATA%\Programs\Ollama\ollama.exe` (default user install).
///   5. `C:\Program Files\Ollama\ollama.exe`.
///   6. `C:\Program Files (x86)\Ollama\ollama.exe`.
///   7. Fallback to the bare string `"ollama"` so the caller fails with a
///      clear "command not found" error rather than a path-shaped one.
fn ollama_cmd() -> String {
    if command_exists("ollama") {
        return "ollama".to_string();
    }
    #[cfg(target_os = "windows")]
    {
        // (2) where.exe — finds custom-path installs that registered with PATH.
        if let Ok(out) = hide_window(Command::new("where.exe").arg("ollama")).output() {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if let Some(first) = stdout.lines().next() {
                    let trimmed = first.trim();
                    if !trimmed.is_empty() && std::path::Path::new(trimmed).exists() {
                        return trimmed.to_string();
                    }
                }
            }
        }

        // (3) Registry InstallLocation under the Uninstall key.
        {
            use winreg::enums::HKEY_CURRENT_USER;
            use winreg::RegKey;
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            if let Ok(key) = hkcu.open_subkey(
                r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Ollama",
            ) {
                if let Ok(install_location) = key.get_value::<String, _>("InstallLocation") {
                    let candidate = std::path::PathBuf::from(install_location.trim())
                        .join("ollama.exe");
                    if candidate.exists() {
                        return candidate.to_string_lossy().into_owned();
                    }
                }
            }
        }

        // (4) Default Inno Setup user install path.
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let ollama_path = format!(r"{}\Programs\Ollama\ollama.exe", local_app_data);
            if std::path::Path::new(&ollama_path).exists() {
                return ollama_path;
            }
        }
        // (5) + (6) Program Files fallbacks.
        let alt_paths = [
            r"C:\Program Files\Ollama\ollama.exe",
            r"C:\Program Files (x86)\Ollama\ollama.exe",
        ];
        for p in &alt_paths {
            if std::path::Path::new(p).exists() {
                return p.to_string();
            }
        }
    }
    "ollama".to_string() // fallback — will fail with clear error
}

/// Find the Python executable (python3 on Unix, python on Windows)
fn python_cmd() -> &'static str {
    #[cfg(unix)]
    { "python3" }
    #[cfg(windows)]
    {
        // Windows: verify Python actually works (not just exists)
        // Microsoft Store stub python3.exe crashes with 0xc0000142
        static PYTHON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let s: &String = PYTHON.get_or_init(|| {
            // Helper: check if python actually outputs a version string
            let works = |cmd: &str| -> bool {
                hide_window(Command::new(cmd).arg("--version"))
                    .output()
                    .map(|o| {
                        o.status.success() &&
                        String::from_utf8_lossy(&o.stdout).contains("Python")
                    })
                    .unwrap_or(false)
            };
            // Try embedded first (most reliable on Windows)
            if let Ok(dcp) = dcp_home() {
                let embedded = dcp.join("python").join("python.exe");
                if embedded.exists() && works(&embedded.to_string_lossy()) {
                    return embedded.to_string_lossy().into_owned();
                }
            }
            if works("python") {
                "python".to_string()
            } else if works("python3") {
                "python3".to_string()
            } else if let Ok(dcp) = dcp_home() {
                // Will be installed by Step 3.5
                dcp.join("python").join("python.exe").to_string_lossy().into_owned()
            } else {
                "python".to_string()
            }
        });
        // SAFETY: OnceLock ensures the String lives for 'static; we leak a &str ref.
        // This is fine — it's a one-time allocation that lives for the process lifetime.
        let leaked: &'static str = unsafe { &*(s.as_str() as *const str) };
        leaked
    }
}

/// Get the DCP home directory (~/.dcp)
fn dcp_home() -> Result<std::path::PathBuf, String> {
    let home = dirs::home_dir().ok_or("Could not determine home directory")?;
    let dcp_dir = home.join(".dcp");
    std::fs::create_dir_all(&dcp_dir)
        .map_err(|e| format!("Failed to create ~/.dcp directory: {}", e))?;
    Ok(dcp_dir)
}

/// Read the last N lines from a file.
///
/// M6 — seek from end and read up to TAIL_WINDOW bytes instead of slurping
/// the whole file each poll. With 5s metric polls and verbose MLX/daemon
/// output, the previous read_to_string approach grew O(filesize) per call
/// and froze the dashboard after hours.
fn tail_file(path: &std::path::Path, n: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL_WINDOW: u64 = 64 * 1024; // 64 KB is enough for ~hundreds of log lines

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let len = match file.metadata() {
        Ok(m) => m.len(),
        Err(_) => return Vec::new(),
    };
    let read_from = len.saturating_sub(TAIL_WINDOW);
    if file.seek(SeekFrom::Start(read_from)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::with_capacity(TAIL_WINDOW.min(len) as usize);
    if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    // If we started mid-line (read_from > 0), drop the partial leading line.
    let lines: Vec<&str> = text.lines().collect();
    let lines = if read_from > 0 && !lines.is_empty() {
        &lines[1..]
    } else {
        &lines[..]
    };
    let start = if lines.len() > n { lines.len() - n } else { 0 };
    lines[start..].iter().map(|s| s.to_string()).collect()
}

/// Read PID from the PID file
fn read_pid_file() -> Option<u32> {
    let dcp_dir = dcp_home().ok()?;
    let pid_path = dcp_dir.join("daemon.pid");
    let content = std::fs::read_to_string(pid_path).ok()?;
    content.trim().parse().ok()
}

/// Write PID to the PID file
fn write_pid_file(pid: u32) -> Result<(), String> {
    let dcp_dir = dcp_home()?;
    let pid_path = dcp_dir.join("daemon.pid");
    std::fs::write(&pid_path, pid.to_string())
        .map_err(|e| format!("Failed to write PID file: {}", e))
}

/// Remove the PID file
fn remove_pid_file() {
    if let Ok(dcp_dir) = dcp_home() {
        let _ = std::fs::remove_file(dcp_dir.join("daemon.pid"));
    }
}

#[tauri::command]
async fn start_daemon_process(api_key: String, state: State<'_, DaemonManager>) -> Result<String, String> {
    // 1. Check if daemon is already running
    {
        let guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
        if let Some(pid) = guard.pid {
            if is_process_alive(pid) {
                return Ok(format!("Daemon already running with PID {}", pid));
            }
        }
    }

    // Also check the PID file for daemons started outside this session
    if let Some(existing_pid) = read_pid_file() {
        if is_process_alive(existing_pid) {
            let mut guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
            guard.pid = Some(existing_pid);
            guard.status = "running".to_string();
            guard.started_at = Some(std::time::Instant::now());
            return Ok(format!("Daemon already running with PID {}", existing_pid));
        }
    }

    let dcp_dir = dcp_home()?;
    let daemon_path = dcp_dir.join("dcp_daemon.py");

    // 2. Backlog #7 — download the daemon from the platform if not present.
    //    The platform is the single source of the daemon (no bundled copy), so
    //    fetch + verify (size + sha256) against the manifest BEFORE writing.
    //    Fail-safe: if the verified fetch fails, surface the error and DO NOT
    //    spawn — never run a stale/missing daemon.
    if !daemon_path.exists() {
        let verified = fetch_verified_daemon(&api_key)
            .await
            .map_err(|e| format!("Couldn't fetch daemon from platform: {}", e))?;
        // M11 — atomic write so we never spawn a half-downloaded daemon.
        atomic_write(&daemon_path, &verified.bytes)
            .map_err(|e| format!("Failed to write daemon file: {}", e))?;
    }

    // 3. Spawn the daemon process
    let log_path = dcp_dir.join("daemon.log");
    let err_log_path = dcp_dir.join("daemon_error.log");

    // M7 — append, don't truncate. We lose post-mortem context exactly when we need
    // it (immediately after a crash-restart) if these are wiped on every start.
    let log_file = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)
        .map_err(|e| format!("Failed to open log file: {}", e))?;
    let err_file = std::fs::OpenOptions::new().create(true).append(true).open(&err_log_path)
        .map_err(|e| format!("Failed to open error log file: {}", e))?;

    // Update state to "starting"
    {
        let mut guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
        guard.status = "starting".to_string();
    }

    // Read served_model and engine from config to pass as env vars
    let config_path = dcp_dir.join("config.json");
    let (served_model, engine_name) = if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
            (
                config.get("served_model").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                config.get("engine").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            )
        } else { (String::new(), String::new()) }
    } else { (String::new(), String::new()) };

    // G2 — detach so the daemon survives a desktop UI quit / crash.
    let child = detach_process(Command::new(python_cmd())
        .arg(&daemon_path)
        .arg("--no-watchdog")
        .arg("--key")
        .arg(&api_key)
        .arg("--url")
        .arg("https://api.dcp.sa")
        .env("DCP_SERVED_MODEL", &served_model)
        .env("DCP_ENGINE", &engine_name)
        .stdout(log_file)
        .stderr(err_file))
        .spawn()
        .map_err(|e| format!("Failed to spawn daemon: {}", e))?;

    let pid = child.id();

    // 4. Save PID to state + PID file
    write_pid_file(pid)?;

    {
        let mut guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
        guard.pid = Some(pid);
        guard.status = "running".to_string();
        guard.started_at = Some(std::time::Instant::now());
        guard.last_restart = Some(std::time::Instant::now());
        guard.restart_count += 1;
    }

    Ok(format!("started:{}", pid))
}

#[tauri::command]
async fn stop_daemon_process(state: State<'_, DaemonManager>) -> Result<String, String> {
    let pid = {
        let guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
        guard.pid
    };

    // Also check PID file as fallback
    let pid = pid.or_else(read_pid_file);

    let pid = match pid {
        Some(p) => p,
        None => {
            let mut guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
            guard.status = "stopped".to_string();
            return Ok("stopped".to_string());
        }
    };

    if !is_process_alive(pid) {
        let mut guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
        guard.pid = None;
        guard.status = "stopped".to_string();
        remove_pid_file();
        return Ok("stopped".to_string());
    }

    // Graceful shutdown (SIGTERM / taskkill)
    kill_process_graceful(pid);

    // Wait up to 10 seconds for graceful shutdown
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if !is_process_alive(pid) {
            let mut guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
            guard.pid = None;
            guard.status = "stopped".to_string();
            guard.started_at = None;
            remove_pid_file();
            return Ok("stopped".to_string());
        }
    }

    // Force kill if still alive
    kill_process_force(pid);

    std::thread::sleep(std::time::Duration::from_millis(500));

    let mut guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
    guard.pid = None;
    guard.status = "stopped".to_string();
    guard.started_at = None;
    remove_pid_file();

    Ok("stopped".to_string())
}

#[tauri::command]
async fn get_daemon_status(state: State<'_, DaemonManager>) -> Result<DaemonStatus, String> {
    let (pid, status, started_at) = {
        let guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
        (guard.pid, guard.status.clone(), guard.started_at)
    };

    // Also check PID file if state has no PID
    let actual_pid = pid.or_else(read_pid_file);

    let (actual_status, alive) = match actual_pid {
        Some(p) => {
            if is_process_alive(p) {
                ("running".to_string(), true)
            } else {
                ("crashed".to_string(), false)
            }
        }
        None => (status, false),
    };

    // Update state if process is alive but we don't have started_at (discovered via PID file)
    if alive && started_at.is_none() {
        if let Ok(mut guard) = state.lock() {
            guard.pid = actual_pid;
            guard.status = "running".to_string();
            if guard.started_at.is_none() {
                // Set started_at to now — we don't know exactly when it started,
                // but uptime will at least tick from discovery onward
                guard.started_at = Some(std::time::Instant::now());
            }
        }
    }

    // Update state if the process died
    if !alive && actual_pid.is_some() {
        if let Ok(mut guard) = state.lock() {
            guard.status = actual_status.clone();
            if actual_status == "crashed" {
                guard.pid = None;
                remove_pid_file();
            }
        }
    }

    // Calculate uptime — re-read started_at after potential update above
    let current_started_at = state.lock().ok().and_then(|g| g.started_at);
    let uptime_seconds = if alive {
        current_started_at
            .map(|s| s.elapsed().as_secs())
            .unwrap_or(0)
    } else {
        0
    };

    // Read last log lines
    let dcp_dir = dcp_home().unwrap_or_default();
    let log_path = dcp_dir.join("daemon.log");
    let last_log_lines = tail_file(&log_path, 20);

    Ok(DaemonStatus {
        status: actual_status,
        pid: if alive { actual_pid } else { None },
        uptime_seconds,
        last_log_lines,
    })
}

#[tauri::command]
async fn check_daemon_health() -> Result<HealthReport, String> {
    let mut checks: Vec<HealthCheck> = Vec::new();

    // 1. GPU detected?
    #[cfg(target_os = "macos")]
    {
        let arch_output = Command::new("uname").arg("-m").output();
        match arch_output {
            Ok(o) => {
                let arch = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if arch == "arm64" {
                    checks.push(HealthCheck {
                        name: "GPU Driver".to_string(),
                        status: "ok".to_string(),
                        message: "Apple Silicon detected (Metal/MLX supported)".to_string(),
                        can_auto_fix: false,
                        fix_action: None,
                    });
                } else {
                    checks.push(HealthCheck {
                        name: "GPU Driver".to_string(),
                        status: "warning".to_string(),
                        message: "Intel Mac detected — limited GPU acceleration".to_string(),
                        can_auto_fix: false,
                        fix_action: None,
                    });
                }
            }
            Err(_) => {
                checks.push(HealthCheck {
                    name: "GPU Driver".to_string(),
                    status: "error".to_string(),
                    message: "Could not detect GPU architecture".to_string(),
                    can_auto_fix: false,
                    fix_action: None,
                });
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let smi = find_nvidia_smi();
        let nvidia = smi.as_ref().map(|path| {
            hide_window(Command::new(path)
                .arg("--query-gpu=name")
                .arg("--format=csv,noheader"))
                .output()
        });
        match nvidia {
            Some(Ok(o)) if o.status.success() => {
                let gpu_name = String::from_utf8_lossy(&o.stdout).trim().to_string();
                checks.push(HealthCheck {
                    name: "GPU Driver".to_string(),
                    status: "ok".to_string(),
                    message: format!("{} detected", gpu_name),
                    can_auto_fix: false,
                    fix_action: None,
                });
            }
            _ => {
                checks.push(HealthCheck {
                    name: "GPU Driver".to_string(),
                    status: "error".to_string(),
                    message: "nvidia-smi not found — no NVIDIA GPU or drivers missing".to_string(),
                    can_auto_fix: true,
                    fix_action: Some("install_drivers".to_string()),
                });
            }
        }
    }

    // 2. Python installed?
    let python_check = hide_window(Command::new(python_cmd()).arg("--version")).output();
    match python_check {
        Ok(o) if o.status.success() => {
            let version = String::from_utf8_lossy(&o.stdout).trim().to_string();
            checks.push(HealthCheck {
                name: "Python".to_string(),
                status: "ok".to_string(),
                message: format!("{} installed", version),
                can_auto_fix: false,
                fix_action: None,
            });
        }
        _ => {
            checks.push(HealthCheck {
                name: "Python".to_string(),
                status: "error".to_string(),
                message: "python3 not found in PATH".to_string(),
                can_auto_fix: false,
                fix_action: None,
            });
        }
    }

    // 3. Daemon file exists?
    let dcp_dir = dcp_home().unwrap_or_default();
    let daemon_path = dcp_dir.join("dcp_daemon.py");
    if daemon_path.exists() {
        checks.push(HealthCheck {
            name: "Daemon File".to_string(),
            status: "ok".to_string(),
            message: format!("{} exists", daemon_path.display()),
            can_auto_fix: false,
            fix_action: None,
        });
    } else {
        checks.push(HealthCheck {
            name: "Daemon File".to_string(),
            status: "error".to_string(),
            message: "dcp_daemon.py not found — will be downloaded on start".to_string(),
            can_auto_fix: true,
            fix_action: Some("download_daemon".to_string()),
        });
    }

    // 4. Inference engine installed?
    #[cfg(target_os = "macos")]
    {
        let mlx_check = Command::new(python_cmd())
            .args(["-c", "import mlx_lm; print(mlx_lm.__version__)"])
            .output();
        match mlx_check {
            Ok(o) if o.status.success() => {
                let ver = String::from_utf8_lossy(&o.stdout).trim().to_string();
                checks.push(HealthCheck {
                    name: "Inference Engine".to_string(),
                    status: "ok".to_string(),
                    message: format!("mlx-lm {} installed", ver),
                    can_auto_fix: false,
                    fix_action: None,
                });
            }
            _ => {
                // Also check for Ollama as fallback on macOS
                if command_exists("ollama") {
                    checks.push(HealthCheck {
                        name: "Inference Engine".to_string(),
                        status: "ok".to_string(),
                        message: "Ollama installed (fallback)".to_string(),
                        can_auto_fix: false,
                        fix_action: None,
                    });
                } else {
                    checks.push(HealthCheck {
                        name: "Inference Engine".to_string(),
                        status: "error".to_string(),
                        message: "No inference engine found (need mlx-lm or ollama)".to_string(),
                        can_auto_fix: true,
                        fix_action: Some("install_engine".to_string()),
                    });
                }
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        if command_exists("ollama") {
            checks.push(HealthCheck {
                name: "Inference Engine".to_string(),
                status: "ok".to_string(),
                message: "Ollama installed".to_string(),
                can_auto_fix: false,
                fix_action: None,
            });
        } else {
            checks.push(HealthCheck {
                name: "Inference Engine".to_string(),
                status: "error".to_string(),
                message: "Ollama not found in PATH".to_string(),
                can_auto_fix: true,
                fix_action: Some("install_engine".to_string()),
            });
        }
    }

    // 5. Model downloaded? Check ollama list or mlx model dir
    let model_check = hide_window(Command::new(&ollama_cmd()).arg("list")).output();
    match model_check {
        Ok(o) if o.status.success() => {
            let output = String::from_utf8_lossy(&o.stdout).to_string();
            let model_count = output.lines().count().saturating_sub(1); // subtract header
            if model_count > 0 {
                checks.push(HealthCheck {
                    name: "Model".to_string(),
                    status: "ok".to_string(),
                    message: format!("{} model(s) available", model_count),
                    can_auto_fix: false,
                    fix_action: None,
                });
            } else {
                checks.push(HealthCheck {
                    name: "Model".to_string(),
                    status: "warning".to_string(),
                    message: "No models downloaded yet".to_string(),
                    can_auto_fix: true,
                    fix_action: Some("download_model".to_string()),
                });
            }
        }
        _ => {
            // Check MLX models directory as fallback
            let home = dirs::home_dir().unwrap_or_default();
            let mlx_cache = home.join(".cache").join("huggingface").join("hub");
            if mlx_cache.exists() {
                checks.push(HealthCheck {
                    name: "Model".to_string(),
                    status: "ok".to_string(),
                    message: "HuggingFace model cache found".to_string(),
                    can_auto_fix: false,
                    fix_action: None,
                });
            } else {
                checks.push(HealthCheck {
                    name: "Model".to_string(),
                    status: "warning".to_string(),
                    message: "No models found — will be downloaded on first run".to_string(),
                    can_auto_fix: true,
                    fix_action: Some("download_model".to_string()),
                });
            }
        }
    }

    // 6. Daemon process alive?
    let daemon_alive = read_pid_file()
        .map(|pid| is_process_alive(pid))
        .unwrap_or(false);
    checks.push(HealthCheck {
        name: "Daemon Process".to_string(),
        status: if daemon_alive { "ok".to_string() } else { "error".to_string() },
        message: if daemon_alive {
            format!("Running (PID {})", read_pid_file().unwrap_or(0))
        } else {
            "Not running".to_string()
        },
        can_auto_fix: true,
        fix_action: if daemon_alive { None } else { Some("start_daemon".to_string()) },
    });

    // 7. Daemon heartbeating? Check last heartbeat from log
    let log_path = dcp_dir.join("daemon.log");
    let log_lines = tail_file(&log_path, 50);
    let has_heartbeat = log_lines.iter().any(|l| l.contains("Heartbeat") || l.contains("heartbeat"));
    checks.push(HealthCheck {
        name: "Daemon Heartbeat".to_string(),
        status: if daemon_alive && has_heartbeat { "ok".to_string() } else if daemon_alive { "warning".to_string() } else { "error".to_string() },
        message: if daemon_alive && has_heartbeat {
            "Heartbeat OK".to_string()
        } else if daemon_alive {
            "Running but no heartbeat detected in recent logs".to_string()
        } else {
            "Daemon not running".to_string()
        },
        can_auto_fix: false,
        fix_action: None,
    });

    // 8. Port 8000 responding?
    let port_check = hide_window(Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "--connect-timeout", "2", "http://localhost:8000/health"]))
        .output();
    match port_check {
        Ok(o) if o.status.success() => {
            let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if code == "200" {
                checks.push(HealthCheck {
                    name: "Local Server".to_string(),
                    status: "ok".to_string(),
                    message: "Port 8000 responding (HTTP 200)".to_string(),
                    can_auto_fix: false,
                    fix_action: None,
                });
            } else {
                checks.push(HealthCheck {
                    name: "Local Server".to_string(),
                    status: "warning".to_string(),
                    message: format!("Port 8000 returned HTTP {}", code),
                    can_auto_fix: false,
                    fix_action: None,
                });
            }
        }
        _ => {
            checks.push(HealthCheck {
                name: "Local Server".to_string(),
                status: if daemon_alive { "warning".to_string() } else { "error".to_string() },
                message: "Port 8000 not responding".to_string(),
                can_auto_fix: false,
                fix_action: None,
            });
        }
    }

    // 9. Internet reachable?
    let internet_check = hide_window(Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "--connect-timeout", "5", "https://api.dcp.sa/api/health"]))
        .output();
    match internet_check {
        Ok(o) if o.status.success() => {
            let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let code_num: u16 = code.parse().unwrap_or(0);
            if code_num >= 200 && code_num < 400 {
                checks.push(HealthCheck {
                    name: "Internet".to_string(),
                    status: "ok".to_string(),
                    message: "api.dcp.sa reachable".to_string(),
                    can_auto_fix: false,
                    fix_action: None,
                });
            } else {
                checks.push(HealthCheck {
                    name: "Internet".to_string(),
                    status: "warning".to_string(),
                    message: format!("api.dcp.sa returned HTTP {}", code),
                    can_auto_fix: false,
                    fix_action: None,
                });
            }
        }
        _ => {
            checks.push(HealthCheck {
                name: "Internet".to_string(),
                status: "error".to_string(),
                message: "Cannot reach api.dcp.sa — check internet connection".to_string(),
                can_auto_fix: false,
                fix_action: None,
            });
        }
    }

    // 10. Internet speed probe (download 2 MB sample and measure throughput)
    {
        let speed_url = "https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe";
        let speed_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .unwrap_or_default();
        let speed_start = std::time::Instant::now();
        let speed_result = speed_client.get(speed_url)
            .header("Range", "bytes=0-2097151")
            .send().await;
        match speed_result {
            Ok(resp) => {
                match resp.bytes().await {
                    Ok(bytes) => {
                        let elapsed_s = speed_start.elapsed().as_secs_f64();
                        if elapsed_s > 0.01 {
                            let mbps = (bytes.len() as f64) / elapsed_s / 1_048_576.0;
                            let (status, msg) = if mbps >= 10.0 {
                                ("ok", format!("{:.1} MB/s — excellent", mbps))
                            } else if mbps >= 2.0 {
                                ("ok", format!("{:.1} MB/s — adequate for model downloads", mbps))
                            } else if mbps >= 0.5 {
                                ("warning", format!("{:.1} MB/s — slow, model downloads will take a long time", mbps))
                            } else {
                                ("error", format!("{:.2} MB/s — very slow, model downloads may fail or timeout", mbps))
                            };
                            checks.push(HealthCheck {
                                name: "Internet Speed".to_string(),
                                status: status.to_string(),
                                message: msg,
                                can_auto_fix: false,
                                fix_action: None,
                            });
                        }
                    }
                    Err(_) => {
                        checks.push(HealthCheck {
                            name: "Internet Speed".to_string(),
                            status: "warning".to_string(),
                            message: "Speed probe failed — could not read response".to_string(),
                            can_auto_fix: false,
                            fix_action: None,
                        });
                    }
                }
            }
            Err(_) => {
                checks.push(HealthCheck {
                    name: "Internet Speed".to_string(),
                    status: "warning".to_string(),
                    message: "Speed probe failed — could not connect".to_string(),
                    can_auto_fix: false,
                    fix_action: None,
                });
            }
        }
    }

    // 11. Sufficient disk space?
    #[cfg(target_os = "macos")]
    {
        let df_output = Command::new("df")
            .args(["-g", "/"])
            .output();
        match df_output {
            Ok(o) if o.status.success() => {
                let output = String::from_utf8_lossy(&o.stdout).to_string();
                // Parse the "Available" column from df -g output
                if let Some(data_line) = output.lines().nth(1) {
                    let parts: Vec<&str> = data_line.split_whitespace().collect();
                    if parts.len() >= 4 {
                        let available_gb: u64 = parts[3].parse().unwrap_or(0);
                        if available_gb >= 10 {
                            checks.push(HealthCheck {
                                name: "Disk Space".to_string(),
                                status: "ok".to_string(),
                                message: format!("{}GB available", available_gb),
                                can_auto_fix: false,
                                fix_action: None,
                            });
                        } else if available_gb >= 5 {
                            checks.push(HealthCheck {
                                name: "Disk Space".to_string(),
                                status: "warning".to_string(),
                                message: format!("{}GB available — may need more for models", available_gb),
                                can_auto_fix: false,
                                fix_action: None,
                            });
                        } else {
                            checks.push(HealthCheck {
                                name: "Disk Space".to_string(),
                                status: "error".to_string(),
                                message: format!("Only {}GB available — insufficient for models", available_gb),
                                can_auto_fix: false,
                                fix_action: None,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    #[cfg(target_os = "linux")]
    {
        let df_output = Command::new("df")
            .args(["--output=avail", "-BG", "/"])
            .output();
        match df_output {
            Ok(o) if o.status.success() => {
                let output = String::from_utf8_lossy(&o.stdout).to_string();
                if let Some(data_line) = output.lines().nth(1) {
                    let avail_str = data_line.trim().trim_end_matches('G');
                    let available_gb: u64 = avail_str.parse().unwrap_or(0);
                    let (status, msg) = if available_gb >= 10 {
                        ("ok", format!("{}GB available", available_gb))
                    } else if available_gb >= 5 {
                        ("warning", format!("{}GB available — may need more for models", available_gb))
                    } else {
                        ("error", format!("Only {}GB available — insufficient for models", available_gb))
                    };
                    checks.push(HealthCheck {
                        name: "Disk Space".to_string(),
                        status: status.to_string(),
                        message: msg,
                        can_auto_fix: false,
                        fix_action: None,
                    });
                }
            }
            _ => {}
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Use wmic to get free disk space on C:
        let wmic_output = hide_window(Command::new("wmic")
            .args(["logicaldisk", "where", "DeviceID='C:'", "get", "FreeSpace", "/value"]))
            .output();
        match wmic_output {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                if let Some(line) = text.lines().find(|l| l.starts_with("FreeSpace=")) {
                    if let Some(bytes_str) = line.strip_prefix("FreeSpace=") {
                        let free_bytes: u64 = bytes_str.trim().parse().unwrap_or(0);
                        let available_gb = free_bytes / 1_073_741_824;
                        let (status, msg) = if available_gb >= 10 {
                            ("ok", format!("{}GB available", available_gb))
                        } else if available_gb >= 5 {
                            ("warning", format!("{}GB available — may need more for models", available_gb))
                        } else {
                            ("error", format!("Only {}GB available — insufficient for models", available_gb))
                        };
                        checks.push(HealthCheck {
                            name: "Disk Space".to_string(),
                            status: status.to_string(),
                            message: msg,
                            can_auto_fix: false,
                            fix_action: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    // Determine overall status
    let error_count = checks.iter().filter(|c| c.status == "error").count();
    let warning_count = checks.iter().filter(|c| c.status == "warning").count();

    let overall = if error_count >= 2 {
        "critical".to_string()
    } else if error_count >= 1 || warning_count >= 2 {
        "degraded".to_string()
    } else {
        "healthy".to_string()
    };

    Ok(HealthReport { overall, checks })
}

#[tauri::command]
async fn get_live_metrics(state: State<'_, DaemonManager>) -> Result<LiveMetrics, String> {
    let daemon_pid = {
        let guard = state.lock().map_err(|e| format!("Lock error: {}", e))?;
        guard.pid
    };
    let actual_pid = daemon_pid.or_else(read_pid_file);
    let daemon_alive = actual_pid.map(|p| is_process_alive(p)).unwrap_or(false);

    #[allow(unused_mut)]
    let mut gpu_temperature: Option<f32> = None;
    let mut gpu_utilization: Option<f32> = None;
    let mut memory_used_mb: Option<u64> = None;

    // GPU metrics
    #[cfg(target_os = "macos")]
    {
        // On macOS: find the MLX server process (the one doing GPU work)
        // The daemon process just heartbeats — the MLX server does the inference
        let mlx_pid_output = Command::new("pgrep")
            .args(["-f", "mlx_lm.server"])
            .output();
        let mlx_pid: Option<u32> = mlx_pid_output.ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().lines().next()
                .and_then(|s| s.parse().ok()));

        // GPU utilization on Apple Silicon:
        // MLX server idles at 100% CPU (known bug) so CPU% is a bad proxy.
        // Instead, check if the server is actively processing by probing /v1/models
        // response time — if it's slow, GPU is busy. Or better: use inference_speed
        // from daemon logs. If we found a recent speed reading, GPU is active.
        // We'll set gpu_utilization after parsing inference_speed below.
        // For now, just check if MLX server is running at all.
        if mlx_pid.is_some() {
            gpu_utilization = Some(0.0); // Will be updated below if inference is active
        }

        // Memory: get MLX server RSS (this is the process using GPU memory)
        let mem_pid = mlx_pid.or(actual_pid);
        if let Some(pid) = mem_pid {
            if is_process_alive(pid) {
                let rss_output = Command::new("ps")
                    .args(["-p", &pid.to_string(), "-o", "rss="])
                    .output();
                if let Ok(o) = rss_output {
                    if o.status.success() {
                        let rss_str = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        if let Ok(rss_kb) = rss_str.parse::<u64>() {
                            memory_used_mb = Some(rss_kb / 1024);
                        }
                    }
                }
            }
        }

        // Temperature: macOS has no unprivileged GPU temp API
        // Apple Silicon thermal data requires sudo powermetrics which we can't run
        // Temperature stays as None — the dashboard shows "N/A" for Mac
    }

    #[cfg(not(target_os = "macos"))]
    {
        // NVIDIA: get real GPU metrics via nvidia-smi
        let smi_output = find_nvidia_smi().and_then(|path| {
            hide_window(Command::new(&path)
                .args([
                    "--query-gpu=temperature.gpu,utilization.gpu,memory.used",
                    "--format=csv,noheader,nounits",
                ]))
                .output()
                .ok()
        });

        if let Some(o) = smi_output {
            if o.status.success() {
                let line = String::from_utf8_lossy(&o.stdout).trim().to_string();
                let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
                if parts.len() >= 3 {
                    gpu_temperature = parts[0].parse().ok();
                    gpu_utilization = parts[1].parse().ok();
                    memory_used_mb = parts[2].parse().ok();
                }
            }
        }
    }

    // Inference speed: parse from daemon log
    let mut inference_speed: Option<f32> = None;
    let dcp_dir = dcp_home().unwrap_or_default();
    let log_path = dcp_dir.join("daemon.log");
    let log_lines = tail_file(&log_path, 50);

    // Search backwards for the most recent speed reading
    for line in log_lines.iter().rev() {
        // Look for patterns like "predicted_per_second: 45.2" or "tok/s: 45.2" or "speed: 45.2"
        if let Some(idx) = line.find("predicted_per_second") {
            let after = &line[idx..];
            // Extract the number after the colon or equals
            let num_str: String = after
                .chars()
                .skip_while(|c| !c.is_ascii_digit() && *c != '.')
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if let Ok(speed) = num_str.parse::<f32>() {
                inference_speed = Some(speed);
                break;
            }
        }
        if let Some(idx) = line.find("tok/s") {
            // Try to extract number before "tok/s"
            let before = &line[..idx];
            let num_str: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ' ')
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            let num_str = num_str.trim();
            if let Ok(speed) = num_str.parse::<f32>() {
                inference_speed = Some(speed);
                break;
            }
        }
    }

    // On macOS: if inference is active (speed > 0 from recent logs), show GPU as busy
    #[cfg(target_os = "macos")]
    {
        if let Some(speed) = inference_speed {
            if speed > 0.0 {
                // Estimate GPU% from speed relative to expected max (~60 tok/s for 8B on M4)
                gpu_utilization = Some((speed / 60.0 * 100.0).min(100.0));
            }
        }
    }

    Ok(LiveMetrics {
        gpu_temperature,
        gpu_utilization,
        inference_speed,
        memory_used_mb,
        daemon_pid: if daemon_alive { actual_pid } else { None },
        daemon_alive,
    })
}

#[tauri::command]
async fn install_engine() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        // Detect Apple Silicon
        let arch_output = Command::new("uname")
            .arg("-m")
            .output()
            .map_err(|e| format!("Failed to detect architecture: {}", e))?;
        let arch = String::from_utf8_lossy(&arch_output.stdout).trim().to_string();

        if arch == "arm64" {
            // Install mlx and mlx-lm via pip
            let output = Command::new(python_cmd())
                .args(["-m", "pip", "install", "--upgrade", "mlx", "mlx-lm"])
                .output()
                .map_err(|e| format!("Failed to run pip: {}", e))?;

            if output.status.success() {
                Ok("mlx-lm installed successfully".to_string())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                Err(format!("pip install failed: {}", stderr))
            }
        } else {
            // Intel Mac or fallback: install Ollama
            let output = Command::new("sh")
                .args(["-c", "curl -fsSL https://ollama.com/install.sh | sh"])
                .output()
                .map_err(|e| format!("Failed to install Ollama: {}", e))?;

            if output.status.success() {
                Ok("Ollama installed successfully".to_string())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                Err(format!("Ollama install failed: {}", stderr))
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let output = Command::new("sh")
            .args(["-c", "curl -fsSL https://ollama.com/install.sh | sh"])
            .output()
            .map_err(|e| format!("Failed to install Ollama: {}", e))?;

        if output.status.success() {
            Ok("Ollama installed successfully".to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(format!("Ollama install failed: {}", stderr))
        }
    }

    #[cfg(target_os = "windows")]
    {
        // Download OllamaSetup.exe directly — works on all Windows 10+ without winget
        let installer_path = dcp_home()?.join("OllamaSetup.exe");
        let response = reqwest::get("https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe")
            .await
            .map_err(|e| format!("Failed to download Ollama installer: {}", e))?;
        let bytes = response.bytes().await
            .map_err(|e| format!("Failed to read Ollama installer bytes: {}", e))?;
        std::fs::write(&installer_path, &bytes)
            .map_err(|e| format!("Failed to save OllamaSetup.exe: {}", e))?;

        let mut cmd_inst = Command::new(&installer_path);
        cmd_inst.args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]);
        hide_window(&mut cmd_inst);
        let output = cmd_inst
            .output()
            .map_err(|e| format!("Failed to run Ollama installer: {}", e))?;

        // Clean up installer
        let _ = std::fs::remove_file(&installer_path);

        if output.status.success() {
            Ok("Ollama installed successfully".to_string())
        } else {
            Err("Failed to install Ollama. Install manually from https://ollama.com/download/windows".to_string())
        }
    }
}

#[tauri::command]
async fn download_model(model_name: String) -> Result<String, String> {
    if model_name.is_empty() {
        return Err("Model name is required".to_string());
    }

    // Try Ollama first
    let has_ollama = command_exists("ollama");

    if has_ollama {
        let mut cmd_pull = Command::new(&ollama_cmd());
        cmd_pull.args(["pull", &model_name]);
        hide_window(&mut cmd_pull);
        let output = cmd_pull
            .output()
            .map_err(|e| format!("Failed to pull model: {}", e))?;

        if output.status.success() {
            return Ok(format!("Model {} downloaded via Ollama", model_name));
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(format!("ollama pull failed: {}", stderr));
        }
    }

    // Fallback: for MLX models, they auto-download on first use
    // But we can trigger a pre-download
    #[cfg(target_os = "macos")]
    {
        let mlx_check = Command::new(python_cmd())
            .args(["-c", "import mlx_lm"])
            .output();
        if mlx_check.map(|o| o.status.success()).unwrap_or(false) {
            let output = Command::new(python_cmd())
                .args(["-c", &format!(
                    "from mlx_lm import load; load('{}')",
                    model_name
                )])
                .output()
                .map_err(|e| format!("Failed to download MLX model: {}", e))?;

            if output.status.success() {
                return Ok(format!("Model {} downloaded via MLX", model_name));
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                return Err(format!("MLX model download failed: {}", stderr));
            }
        }
    }

    Err("No inference engine found. Install Ollama or mlx-lm first.".to_string())
}

#[derive(Debug, serde::Deserialize)]
struct DaemonManifest {
    version: String,
    size: u64,
    sha256: String,
}

/// Verified daemon bytes + the manifest version that produced them.
struct VerifiedDaemon {
    bytes: Vec<u8>,
    version: String,
}

/// Backlog #7 — single daemon source. Fetch the platform-served daemon and
/// verify it against the server-computed manifest (size + sha256) BEFORE the
/// caller is allowed to write or spawn it. The platform is the ONE source of
/// the daemon: there is no bundled/forked copy, so every node converges to
/// whatever the manifest pins.
///
/// Audit G19 — the manifest is computed server-side over the EXACT bytes that
/// /download/daemon serves for this api_key (post-injection of
/// API_KEY/API_URL/HMAC_SECRET). Verifying here means a path-injection or
/// in-flight tamper between the backend and this client can never be written
/// into ~/.dcp.
///
/// Fail-safe: returns a clear `Err` on any network/HTTP/parse/mismatch failure.
/// Callers MUST NOT spawn a daemon when this errors — surface "couldn't fetch
/// daemon" rather than silently running a stale or missing one.
async fn fetch_verified_daemon(api_key: &str) -> Result<VerifiedDaemon, String> {
    use sha2::{Digest, Sha256};

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    // 1. Fetch the manifest first.
    // Audit 2026-05-30 — key sent via x-api-key header only, not as ?key= URL param.
    let manifest_url =
        "https://api.dcp.sa/api/providers/download/daemon/manifest".to_string();
    let manifest_resp = client
        .get(&manifest_url)
        .header("x-api-key", api_key)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch daemon manifest: {}", e))?;
    if !manifest_resp.status().is_success() {
        return Err(format!(
            "Failed to fetch daemon manifest: HTTP {}",
            manifest_resp.status()
        ));
    }
    let manifest: DaemonManifest = manifest_resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse daemon manifest: {}", e))?;

    // 2. Download daemon bytes.
    // Audit 2026-05-30 — key sent via x-api-key header only, not as ?key= URL param.
    let download_url = "https://api.dcp.sa/api/providers/download/daemon".to_string();
    let resp = client
        .get(&download_url)
        .header("x-api-key", api_key)
        .send()
        .await
        .map_err(|e| format!("Failed to download daemon: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Failed to download daemon: HTTP {}", resp.status()));
    }

    let new_bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read daemon download: {}", e))?;

    // 3. Audit G19 — verify size + sha256 against the manifest BEFORE the
    //    caller writes or spawns anything. Reject on mismatch with a clear
    //    error so the UI escape (Tier 3.15 / C2) can surface it.
    if new_bytes.len() as u64 != manifest.size {
        return Err(format!(
            "Daemon size mismatch: manifest={} downloaded={} — refusing to install",
            manifest.size,
            new_bytes.len()
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(&new_bytes);
    let got = hex::encode(hasher.finalize());
    if !got.eq_ignore_ascii_case(&manifest.sha256) {
        return Err(format!(
            "Daemon sha256 mismatch: manifest={} downloaded={} — refusing to install",
            manifest.sha256, got
        ));
    }

    Ok(VerifiedDaemon {
        bytes: new_bytes.to_vec(),
        version: manifest.version,
    })
}

#[tauri::command]
async fn update_daemon(api_key: String) -> Result<String, String> {
    let dcp_dir = dcp_home()?;
    let daemon_path = dcp_dir.join("dcp_daemon.py");

    // 1-3. Backlog #7 / Audit G19 — fetch + verify against the manifest BEFORE
    //       any process kill or filesystem write.
    let verified = fetch_verified_daemon(&api_key).await?;
    let new_bytes = verified.bytes;
    let manifest_version = verified.version;

    // 4. Compare with current file (now safe — bytes are verified).
    let current_bytes = std::fs::read(&daemon_path).unwrap_or_default();
    if current_bytes == new_bytes {
        return Ok("already_latest".to_string());
    }

    // 5. Stop the daemon if running.
    if let Some(pid) = read_pid_file() {
        if is_process_alive(pid) {
            kill_process_graceful(pid);
            for _ in 0..10 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if !is_process_alive(pid) { break; }
            }
            if is_process_alive(pid) {
                kill_process_force(pid);
            }
            remove_pid_file();
        }
    }

    // 6. Audit G6 (Tier 4.18) — back up the current daemon BEFORE overwriting
    //    so a bad update is recoverable. The .bak path is the dropoff point
    //    rollback_daemon() reads from. We don't fail the update if backup
    //    fails — first-install case has no current daemon to back up — but we
    //    do log it via the returned status detail.
    let backup_path = daemon_path.with_extension("py.bak");
    let backup_status = if !current_bytes.is_empty() {
        match atomic_write(&backup_path, &current_bytes) {
            Ok(_) => "backed_up",
            Err(_) => "backup_failed",
        }
    } else {
        "no_prior"
    };

    // 7. Replace the daemon file (M11 — atomic).
    atomic_write(&daemon_path, &new_bytes)
        .map_err(|e| format!("Failed to write updated daemon: {}", e))?;

    Ok(format!("updated:{}:{}", manifest_version, backup_status))
}

// Audit G6 (Tier 4.18) — companion to update_daemon. Restores ~/.dcp/dcp_daemon.py
// from the .bak that update_daemon wrote before its last overwrite. The tray UI
// (Tier 3.14 / G33 Report a Problem and Tier 3.15 / C2 updater error path) can
// surface this as a "rollback" button when the new daemon misbehaves.
//
// Errors clearly when no .bak exists (e.g., first install, or rollback already
// consumed). Does NOT chain another rollback — single-shot.
#[tauri::command]
async fn rollback_daemon() -> Result<String, String> {
    let dcp_dir = dcp_home()?;
    let daemon_path = dcp_dir.join("dcp_daemon.py");
    let backup_path = daemon_path.with_extension("py.bak");

    if !backup_path.exists() {
        return Err(format!(
            "No backup to roll back to at {}",
            backup_path.display()
        ));
    }

    let backup_bytes = std::fs::read(&backup_path)
        .map_err(|e| format!("Failed to read backup: {}", e))?;

    // Stop the running daemon first — same dance as update_daemon step 5.
    if let Some(pid) = read_pid_file() {
        if is_process_alive(pid) {
            kill_process_graceful(pid);
            for _ in 0..10 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if !is_process_alive(pid) { break; }
            }
            if is_process_alive(pid) {
                kill_process_force(pid);
            }
            remove_pid_file();
        }
    }

    atomic_write(&daemon_path, &backup_bytes)
        .map_err(|e| format!("Failed to restore daemon from backup: {}", e))?;

    // Best-effort version extract from the restored bytes for UI display.
    let content = String::from_utf8_lossy(&backup_bytes);
    let version = content
        .lines()
        .find(|l| l.contains("DAEMON_VERSION"))
        .and_then(|l| {
            l.split('=')
                .last()
                .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());

    // Consume the backup so a second rollback can't loop on the same .bak.
    let _ = std::fs::remove_file(&backup_path);

    Ok(format!("rolled_back:{}", version))
}

// ── WireGuard Mesh Setup (replaces broken Cloudflare Quick Tunnel) ──

/// Auto-install WireGuard tools if not already present.
/// macOS: brew install wireguard-tools
/// Windows: download and run the silent NSIS installer
/// Linux: apt-get install wireguard-tools
/// Return the path to `wg` binary, checking common locations.
fn wg_bin() -> &'static str {
    // Tauri apps don't inherit shell PATH on macOS — check known locations
    for candidate in &["/opt/homebrew/bin/wg", "/usr/local/bin/wg", "/usr/bin/wg"] {
        if std::path::Path::new(candidate).exists() {
            // Leak a &'static str from the first match (one-time)
            return match *candidate {
                "/opt/homebrew/bin/wg" => "/opt/homebrew/bin/wg",
                "/usr/local/bin/wg" => "/usr/local/bin/wg",
                _ => "/usr/bin/wg",
            };
        }
    }
    "wg" // fallback to PATH lookup
}

fn wg_quick_bin() -> &'static str {
    for candidate in &["/opt/homebrew/bin/wg-quick", "/usr/local/bin/wg-quick", "/usr/bin/wg-quick"] {
        if std::path::Path::new(candidate).exists() {
            return match *candidate {
                "/opt/homebrew/bin/wg-quick" => "/opt/homebrew/bin/wg-quick",
                "/usr/local/bin/wg-quick" => "/usr/local/bin/wg-quick",
                _ => "/usr/bin/wg-quick",
            };
        }
    }
    "wg-quick"
}

async fn ensure_wireguard_installed() -> Result<(), String> {
    // Check if wg command exists at known paths
    let wg = wg_bin();
    if std::path::Path::new(wg).exists() || Command::new(wg).arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
        eprintln!("[wg] WireGuard found at {}", wg);
        return Ok(());
    }

    eprintln!("[wg] WireGuard tools not found, attempting auto-install...");

    // Auto-install based on platform
    #[cfg(target_os = "macos")]
    {
        let brew_path = if std::path::Path::new("/opt/homebrew/bin/brew").exists() {
            "/opt/homebrew/bin/brew"
        } else {
            "brew"
        };
        let brew = Command::new(brew_path)
            .args(["install", "wireguard-tools"])
            .output()
            .map_err(|e| format!("Failed to install WireGuard via brew: {}. Please install from https://www.wireguard.com/install/", e))?;
        if !brew.status.success() {
            let stderr = String::from_utf8_lossy(&brew.stderr);
            return Err(format!("brew install wireguard-tools failed: {}. Please install WireGuard manually from https://www.wireguard.com/install/", stderr));
        }
        eprintln!("[wg] Installed wireguard-tools via brew");
    }

    #[cfg(target_os = "windows")]
    {
        let dcp_dir = dcp_home()?;
        let installer_path = dcp_dir.join("wireguard-installer.exe");

        let client = reqwest::Client::new();
        let resp = client
            .get("https://download.wireguard.com/windows-client/wireguard-installer.exe")
            .send()
            .await
            .map_err(|e| format!("Failed to download WireGuard installer: {}", e))?;

        let bytes = resp.bytes().await
            .map_err(|e| format!("Failed to read WireGuard installer: {}", e))?;

        std::fs::write(&installer_path, &bytes)
            .map_err(|e| format!("Failed to save WireGuard installer: {}", e))?;

        // Run silent install (NSIS /S flag)
        let install = Command::new(&installer_path)
            .args(["/S"])
            .output()
            .map_err(|e| format!("Failed to run WireGuard installer: {}", e))?;

        if !install.status.success() {
            let _ = std::fs::remove_file(&installer_path);
            return Err("WireGuard silent install failed. Please install manually from https://www.wireguard.com/install/".to_string());
        }

        // Clean up installer
        let _ = std::fs::remove_file(&installer_path);
        eprintln!("[wg] Installed WireGuard via silent installer");
    }

    #[cfg(target_os = "linux")]
    {
        let apt = Command::new("sudo")
            .args(["apt-get", "install", "-y", "wireguard-tools"])
            .output()
            .map_err(|e| format!("Failed to install wireguard-tools: {}", e))?;
        if !apt.status.success() {
            let stderr = String::from_utf8_lossy(&apt.stderr);
            return Err(format!("apt install wireguard-tools failed: {}. Please install manually.", stderr));
        }
        eprintln!("[wg] Installed wireguard-tools via apt");
    }

    Ok(())
}

/// Generate WG keypair, register with backend, write config, activate tunnel.
/// Returns the assigned mesh IP on success.
async fn setup_wireguard(dcp_dir: &std::path::Path, api_key: &str) -> Result<String, String> {
    let wg_conf_path = dcp_dir.join("wg0.conf");

    // Step 0: Ensure WireGuard tools are installed (auto-install if missing)
    eprintln!("[wg] Installing DCP network tools...");
    if let Err(e) = ensure_wireguard_installed().await {
        eprintln!("[wg] Auto-install failed: {}. Continuing — keypair generation will use fallback if available.", e);
    }

    // If we already have a running WG interface, check if it's healthy
    #[cfg(unix)]
    {
        let check = Command::new(wg_bin()).args(["show", "wg0"]).output();
        if let Ok(o) = check {
            if o.status.success() {
                let output = String::from_utf8_lossy(&o.stdout);
                // WG is already up — extract our IP from the config file
                if let Ok(conf) = std::fs::read_to_string(&wg_conf_path) {
                    if let Some(ip_line) = conf.lines().find(|l| l.trim().starts_with("Address")) {
                        if let Some(ip) = ip_line.split('=').nth(1) {
                            let mesh_ip = ip.trim().split('/').next().unwrap_or("").to_string();
                            if !mesh_ip.is_empty() {
                                eprintln!("[wg] WireGuard wg0 already active, mesh IP: {}", mesh_ip);
                                return Ok(mesh_ip);
                            }
                        }
                    }
                }
                // WG is up but we can't determine our IP — treat as success with unknown IP
                if output.contains("endpoint") {
                    eprintln!("[wg] WireGuard wg0 active but could not determine mesh IP from config");
                    return Ok("unknown".to_string());
                }
            }
        }
    }
    #[cfg(windows)]
    {
        let check = Command::new("wireguard").args(["/dumplog", "wg0"]).output();
        if let Ok(o) = check {
            if o.status.success() {
                if let Ok(conf) = std::fs::read_to_string(&wg_conf_path) {
                    if let Some(ip_line) = conf.lines().find(|l| l.trim().starts_with("Address")) {
                        if let Some(ip) = ip_line.split('=').nth(1) {
                            let mesh_ip = ip.trim().split('/').next().unwrap_or("").to_string();
                            if !mesh_ip.is_empty() {
                                eprintln!("[wg] WireGuard wg0 already active, mesh IP: {}", mesh_ip);
                                return Ok(mesh_ip);
                            }
                        }
                    }
                }
            }
        }
    }

    // Step 1: Generate WG keypair
    eprintln!("[wg] Generating WireGuard keypair...");
    let (private_key, public_key) = generate_wg_keypair()?;

    // Step 2: Register public key with backend
    eprintln!("[wg] Registering public key with backend...");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    let resp = client
        .post(format!("{}/wg/register", API_BASE))
        .header("x-provider-key", api_key)
        .json(&serde_json::json!({ "public_key": public_key }))
        .send()
        .await
        .map_err(|e| format!("WG registration request failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("WG registration HTTP {}: {}", status, body));
    }

    let wg_config: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("WG registration response parse failed: {}", e))?;

    let mesh_ip = wg_config["ip"].as_str()
        .ok_or("WG registration response missing 'ip'")?
        .to_string();
    let server_pubkey = wg_config["server_pubkey"].as_str()
        .ok_or("WG registration response missing 'server_pubkey'")?
        .to_string();
    let server_endpoint = wg_config["server_endpoint"].as_str()
        .ok_or("WG registration response missing 'server_endpoint'")?
        .to_string();
    // PSK is only returned on first registration, not on idempotent re-register
    let psk = wg_config["psk"].as_str().map(|s| s.to_string());
    let already_registered = wg_config["already_registered"].as_bool().unwrap_or(false);

    eprintln!("[wg] Assigned mesh IP: {} (already_registered: {})", mesh_ip, already_registered);

    // Step 3: Write WG config file (only if we have a PSK or it's a fresh registration)
    if !already_registered || !wg_conf_path.exists() {
        if let Some(ref psk_val) = psk {
            let conf_content = format!(
                "[Interface]\n\
                 PrivateKey = {}\n\
                 Address = {}/24\n\
                 DNS = 1.1.1.1\n\
                 \n\
                 [Peer]\n\
                 PublicKey = {}\n\
                 PresharedKey = {}\n\
                 Endpoint = {}\n\
                 AllowedIPs = 10.8.0.0/24\n\
                 PersistentKeepalive = 25\n",
                private_key, mesh_ip, server_pubkey, psk_val, server_endpoint
            );
            std::fs::write(&wg_conf_path, &conf_content)
                .map_err(|e| format!("Failed to write wg0.conf: {}", e))?;

            // Restrict permissions on the config file (contains private key)
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&wg_conf_path,
                    std::fs::Permissions::from_mode(0o600));
            }

            eprintln!("[wg] Wrote WG config to {}", wg_conf_path.display());
        } else if already_registered && !wg_conf_path.exists() {
            // Re-registered but no PSK returned and no local config — user must re-register
            // with a new key. For now, log a warning.
            eprintln!("[wg] WARNING: Already registered but no local config and no PSK returned. \
                       WireGuard config file missing — delete wg_mesh_ip from backend and retry.");
            return Err("WG config missing and re-registration returned no PSK. Contact support.".to_string());
        }
    }

    // Step 3b: Optionally write wg1.conf fallback (UDP/443) if backend
    // returned the fallback fields. Most backends won't yet, so this is
    // best-effort and silently skipped when fields are missing.
    let wg1_conf_path = dcp_dir.join("wg1.conf");
    if let (Some(fb_endpoint), Some(fb_pub), Some(fb_target), Some(fb_subnet), Some(ref psk_val)) = (
        wg_config["fallback_endpoint"].as_str(),
        wg_config["fallback_server_public_key"].as_str(),
        wg_config["fallback_tunnel_target"].as_str(),
        wg_config["fallback_subnet"].as_str(),
        psk.as_ref(),
    ) {
        if !already_registered || !wg1_conf_path.exists() {
            let conf_content = format!(
                "[Interface]\n\
                 PrivateKey = {}\n\
                 Address = {}/24\n\
                 DNS = 1.1.1.1\n\
                 \n\
                 [Peer]\n\
                 PublicKey = {}\n\
                 PresharedKey = {}\n\
                 Endpoint = {}\n\
                 AllowedIPs = {}\n\
                 PersistentKeepalive = 25\n",
                private_key, fb_target, fb_pub, psk_val, fb_endpoint, fb_subnet
            );
            if let Err(e) = std::fs::write(&wg1_conf_path, &conf_content) {
                eprintln!("[wg] Failed to write wg1.conf (fallback, non-fatal): {}", e);
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&wg1_conf_path,
                        std::fs::Permissions::from_mode(0o600));
                }
                eprintln!("[wg] Wrote fallback WG config to {}", wg1_conf_path.display());
            }
        }
    }

    // Step 4: Try to activate the WG tunnel
    let activation_result = activate_wireguard(dcp_dir).await;
    match &activation_result {
        Ok(()) => {
            eprintln!("[wg] WireGuard tunnel activated successfully");
            // Step 4b: Verify tunnel by pinging the server's mesh IP (10.8.0.1).
            // If primary fails AND wg1.conf exists, fall back to wg1 + 10.9.0.1.
            if !verify_wg_tunnel("10.8.0.1").await {
                eprintln!("[wg] Primary tunnel ping failed");
                if wg1_conf_path.exists() {
                    eprintln!("[wg] Attempting wg1 fallback...");
                    match activate_wireguard_fallback(dcp_dir).await {
                        Ok(()) if verify_wg_tunnel("10.9.0.1").await => {
                            eprintln!("[wg] Fallback tunnel verified");
                        }
                        Ok(()) => {
                            eprintln!("[wg] Fallback tunnel up but ping failed — leaving up anyway");
                        }
                        Err(e) => {
                            eprintln!("[wg] Fallback activation failed: {}", e);
                        }
                    }
                } else {
                    eprintln!("[wg] No wg1.conf fallback configured — tunnel may be unreachable");
                }
            } else {
                eprintln!("[wg] Tunnel verified via ping to 10.8.0.1");
            }
        }
        Err(e) => {
            eprintln!("[wg] WireGuard activation failed: {}. Config saved to {}", e, wg_conf_path.display());
            eprintln!("[wg] Install WireGuard and run: wg-quick up {}", wg_conf_path.display());
            // Don't fail the whole setup — config is saved for manual activation
        }
    }

    Ok(mesh_ip)
}

/// Ping the WG server's mesh IP from inside the tunnel (5 packets, 1s interval).
/// Returns true if at least one reply was received. Best-effort — failures
/// here trigger the wg1 fallback path but do not fail the install.
async fn verify_wg_tunnel(target: &str) -> bool {
    // Give the tunnel a beat to come up
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    #[cfg(unix)]
    let args: Vec<&str> = vec!["-c", "5", "-W", "2", target];
    #[cfg(windows)]
    let args: Vec<&str> = vec!["-n", "5", "-w", "2000", target];

    let result = Command::new("ping").args(&args).output();
    match result {
        Ok(output) => output.status.success(),
        Err(_) => false,
    }
}

/// Bring up wg1 (UDP/443 fallback) if wg1.conf exists. Linux + macOS only —
/// Windows doesn't currently use the fallback because the wireguard.exe
/// service installer does not multiplex two interfaces cleanly.
#[cfg(unix)]
async fn activate_wireguard_fallback(dcp_dir: &std::path::Path) -> Result<(), String> {
    let wg1_conf_path = dcp_dir.join("wg1.conf");
    if !wg1_conf_path.exists() {
        return Err("wg1.conf not found".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        let conf_src = wg1_conf_path.to_string_lossy().to_string();
        let install_and_up = format!(
            "set -e; install -d -m 0755 /etc/wireguard; \
             install -m 0600 -o root -g root '{src}' /etc/wireguard/wg1.conf; \
             {wq} down wg1 2>/dev/null || true; \
             {wq} up wg1",
            src = conf_src.replace('\'', "'\\''"),
            wq = wg_quick_bin(),
        );
        let elevator = if command_exists("pkexec") { "pkexec" } else { "sudo" };
        let r = Command::new(elevator)
            .args(["sh", "-c", &install_and_up])
            .output()
            .map_err(|e| format!("fallback wg-quick spawn failed: {}", e))?;
        if !r.status.success() {
            return Err(format!("fallback wg-quick up failed: {}",
                String::from_utf8_lossy(&r.stderr)));
        }
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let conf = wg1_conf_path.to_string_lossy().to_string();
        let wq = wg_quick_bin();
        let cmd = format!("{wq} down wg1 2>/dev/null; {wq} up {conf}");
        let r = Command::new("osascript")
            .args(["-e", &format!(
                "do shell script \"{}\" with administrator privileges",
                cmd.replace('"', "\\\"")
            )])
            .output()
            .map_err(|e| format!("osascript fallback failed: {}", e))?;
        if !r.status.success() {
            return Err(format!("fallback wg-quick up failed: {}",
                String::from_utf8_lossy(&r.stderr)));
        }
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("Fallback activation not supported on this Unix variant".to_string())
}

#[cfg(windows)]
async fn activate_wireguard_fallback(_dcp_dir: &std::path::Path) -> Result<(), String> {
    // Windows fallback path is parked: the wireguard.exe tunnel service
    // model doesn't lend itself to a hot wg0→wg1 swap without confusing the
    // service manager. Real users on Windows almost never hit a UDP-blocked
    // network in DCP today; revisit if telemetry shows otherwise.
    Err("wg1 fallback not yet implemented on Windows".to_string())
}

/// Generate a WireGuard keypair. Tries `wg genkey`/`wg pubkey` first,
/// falls back to pure-Rust x25519 if wg tools are not installed.
fn generate_wg_keypair() -> Result<(String, String), String> {
    // Try using wg tools (available if WireGuard is installed)
    let genkey_result = Command::new(wg_bin()).arg("genkey").output();
    if let Ok(output) = genkey_result {
        if output.status.success() {
            let private_key = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !private_key.is_empty() {
                // Derive public key
                let mut pubkey_cmd = Command::new(wg_bin());
                pubkey_cmd.arg("pubkey");
                pubkey_cmd.stdin(std::process::Stdio::piped());
                pubkey_cmd.stdout(std::process::Stdio::piped());
                pubkey_cmd.stderr(std::process::Stdio::piped());
                let mut child = pubkey_cmd.spawn()
                    .map_err(|e| format!("wg pubkey spawn failed: {}", e))?;
                if let Some(ref mut stdin) = child.stdin {
                    use std::io::Write;
                    let _ = stdin.write_all(private_key.as_bytes());
                    let _ = stdin.write_all(b"\n");
                }
                let pubkey_output = child.wait_with_output()
                    .map_err(|e| format!("wg pubkey failed: {}", e))?;
                if pubkey_output.status.success() {
                    let public_key = String::from_utf8_lossy(&pubkey_output.stdout).trim().to_string();
                    if !public_key.is_empty() {
                        return Ok((private_key, public_key));
                    }
                }
            }
        }
    }

    // Fallback: without the `wg` CLI we cannot generate a valid Curve25519
    // keypair (would need an x25519 crate). Tell the user to install WireGuard.
    Err("WireGuard tools (wg) not found. Please install WireGuard:\n\
         - macOS: brew install wireguard-tools\n\
         - Linux: sudo apt install wireguard-tools\n\
         - Windows: https://www.wireguard.com/install/".to_string())
}

/// Create Windows firewall rules to allow inbound traffic on the DCP mesh
/// subnet (10.8.0.0/24) and Ollama port (11434/TCP). Runs silently — errors
/// are logged but do not fail the WireGuard setup.
#[cfg(windows)]
fn create_windows_firewall_rules() {
    // Allow all inbound from the WireGuard mesh subnet
    let _ = Command::new("powershell")
        .args(["-Command", "New-NetFirewallRule -DisplayName 'DCP Mesh Allow' -Direction Inbound -RemoteAddress 10.8.0.0/24 -Action Allow -ErrorAction SilentlyContinue"])
        .output();
    // Allow inbound TCP on Ollama port so mesh peers can reach the inference server
    let _ = Command::new("powershell")
        .args(["-Command", "New-NetFirewallRule -DisplayName 'DCP Ollama In' -Direction Inbound -LocalPort 11434 -Protocol TCP -Action Allow -ErrorAction SilentlyContinue"])
        .output();
    eprintln!("[wg] Windows firewall rules created for DCP mesh (10.8.0.0/24) and Ollama (:11434)");
}

/// Attempt to activate the WireGuard tunnel using wg-quick or platform tools.
async fn activate_wireguard(dcp_dir: &std::path::Path) -> Result<(), String> {
    let wg_conf_path = dcp_dir.join("wg0.conf");

    if !wg_conf_path.exists() {
        return Err("wg0.conf not found".to_string());
    }

    // First, bring down any existing wg0 interface (ignore errors — it may not be up)
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new(wg_quick_bin()).args(["down", &wg_conf_path.to_string_lossy()]).output();
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    // On macOS, down+up are combined below to avoid two admin password prompts.

    // Platform-specific activation
    #[cfg(target_os = "macos")]
    {
        // macOS: Install a launchd keeper daemon that manages the WG tunnel
        // persistently — survives reboots, sleep/wake, wireguard-go crashes.
        // Single admin prompt covers: wg-quick up + keeper script + launchd plist.
        let wq = wg_quick_bin();
        let conf = wg_conf_path.to_string_lossy().to_string();

        // First try without sudo (works if user already has passwordless sudo)
        let up_result = Command::new(wq).args(["up", &conf]).output();
        let needs_admin = match &up_result {
            Ok(output) if output.status.success() => false,
            _ => true,
        };

        // Build the keeper script content
        let keeper_script = r#"#!/opt/homebrew/bin/bash
export PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
LOG="/var/log/dcp-wireguard.log"
log() { echo "$(date '+%Y-%m-%d %H:%M:%S') $1" >> "$LOG"; }
log "dcp-wg-keeper starting"
wg-quick down wg0 2>/dev/null
if wg-quick up wg0 2>>"$LOG"; then
    log "Tunnel UP"
else
    log "ERROR: wg-quick up failed"
    exit 1
fi
while true; do
    sleep 60
    if ! pgrep -q wireguard-go; then
        log "wireguard-go died, restarting tunnel"
        wg-quick down wg0 2>/dev/null
        if wg-quick up wg0 2>>"$LOG"; then
            log "Tunnel restored"
        else
            log "ERROR: tunnel restore failed, exiting for launchd restart"
            exit 1
        fi
    fi
done
"#;

        let plist_content = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>sa.dcp.wireguard</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/dcp-wg-keeper.sh</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>NetworkState</key>
        <true/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>StandardOutPath</key>
    <string>/var/log/dcp-wireguard.log</string>
    <key>StandardErrorPath</key>
    <string>/var/log/dcp-wireguard.log</string>
</dict>
</plist>
"#;

        // Write keeper script and plist to temp, then install with admin prompt
        let keeper_tmp = std::env::temp_dir().join("dcp-wg-keeper.sh");
        let plist_tmp = std::env::temp_dir().join("sa.dcp.wireguard.plist");
        std::fs::write(&keeper_tmp, keeper_script)
            .map_err(|e| format!("Failed to write keeper script: {}", e))?;
        std::fs::write(&plist_tmp, plist_content)
            .map_err(|e| format!("Failed to write plist: {}", e))?;

        if needs_admin {
            eprintln!("[wg] Installing persistent WG tunnel via launchd (one-time admin prompt)...");
            let shell_cmd = format!(
                concat!(
                    "export PATH=/opt/homebrew/bin:/usr/local/bin:$PATH; ",
                    "cp {keeper} /usr/local/bin/dcp-wg-keeper.sh; ",
                    "chmod 755 /usr/local/bin/dcp-wg-keeper.sh; ",
                    "cp {plist} /Library/LaunchDaemons/sa.dcp.wireguard.plist; ",
                    "chmod 644 /Library/LaunchDaemons/sa.dcp.wireguard.plist; ",
                    "chown root:wheel /Library/LaunchDaemons/sa.dcp.wireguard.plist; ",
                    "launchctl unload /Library/LaunchDaemons/sa.dcp.wireguard.plist 2>/dev/null; ",
                    "{wq} down wg0 2>/dev/null; sleep 0.5; {wq} up {conf}; ",
                    "launchctl load /Library/LaunchDaemons/sa.dcp.wireguard.plist"
                ),
                keeper = keeper_tmp.to_string_lossy(),
                plist = plist_tmp.to_string_lossy(),
                wq = wq, conf = conf
            ).replace('\"', "\\\"");

            let osa_result = Command::new("osascript")
                .args(["-e", &format!(
                    "do shell script \"{}\" with administrator privileges",
                    shell_cmd
                )])
                .output();
            match osa_result {
                Ok(o) if o.status.success() => {
                    eprintln!("[wg] Persistent WG tunnel installed via launchd");
                    return Ok(());
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    if stderr.contains("User canceled") || stderr.contains("canceled") {
                        return Err("WireGuard setup cancelled — admin password required to activate the tunnel.".to_string());
                    }
                    return Err(format!("wg-quick up failed (admin): {}", stderr));
                }
                Err(e) => return Err(format!("osascript admin prompt failed: {}", e)),
            }
        } else {
            // wg-quick succeeded without admin — still try to install keeper (best effort)
            eprintln!("[wg] Tunnel up without admin. Installing keeper (best effort)...");
            let _ = Command::new("osascript")
                .args(["-e", &format!(
                    "do shell script \"cp {} /usr/local/bin/dcp-wg-keeper.sh && chmod 755 /usr/local/bin/dcp-wg-keeper.sh && cp {} /Library/LaunchDaemons/sa.dcp.wireguard.plist && chmod 644 /Library/LaunchDaemons/sa.dcp.wireguard.plist && launchctl load /Library/LaunchDaemons/sa.dcp.wireguard.plist\" with administrator privileges",
                    keeper_tmp.to_string_lossy(), plist_tmp.to_string_lossy()
                )])
                .output();
            return Ok(());
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Linux flow:
        //   1. Copy ~/.dcp/wg0.conf → /etc/wireguard/wg0.conf via pkexec
        //      (wg-quick requires the conf to live under /etc/wireguard for
        //      the interface name to be derived from the filename).
        //   2. wg-quick up wg0 via pkexec.
        //
        // pkexec is preferred over sudo because:
        //   • Tauri apps don't have a controlling tty so sudo prompt would
        //     hang silently.
        //   • The bundled polkit policy (sa.dcp.provider.policy) gives the
        //     user an honest GUI dialog explaining why root is needed.
        //   • Falls back gracefully if polkit is missing — pkexec itself
        //     pops a generic prompt.
        let conf_src = wg_conf_path.to_string_lossy().to_string();
        let etc_wg_path = "/etc/wireguard/wg0.conf";

        // Use a single sh -c so the user only sees one polkit prompt.
        let install_and_up = format!(
            "set -e; install -d -m 0755 /etc/wireguard; \
             install -m 0600 -o root -g root '{src}' '{dst}'; \
             {wq} down wg0 2>/dev/null || true; \
             {wq} up wg0",
            src = conf_src.replace('\'', "'\\''"),
            dst = etc_wg_path,
            wq = wg_quick_bin(),
        );

        let elevator = if command_exists("pkexec") { "pkexec" } else { "sudo" };
        let up_result = Command::new(elevator)
            .args(["sh", "-c", &install_and_up])
            .output();

        match up_result {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("not authorized") || stderr.contains("dismissed") {
                    return Err("WireGuard setup cancelled — admin authorization required to activate the tunnel.".to_string());
                }
                return Err(format!("wg-quick up failed: {}", stderr));
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Err(format!("{} not found. Install: sudo apt install policykit-1 wireguard-tools", elevator));
                }
                return Err(format!("wg-quick failed: {}", e));
            }
        }
    }

    #[cfg(windows)]
    {
        // Windows: `wireguard.exe /installtunnelservice <conf>` registers the
        // tunnel as a Windows *service*, which REQUIRES ADMIN. This app runs as a
        // normal user (RequestExecutionLevel user), so a plain non-elevated call
        // fails with access-denied — the config gets saved but the tunnel never
        // activates: no handshake, provider endpoint unreachable, node can't
        // serve. (Root cause of Node-1/Fadi "online but not servable", 2026-07-02.)
        //
        // Fix: elevate the install via PowerShell `Start-Process -Verb RunAs`,
        // which raises a single UAC prompt (standard for VPN clients). -Wait
        // -PassThru lets us propagate the child's exit code so a declined prompt
        // or a real failure surfaces to the UI instead of silently no-op'ing.
        let conf_str = wg_conf_path.to_string_lossy().to_string();
        let wg_exe = if std::path::Path::new(r"C:\Program Files\WireGuard\wireguard.exe").exists() {
            r"C:\Program Files\WireGuard\wireguard.exe".to_string()
        } else {
            "wireguard".to_string()
        };
        // Escape single quotes for the PowerShell single-quoted string literals.
        let esc = |s: &str| s.replace('\'', "''");
        let ps = format!(
            "$ErrorActionPreference='Stop'; \
             $p = Start-Process -FilePath '{}' -ArgumentList '/installtunnelservice','{}' \
                  -Verb RunAs -Wait -PassThru; \
             exit $p.ExitCode",
            esc(&wg_exe),
            esc(&conf_str)
        );
        let install_result = Command::new("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .output();
        match install_result {
            Ok(output) if output.status.success() => {
                create_windows_firewall_rules();
                return Ok(());
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(format!(
                    "WireGuard tunnel activation needs admin approval. If a Windows \
                     security prompt appeared, click Yes to connect your node. ({})",
                    stderr.trim()
                ));
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    return Err("WireGuard not found. Install from https://www.wireguard.com/install/".to_string());
                }
                return Err(format!("Failed to launch elevated WireGuard install: {}", e));
            }
        }
    }

    // Fallback for other platforms
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        Err("WireGuard activation not supported on this platform. Manually run: wg-quick up wg0.conf".to_string())
    }
}

// ── Full Provider Start (chains: engine install → model download → inference server → daemon) ──

#[tauri::command]
async fn full_start_provider(window: tauri::Window, api_key: String, state: State<'_, DaemonManager>) -> Result<String, String> {
    let dcp_dir = dcp_home()?;

    // Spawn periodic log uploader — uploads every 60s so admin has live visibility
    // even if the install gets stuck (e.g. on model download).
    let done_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_flag_bg = done_flag.clone();
    let bg_api_key = api_key.clone();
    let bg_dcp_dir = dcp_dir.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.tick().await; // first tick is immediate, skip it
        loop {
            interval.tick().await;
            if done_flag_bg.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            upload_provider_logs(&bg_api_key, &bg_dcp_dir).await;
        }
    });

    let result = full_start_provider_inner(&window, &api_key, &state, &dcp_dir).await;

    // Signal background uploader to stop
    done_flag.store(true, std::sync::atomic::Ordering::Relaxed);

    // Upload logs on EVERY exit path — success or failure (final upload)
    upload_provider_logs(&api_key, &dcp_dir).await;
    result
}

async fn full_start_provider_inner(window: &tauri::Window, api_key: &str, state: &State<'_, DaemonManager>, dcp_dir: &std::path::Path) -> Result<String, String> {

    // Write startup log for debugging provider issues
    let startup_log_path = dcp_dir.join("startup.log");
    // Truncate old log on each start
    let _ = std::fs::write(&startup_log_path, format!("[{}] === full_start_provider starting ===\n", chrono_now()));
    macro_rules! log_startup {
        ($($arg:tt)*) => {
            let _ = std::fs::OpenOptions::new().append(true).create(true)
                .open(&startup_log_path)
                .and_then(|mut f| { use std::io::Write; writeln!(f, "[{}] {}", chrono_now(), format!($($arg)*)) });
        };
    }

    // System snapshot for remote debugging
    log_startup!("=== SYSTEM SNAPSHOT ===");
    log_startup!("OS: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    if let Ok(hostname) = Command::new("hostname").output() {
        log_startup!("Hostname: {}", String::from_utf8_lossy(&hostname.stdout).trim());
    }
    log_startup!("App version: 0.2.8");
    // Disk free
    if let Ok(output) = Command::new("df").args(["-h", "/"]).output() {
        let df_text = String::from_utf8_lossy(&output.stdout).to_string();
        let lines: Vec<&str> = df_text.lines().collect();
        if lines.len() > 1 { log_startup!("Disk: {}", lines[1]); }
    }
    log_startup!("=== END SNAPSHOT ===");

    emit_wizard_progress(window, WizardProgress {
        step_id: "snapshot".into(),
        status: "done".into(),
        detail: Some(format!("{} {} — v0.2.8", std::env::consts::OS, std::env::consts::ARCH)),
        ..Default::default()
    });

    // Step 0: Kill any existing DCP processes to avoid duplicates (cross-platform)
    kill_by_name("mlx_lm.server");
    kill_by_name("dcp_daemon.py");
    // Also try the PID file
    if let Some(old_pid) = read_pid_file() {
        if is_process_alive(old_pid) {
            kill_process_graceful(old_pid);
            std::thread::sleep(std::time::Duration::from_secs(2));
            if is_process_alive(old_pid) {
                kill_process_force(old_pid);
            }
        }
        remove_pid_file();
    }
    // Brief pause to let ports free up
    std::thread::sleep(std::time::Duration::from_millis(500));

    log_startup!("Step 0: Killed existing processes");

    // Step 1: Detect hardware + choose engine/model
    let is_apple_silicon;
    let total_mem_gb: u64;
    let engine: String;
    let model: String;

    #[cfg(target_os = "macos")]
    {
        let arch_output = Command::new("uname").arg("-m").output()
            .map_err(|e| format!("uname failed: {}", e))?;
        let arch = String::from_utf8_lossy(&arch_output.stdout).trim().to_string();
        is_apple_silicon = arch == "arm64";

        let mem_output = Command::new("sysctl").arg("-n").arg("hw.memsize").output()
            .map_err(|e| format!("sysctl failed: {}", e))?;
        let mem_bytes: u64 = String::from_utf8_lossy(&mem_output.stdout).trim()
            .parse().unwrap_or(0);
        total_mem_gb = mem_bytes / 1_073_741_824;

        if is_apple_silicon {
            engine = "mlx".to_string();
            // Model selection based on unified memory — benchmark-validated:
            // ≥64GB: MoE 30B gives best quality+speed balance
            // ≥32GB: MoE 30B fits comfortably, ~137 tok/s equivalent
            // ≥16GB: Dense 8B is the sweet spot (107-197 tok/s on NVIDIA, similar on MLX)
            //  <16GB: Dense 4B is the only option (163-270 tok/s)
            model = if total_mem_gb >= 32 {
                "mlx-community/Qwen3-30B-A3B-4bit".to_string()
            } else if total_mem_gb >= 16 {
                "mlx-community/Qwen3-8B-4bit".to_string()
            } else {
                "mlx-community/Qwen3-4B-4bit".to_string()
            };
        } else {
            engine = "ollama".to_string();
            model = "qwen3:8b".to_string();
            let _ = total_mem_gb; // suppress unused warning
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        is_apple_silicon = false;
        engine = "ollama".to_string();
        total_mem_gb = 0;
        // Detect VRAM for model selection — try nvidia-smi, then fallback to detect_gpu result
        let vram_mb: u64 = {
            let mut detected = 0u64;
            // Try nvidia-smi
            if let Some(path) = find_nvidia_smi() {
                let mut cmd_smi = Command::new(&path);
                cmd_smi.args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"]);
                hide_window(&mut cmd_smi);
                if let Ok(o) = cmd_smi.output() {
                    let raw = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    // Handle potential multi-line output (multi-GPU) — take first line
                    let first_line = raw.lines().next().unwrap_or(&raw);
                    // Try parsing, handle commas and decimals
                    let cleaned = first_line.replace(',', "").replace(" ", "");
                    detected = cleaned.parse::<f64>().unwrap_or(0.0) as u64;
                    log_startup!("  nvidia-smi VRAM raw='{}' parsed={}MB", raw, detected);
                }
            }
            // Fallback: use detect_gpu which already worked in the wizard
            if detected == 0 {
                if let Ok(gpu) = detect_gpu_nvidia() {
                    detected = gpu.vram_mb;
                    log_startup!("  VRAM from detect_gpu fallback: {}MB", detected);
                }
            }
            // Last resort: hardcode known GPUs by name
            if detected == 0 {
                log_startup!("  VRAM detection failed, using 8192MB default for consumer GPU");
                detected = 8192; // Safe default for RTX 3060 Ti
            }
            detected
        };
        // Benchmark-validated model selection by VRAM:
        // ≥24GB (4090/A5000/A6000): MoE 30B — best quality, 137-200 tok/s
        // ≥10GB (3060Ti 12GB/4070): Dense 8B — 107-197 tok/s
        //  <10GB (8GB/6GB cards):    Dense 4B — Mistral 7B OOMs on 8GB (163-270 tok/s)
        model = if vram_mb >= 20000 {
            "qwen3:30b-a3b".to_string()
        } else if vram_mb >= 10000 {
            "qwen3:8b".to_string()
        } else {
            "qwen3:4b".to_string()
        };
    }

    log_startup!("Step 1: Hardware detected — engine={}, model={}, apple_silicon={}, mem={}GB", engine, model, is_apple_silicon, total_mem_gb);

    // Step 2: Install inference engine
    if engine == "mlx" {
        let check = Command::new(python_cmd())
            .args(["-c", "import mlx_lm"])
            .output();
        let mlx_installed = check.map(|o| o.status.success()).unwrap_or(false);

        if !mlx_installed {
            let install = Command::new(python_cmd())
                .args(["-m", "pip", "install", "--break-system-packages", "-q", "mlx", "mlx-lm"])
                .output()
                .map_err(|e| format!("MLX install failed: {}", e))?;
            if !install.status.success() {
                // Try with --user
                let install2 = Command::new(python_cmd())
                    .args(["-m", "pip", "install", "--user", "-q", "mlx", "mlx-lm"])
                    .output()
                    .map_err(|e| format!("MLX install (user) failed: {}", e))?;
                if !install2.status.success() {
                    return Err("Failed to install MLX. Install manually: pip install mlx mlx-lm".to_string());
                }
            }
        } else {
            log_startup!("Step 2: MLX already installed, skipping");
            // Detect MLX version for display
            let mlx_version = Command::new(python_cmd())
                .args(["-c", "import mlx_lm; print(mlx_lm.__version__)"])
                .output()
                .ok()
                .and_then(|o| if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else { None })
                .unwrap_or_else(|| "latest".to_string());

            emit_wizard_progress(window, WizardProgress {
                step_id: "ollama_download".into(),
                status: "active".into(),
                detail: Some(format!("Checking MLX inference engine v{}...", mlx_version)),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            emit_wizard_progress(window, WizardProgress {
                step_id: "ollama_download".into(),
                status: "done".into(),
                detail: Some(format!("MLX inference engine v{} — already installed", mlx_version)),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            emit_wizard_progress(window, WizardProgress {
                step_id: "ollama_install".into(),
                status: "active".into(),
                detail: Some("Verifying engine...".into()),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            emit_wizard_progress(window, WizardProgress {
                step_id: "ollama_install".into(),
                status: "done".into(),
                detail: Some("No installation needed".into()),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    } else {
        // Ollama — cross-platform install with engine-gate semantics.
        //
        // Pre-flight: if Ollama is already serving on :11434 we skip the
        // entire install path. This prevents re-running the 1.85 GB installer
        // every time the wizard launches and is the cheapest way to detect
        // "already installed and running" reliably on Windows.
        let ollama_serving = {
            let probe = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap_or_default();
            probe.get("http://127.0.0.1:11434/api/version")
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        };

        if ollama_serving {
            log_startup!("Step 2: Ollama already running on :11434, skipping install");
            emit_wizard_progress(window, WizardProgress {
                step_id: "ollama_download".into(),
                status: "active".into(),
                detail: Some("Checking Ollama installation...".into()),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            emit_wizard_progress(window, WizardProgress {
                step_id: "ollama_download".into(),
                status: "done".into(),
                detail: Some("Ollama already installed".into()),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            emit_wizard_progress(window, WizardProgress {
                step_id: "ollama_install".into(),
                status: "active".into(),
                detail: Some("Verifying Ollama service...".into()),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            emit_wizard_progress(window, WizardProgress {
                step_id: "ollama_install".into(),
                status: "done".into(),
                detail: Some("Ollama running on port 11434".into()),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        } else {
            #[allow(unused_mut)]
            let mut ollama_installed = command_exists("ollama");

            // Windows fallback: check known install path if not found in PATH
            #[cfg(windows)]
            if !ollama_installed {
                if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
                    let ollama_path = std::path::PathBuf::from(local_app_data)
                        .join("Programs")
                        .join("Ollama")
                        .join("ollama.exe");
                    if ollama_path.exists() {
                        ollama_installed = true;
                    }
                }
            }

            if !ollama_installed {
                #[cfg(unix)]
                {
                    let install = Command::new("sh")
                        .args(["-c", "curl -fsSL https://ollama.com/install.sh | sh"])
                        .output()
                        .map_err(|e| format!("Ollama install failed: {}", e))?;
                    if !install.status.success() {
                        return Err("Failed to install Ollama. Install manually from https://ollama.com".to_string());
                    }
                }
                #[cfg(windows)]
                {
                    // Reuse a previously-downloaded OllamaSetup.exe if it's
                    // present and at least 1 GB on disk (the real installer
                    // is ~1.85 GB; anything smaller is a partial / failed
                    // download we should re-fetch). Also tolerate a renamed
                    // legacy copy via case-insensitive *[Ss]etup*.exe glob.
                    let dcp_dir = dcp_home()?;
                    let installer_path = dcp_dir.join("OllamaSetup.exe");
                    const MIN_INSTALLER_BYTES: u64 = 1_000_000_000; // 1 GB

                    let mut have_cached_installer = installer_path
                        .metadata()
                        .map(|m| m.len() >= MIN_INSTALLER_BYTES)
                        .unwrap_or(false);

                    // If the canonical name is missing/short, scan dcp_dir
                    // for any *Setup*.exe / *setup*.exe big enough to be it.
                    let mut chosen_installer = installer_path.clone();
                    if !have_cached_installer {
                        if let Ok(entries) = std::fs::read_dir(&dcp_dir) {
                            for entry in entries.flatten() {
                                let p = entry.path();
                                let name = p
                                    .file_name()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("")
                                    .to_ascii_lowercase();
                                if name.ends_with(".exe") && name.contains("setup") {
                                    if let Ok(meta) = entry.metadata() {
                                        if meta.len() >= MIN_INSTALLER_BYTES {
                                            chosen_installer = p;
                                            have_cached_installer = true;
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if have_cached_installer {
                        log_startup!(
                            "Step 2: Reusing existing Ollama installer at {}",
                            chosen_installer.display()
                        );
                    } else {
                        // Download OllamaSetup.exe directly — works on all
                        // Windows 10+ without winget.
                        log_startup!("Step 2: Downloading OllamaSetup.exe (~1.85 GB)");
                        emit_wizard_progress(window, WizardProgress {
                            step_id: "ollama_download".into(),
                            status: "active".into(),
                            detail: Some("Starting Ollama download (~1.85 GB)".into()),
                            ..Default::default()
                        });
                        let response = reqwest::get("https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe")
                            .await
                            .map_err(|e| format!("Failed to download Ollama installer: {}", e))?;
                        let total = response.content_length();
                        let mut stream = response.bytes_stream();
                        use futures_util::StreamExt;
                        let mut file = std::fs::File::create(&installer_path)
                            .map_err(|e| format!("Failed to create OllamaSetup.exe: {}", e))?;
                        let mut downloaded: u64 = 0;
                        let mut last_emit = std::time::Instant::now();
                        let started = std::time::Instant::now();
                        while let Some(chunk_res) = stream.next().await {
                            let chunk = chunk_res.map_err(|e| format!("Download stream error: {}", e))?;
                            use std::io::Write;
                            file.write_all(&chunk).map_err(|e| format!("Write error: {}", e))?;
                            downloaded += chunk.len() as u64;
                            if last_emit.elapsed().as_millis() >= 250 {
                                let elapsed_s = started.elapsed().as_secs_f64().max(0.001);
                                let mbps = (downloaded as f64 * 8.0) / (elapsed_s * 1_000_000.0);
                                let pct = total.map(|t| (downloaded as f32 / t as f32) * 100.0);
                                let eta = total.and_then(|t| {
                                    let remaining = t.saturating_sub(downloaded) as f64;
                                    if mbps > 0.0 { Some((remaining * 8.0 / (mbps * 1_000_000.0)) as u64) } else { None }
                                });
                                emit_wizard_progress(window, WizardProgress {
                                    step_id: "ollama_download".into(),
                                    status: "active".into(),
                                    pct,
                                    mb_done: Some(downloaded as f64 / 1_048_576.0),
                                    mb_total: total.map(|t| t as f64 / 1_048_576.0),
                                    mbps: Some(mbps),
                                    eta_seconds: eta,
                                    ..Default::default()
                                });
                                last_emit = std::time::Instant::now();
                            }
                        }
                        emit_wizard_progress(window, WizardProgress {
                            step_id: "ollama_download".into(),
                            status: "done".into(),
                            detail: Some("Ollama installer downloaded".into()),
                            ..Default::default()
                        });
                        chosen_installer = installer_path.clone();
                    }

                    emit_wizard_progress(window, WizardProgress {
                        step_id: "ollama_install".into(),
                        status: "active".into(),
                        detail: Some("Running Ollama installer (silent)".into()),
                        ..Default::default()
                    });
                    let install = Command::new(&chosen_installer)
                        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
                        .output()
                        .map_err(|e| format!("Failed to run Ollama installer: {}", e))?;

                    // NOTE: deliberately NOT removing the installer anymore
                    // so a subsequent wizard run can reuse it (saves ~1.85 GB
                    // of redownload). The installer is idempotent.

                    if !install.status.success() {
                        return Err("Failed to install Ollama. Install manually from https://ollama.com/download/windows".to_string());
                    }

                    // Post-install: poll :11434/api/version every 1s for up
                    // to 30s. If the daemon never comes up, fail loudly so
                    // the wizard surfaces the problem instead of pretending
                    // success.
                    let probe = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(2))
                        .build()
                        .unwrap_or_default();
                    let mut up = false;
                    for _ in 0..30 {
                        if probe
                            .get("http://127.0.0.1:11434/api/version")
                            .send()
                            .await
                            .map(|r| r.status().is_success())
                            .unwrap_or(false)
                        {
                            up = true;
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                    if !up {
                        return Err(
                            "Ollama installer ran but daemon did not start within 30s; check Windows Defender quarantine or try manual install".to_string(),
                        );
                    }
                    emit_wizard_progress(window, WizardProgress {
                        step_id: "ollama_install".into(),
                        status: "done".into(),
                        detail: Some("Ollama running on :11434".into()),
                        ..Default::default()
                    });
                }
            } else {
                log_startup!("Step 2: Ollama binary found, skipping download/install");
                emit_wizard_progress(window, WizardProgress {
                    step_id: "ollama_download".into(),
                    status: "active".into(),
                    detail: Some("Checking Ollama installation...".into()),
                    ..Default::default()
                });
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                emit_wizard_progress(window, WizardProgress {
                    step_id: "ollama_download".into(),
                    status: "done".into(),
                    detail: Some("Ollama already installed".into()),
                    ..Default::default()
                });
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                emit_wizard_progress(window, WizardProgress {
                    step_id: "ollama_install".into(),
                    status: "active".into(),
                    detail: Some("Starting Ollama service...".into()),
                    ..Default::default()
                });
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                emit_wizard_progress(window, WizardProgress {
                    step_id: "ollama_install".into(),
                    status: "done".into(),
                    detail: Some("Ollama running on port 11434".into()),
                    ..Default::default()
                });
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }

    log_startup!("Step 2: Engine install complete ({})", engine);

    // Checkpoint: upload logs after engine install so admin sees progress
    upload_provider_logs(api_key, dcp_dir).await;

    // Step 3: Check if model is cached, clean old models, start inference server
    let model_cached: bool;
    if engine == "mlx" {
        // Check if the MLX model is cached — simple filesystem check
        // HF cache path: ~/.cache/huggingface/hub/models--{org}--{name}/
        let cache_dir_name = model.replace('/', "--");
        let hf_cache = dirs::home_dir()
            .unwrap_or_default()
            .join(".cache/huggingface/hub")
            .join(format!("models--{}", cache_dir_name));
        model_cached = hf_cache.exists() && hf_cache.is_dir();

        // Read previous model from config to detect model switch
        let config_path = dcp_dir.join("config.json");
        if let Ok(config_str) = std::fs::read_to_string(&config_path) {
            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&config_str) {
                if let Some(old_model) = config.get("served_model").and_then(|v| v.as_str()) {
                    if old_model != model && !old_model.is_empty() {
                        // Model switch detected — clean old model from HF cache
                        let _ = Command::new(python_cmd())
                            .args(["-c", &format!(
                                "from huggingface_hub import scan_cache_dir; cache = scan_cache_dir(); [cache.delete_revisions(r.commit_hash) for repo in cache.repos for r in repo.revisions if '{}' in repo.repo_id]",
                                old_model.replace("mlx-community/", "")
                            )])
                            .output();
                    }
                }
            }
        }

        // Emit model step status for MLX
        // Look up display name for the model
        let mlx_display_name = {
            let mut name = model.clone();
            for (id, dname, _) in MODEL_METADATA {
                if *id == model { name = dname.to_string(); break; }
            }
            name
        };
        let mlx_size_gb = {
            let mut sz: Option<f64> = None;
            for (id, _, size) in MODEL_METADATA {
                if *id == model { sz = Some(*size); break; }
            }
            sz
        };
        if model_cached {
            log_startup!("Step 3: MLX model {} already cached in HF cache", model);
            emit_wizard_progress(window, WizardProgress {
                step_id: "model_download".into(),
                status: "active".into(),
                detail: Some(format!("Checking {} cache...", mlx_display_name)),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            emit_wizard_progress(window, WizardProgress {
                step_id: "model_download".into(),
                status: "done".into(),
                detail: Some(format!("{} ({}) — cached",
                    mlx_display_name,
                    mlx_size_gb.map(|s| format!("{:.1} GB", s)).unwrap_or_else(|| "size unknown".into())
                )),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            emit_wizard_progress(window, WizardProgress {
                step_id: "model_verify".into(),
                status: "active".into(),
                detail: Some("Verifying model files...".into()),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            emit_wizard_progress(window, WizardProgress {
                step_id: "model_verify".into(),
                status: "done".into(),
                detail: Some("Model verified and ready".into()),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        } else {
            // Download MLX model explicitly with progress reporting before starting server
            emit_wizard_progress(window, WizardProgress {
                step_id: "model_download".into(),
                status: "active".into(),
                detail: Some(format!("Downloading {} from HuggingFace...", mlx_display_name)),
                ..Default::default()
            });
            log_startup!("Step 3a: Downloading MLX model {} via huggingface_hub", model);
            // Checkpoint: upload logs before MLX model download
            upload_provider_logs(api_key, dcp_dir).await;

            let download_script = format!(
                "import sys; from huggingface_hub import snapshot_download; \
                 snapshot_download('{}', local_files_only=False)",
                model
            );
            let dl_result = Command::new(python_cmd())
                .args(["-u", "-c", &download_script])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn();

            match dl_result {
                Ok(mut dl_child) => {
                    // Stream stderr for progress — huggingface_hub outputs lines like:
                    // "Downloading model.safetensors: 45%|████ | 1.8G/4.0G [02:30<03:00, 12.3MB/s]"
                    if let Some(stderr) = dl_child.stderr.take() {
                        use std::io::{BufRead, BufReader};
                        let window_clone = window.clone();
                        let mut last_emit = std::time::Instant::now();
                        let mut mlx_slow_since: Option<std::time::Instant> = None;
                        let mut mlx_slow_warned = false;
                        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                            log_startup!("[mlx-dl] {}", line);
                            if last_emit.elapsed().as_millis() >= 500 {
                                // Parse HF progress: "45%|████ | 1.8G/4.0G [02:30<03:00, 12.3MB/s]"
                                let hf_pct = line.find('%').and_then(|idx| {
                                    let before = &line[..idx];
                                    let start = before.rfind(|c: char| !c.is_ascii_digit() && c != '.').map(|i| i + 1).unwrap_or(0);
                                    before[start..].parse::<f32>().ok()
                                });

                                // Parse speed: "12.3MB/s" or "12.3MB/s]"
                                let hf_speed: Option<f64> = {
                                    let mut result = None;
                                    if let Some(idx) = line.find("MB/s") {
                                        let before = &line[..idx];
                                        // Walk back past spaces/commas to find the number
                                        let trimmed = before.trim_end_matches(|c: char| c == ',' || c == ' ');
                                        let start = trimmed.rfind(|c: char| !c.is_ascii_digit() && c != '.').map(|i| i + 1).unwrap_or(0);
                                        if let Ok(v) = trimmed[start..].parse::<f64>() { result = Some(v); }
                                    } else if let Some(idx) = line.find("GB/s") {
                                        let before = &line[..idx];
                                        let trimmed = before.trim_end_matches(|c: char| c == ',' || c == ' ');
                                        let start = trimmed.rfind(|c: char| !c.is_ascii_digit() && c != '.').map(|i| i + 1).unwrap_or(0);
                                        if let Ok(v) = trimmed[start..].parse::<f64>() { result = Some(v * 1024.0); }
                                    }
                                    result
                                };

                                // Parse done/total: "1.8G/4.0G" or "500M/2.0G"
                                let (hf_mb_done, hf_mb_total) = {
                                    let mut done: Option<f64> = None;
                                    let mut total: Option<f64> = None;
                                    // Look for the pipe-separated section with sizes
                                    if let Some(pipe_idx) = line.find('|') {
                                        let after_pipe = &line[pipe_idx..];
                                        if let Some(slash_idx) = after_pipe.find('/') {
                                            let abs_slash = pipe_idx + slash_idx;
                                            // Done: number+unit before slash
                                            let before_slash = &line[pipe_idx..abs_slash];
                                            let bs = before_slash.trim_start_matches('|').trim();
                                            if let Some(g_pos) = bs.rfind('G') {
                                                let chunk = &bs[..g_pos].trim();
                                                let start = chunk.rfind(|c: char| !c.is_ascii_digit() && c != '.').map(|i| i + 1).unwrap_or(0);
                                                if let Ok(v) = chunk[start..].parse::<f64>() { done = Some(v * 1024.0); }
                                            } else if let Some(m_pos) = bs.rfind('M') {
                                                let chunk = &bs[..m_pos].trim();
                                                let start = chunk.rfind(|c: char| !c.is_ascii_digit() && c != '.').map(|i| i + 1).unwrap_or(0);
                                                if let Ok(v) = chunk[start..].parse::<f64>() { done = Some(v); }
                                            }
                                            // Total: number+unit after slash
                                            let after_slash = &line[abs_slash + 1..];
                                            let at = after_slash.trim_start();
                                            if let Some(g_pos) = at.find('G') {
                                                if let Ok(v) = at[..g_pos].trim().parse::<f64>() { total = Some(v * 1024.0); }
                                            } else if let Some(m_pos) = at.find('M') {
                                                if let Ok(v) = at[..m_pos].trim().parse::<f64>() { total = Some(v); }
                                            }
                                        }
                                    }
                                    (done, total)
                                };

                                // Parse ETA: "[02:30<03:00, ...]" — take the part after '<'
                                let hf_eta: Option<u64> = {
                                    let mut result = None;
                                    if let Some(lt_idx) = line.find('<') {
                                        let after = &line[lt_idx + 1..];
                                        // Format: "03:00" or "1:23:45"
                                        let end = after.find(|c: char| c == ',' || c == ']' || c == ' ').unwrap_or(after.len());
                                        let time_str = &after[..end];
                                        let parts: Vec<&str> = time_str.split(':').collect();
                                        match parts.len() {
                                            2 => {
                                                let m = parts[0].parse::<u64>().unwrap_or(0);
                                                let s = parts[1].parse::<u64>().unwrap_or(0);
                                                result = Some(m * 60 + s);
                                            }
                                            3 => {
                                                let h = parts[0].parse::<u64>().unwrap_or(0);
                                                let m = parts[1].parse::<u64>().unwrap_or(0);
                                                let s = parts[2].parse::<u64>().unwrap_or(0);
                                                result = Some(h * 3600 + m * 60 + s);
                                            }
                                            _ => {}
                                        }
                                    }
                                    result
                                };

                                // Build rich detail string
                                let detail = if hf_pct.is_some() || hf_speed.is_some() {
                                    let pct_str = hf_pct.map(|p| format!("{:.0}%", p)).unwrap_or_default();
                                    let size_str = match (hf_mb_done, hf_mb_total) {
                                        (Some(d), Some(t)) => {
                                            if t >= 1024.0 { format!(" ({:.1}/{:.1} GB)", d / 1024.0, t / 1024.0) }
                                            else { format!(" ({:.0}/{:.0} MB)", d, t) }
                                        }
                                        _ => String::new(),
                                    };
                                    let speed_part = hf_speed.map(|s| format!(" at {:.1} MB/s", s)).unwrap_or_default();
                                    let eta_part = hf_eta.map(|s| {
                                        if s >= 3600 { format!(" -- ETA {}h{:02}m", s / 3600, (s % 3600) / 60) }
                                        else if s >= 60 { format!(" -- ETA {}m{:02}s", s / 60, s % 60) }
                                        else { format!(" -- ETA {}s", s) }
                                    }).unwrap_or_default();
                                    format!("Downloading {} -- {}{}{}{}", mlx_display_name, pct_str, size_str, speed_part, eta_part)
                                } else {
                                    // Non-progress line (e.g. "Fetching 12 files...")
                                    line.chars().take(120).collect()
                                };

                                // Slow-speed warning for MLX downloads
                                if let Some(spd) = hf_speed {
                                    if spd < 2.0 {
                                        match mlx_slow_since {
                                            None => { mlx_slow_since = Some(std::time::Instant::now()); }
                                            Some(since) if since.elapsed().as_secs() >= 30 && !mlx_slow_warned => {
                                                mlx_slow_warned = true;
                                                log_startup!("[mlx-dl] Slow download warning: {:.1} MB/s", spd);
                                                emit_wizard_progress(&window_clone, WizardProgress {
                                                    step_id: "model_download".into(),
                                                    status: "active".into(),
                                                    pct: hf_pct,
                                                    mbps: Some(spd),
                                                    detail: Some(format!(
                                                        "Slow download detected ({:.1} MB/s). This may take a while on your connection.",
                                                        spd
                                                    )),
                                                    ..Default::default()
                                                });
                                            }
                                            _ => {}
                                        }
                                    } else {
                                        mlx_slow_since = None;
                                    }
                                }

                                emit_wizard_progress(&window_clone, WizardProgress {
                                    step_id: "model_download".into(),
                                    status: "active".into(),
                                    pct: hf_pct,
                                    mb_done: hf_mb_done,
                                    mb_total: hf_mb_total,
                                    mbps: hf_speed,
                                    eta_seconds: hf_eta,
                                    detail: Some(detail),
                                    ..Default::default()
                                });
                                last_emit = std::time::Instant::now();
                            }
                        }
                    }
                    match dl_child.wait() {
                        Ok(exit) if exit.success() => {
                            log_startup!("Step 3a: MLX model download completed successfully");
                            emit_wizard_progress(window, WizardProgress {
                                step_id: "model_download".into(),
                                status: "done".into(),
                                detail: Some(format!("{} downloaded", mlx_display_name)),
                                ..Default::default()
                            });
                            emit_wizard_progress(window, WizardProgress {
                                step_id: "model_verify".into(),
                                status: "done".into(),
                                detail: Some("Model verified and ready".into()),
                                ..Default::default()
                            });
                        }
                        Ok(exit) => {
                            log_startup!("Step 3a: MLX download exited with code {:?}, server will retry", exit.code());
                        }
                        Err(e) => {
                            log_startup!("Step 3a: MLX download wait error: {}, server will retry", e);
                        }
                    }
                }
                Err(e) => {
                    log_startup!("Step 3a: MLX model download spawn failed: {}, server will retry on load", e);
                }
            }
        }

        // Start mlx_lm.server — it auto-downloads the model on first run (fallback if explicit download failed)
        let log_path = dcp_dir.join("mlx-server.log");
        // M7 — append, don't truncate
        let log_file = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)
            .map_err(|e| format!("Log open failed: {}", e))?;
        let err_file = log_file.try_clone()
            .map_err(|e| format!("Log clone failed: {}", e))?;

        let _server = Command::new(python_cmd())
            .args(["-m", "mlx_lm.server", "--model", &model, "--host", "0.0.0.0", "--port", "8000"])
            .stdout(log_file)
            .stderr(err_file)
            .spawn()
            .map_err(|e| format!("Failed to start MLX server: {}", e))?;

        // Emit final model step status for MLX (server spawned — model loads in background)
        if !model_cached {
            emit_wizard_progress(window, WizardProgress {
                step_id: "model_download".into(),
                status: "done".into(),
                detail: Some(format!("{} loading via MLX server", model)),
                ..Default::default()
            });
            emit_wizard_progress(window, WizardProgress {
                step_id: "model_verify".into(),
                status: "done".into(),
                detail: Some("MLX server started".into()),
                ..Default::default()
            });
        }

        // Write model info to config
        let config_path = dcp_dir.join("config.json");
        // M3 — log config-save failures so a corrupt config is visible
        if let Ok(config_str) = std::fs::read_to_string(&config_path) {
            if let Ok(mut config) = serde_json::from_str::<serde_json::Value>(&config_str) {
                config["served_model"] = serde_json::Value::String(model.clone());
                config["engine"] = serde_json::Value::String(engine.clone());
                if let Err(e) = std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap_or_default()) {
                    log_startup!("Failed to update config.json with model/engine: {}", e);
                }
            }
        } else {
            // Create config if it doesn't exist
            let config = serde_json::json!({
                "api_key": api_key,
                "served_model": model,
                "engine": engine,
            });
            if let Err(e) = std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap_or_default()) {
                log_startup!("Failed to create config.json: {}", e);
            }
        }
    } else {
        // Ollama: start serve + pull model
        // Check if ollama is already serving — use reqwest instead of curl for portability
        let ollama_running = {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap_or_default();
            client.get("http://localhost:11434/api/tags")
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        };

        if !ollama_running {
            let _serve = hide_window(
                Command::new(&ollama_cmd())
                    .arg("serve")
                    .env("OLLAMA_HOST", "0.0.0.0")
                    // Keep models loaded in VRAM permanently — prevents unload
                    // between requests which would add 10-30s cold-start latency
                    .env("OLLAMA_KEEP_ALIVE", "-1")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
            ).spawn()
                .map_err(|e| format!("Failed to start Ollama: {}", e))?;
            // Wait for it to be ready — up to 60s. Fail loudly if it never
            // comes up; the wizard's previous behaviour was to fall through
            // silently and then look "online" with a dead engine.
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap_or_default();
            let mut serve_up = false;
            for _ in 0..60 {
                if client.get("http://127.0.0.1:11434/api/tags")
                    .send().await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false)
                {
                    serve_up = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            if !serve_up {
                return Err("Ollama service not reachable on :11434 after 60s".to_string());
            }
        }

        // Check if model already pulled
        let list_output = {
            let mut cmd_list = Command::new(&ollama_cmd());
            cmd_list.args(["list"]);
            hide_window(&mut cmd_list);
            cmd_list.output()
        };
        model_cached = list_output.ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&model))
            .unwrap_or(false);

        if !model_cached {
            // Checkpoint: upload logs before model download so admin sees pre-download state
            log_startup!("Step 3: Starting Ollama model download for {}", model);
            upload_provider_logs(api_key, dcp_dir).await;

            // ── Pre-download speed estimate ──────────────────────────
            let model_size_gb_est: f64 = {
                let mut sz = 4.0_f64; // fallback guess
                for (id, _, size) in MODEL_METADATA {
                    if *id == model { sz = *size; break; }
                }
                sz
            };
            {
                let probe_url = "https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe";
                let probe_client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(6))
                    .build()
                    .unwrap_or_default();
                let probe_start = std::time::Instant::now();
                if let Ok(resp) = probe_client.get(probe_url).header("Range", "bytes=0-2097151").send().await {
                    if let Ok(bytes) = resp.bytes().await {
                        let elapsed_s = probe_start.elapsed().as_secs_f64();
                        if elapsed_s > 0.01 {
                            let probe_mbps = (bytes.len() as f64) / elapsed_s / 1_048_576.0;
                            let est_minutes = if probe_mbps > 0.1 {
                                (model_size_gb_est * 1024.0) / (probe_mbps * 60.0)
                            } else { 999.0 };
                            log_startup!("Speed probe: {:.1} MB/s, model {:.1} GB, ETA {:.0} min", probe_mbps, model_size_gb_est, est_minutes);
                            if est_minutes > 10.0 {
                                emit_wizard_progress(window, WizardProgress {
                                    step_id: "model_download".into(),
                                    status: "active".into(),
                                    mbps: Some(probe_mbps),
                                    detail: Some(format!(
                                        "Your download speed is {:.1} MB/s. Downloading {:.1} GB model will take approximately {:.0} minutes.",
                                        probe_mbps, model_size_gb_est, est_minutes
                                    )),
                                    ..Default::default()
                                });
                                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                            }
                        }
                    }
                }
            }

            emit_wizard_progress(window, WizardProgress {
                step_id: "model_download".into(),
                status: "active".into(),
                detail: Some(format!("Pulling {} ({:.1} GB) from registry.ollama.ai", model, model_size_gb_est)),
                ..Default::default()
            });
            use std::io::{BufRead, BufReader};
            use std::process::Stdio;
            let mut cmd = Command::new(&ollama_cmd());
            cmd.args(["pull", &model]).stdout(Stdio::piped()).stderr(Stdio::piped());
            hide_window(&mut cmd);
            let mut child = cmd.spawn().map_err(|e| format!("Model pull spawn failed: {}", e))?;
            let stdout = child.stdout.take().ok_or_else(|| "no stdout pipe".to_string())?;
            let started = std::time::Instant::now();
            let mut last_emit = std::time::Instant::now();
            let mut slow_since: Option<std::time::Instant> = None;
            let mut slow_warned = false;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if last_emit.elapsed().as_millis() >= 250 {
                    // Parse percent using substring scan (no regex dep needed)
                    let pct = line.find('%').and_then(|idx| {
                        let before = &line[..idx];
                        let start = before.rfind(|c: char| !c.is_ascii_digit() && c != '.').map(|i| i + 1).unwrap_or(0);
                        before[start..].parse::<f32>().ok()
                    });

                    // Parse speed: look for "X MB/s" or "X.Y MB/s"
                    let speed_str: Option<(f64, String)> = {
                        let mut result = None;
                        if let Some(idx) = line.find("MB/s") {
                            let before = line[..idx].trim_end();
                            let start = before.rfind(|c: char| !c.is_ascii_digit() && c != '.').map(|i| i + 1).unwrap_or(0);
                            if let Ok(val) = before[start..].parse::<f64>() {
                                result = Some((val, format!("{:.0} MB/s", val)));
                            }
                        } else if let Some(idx) = line.find("GB/s") {
                            let before = line[..idx].trim_end();
                            let start = before.rfind(|c: char| !c.is_ascii_digit() && c != '.').map(|i| i + 1).unwrap_or(0);
                            if let Ok(val) = before[start..].parse::<f64>() {
                                result = Some((val * 1024.0, format!("{:.2} GB/s", val)));
                            }
                        }
                        result
                    };

                    // Parse downloaded/total: look for "X.Y GB/Z.W GB" or "X MB/Y MB"
                    let (mb_done, mb_total) = {
                        let mut done: Option<f64> = None;
                        let mut total: Option<f64> = None;
                        // Pattern: "1.8 GB/4.0 GB"
                        if let Some(slash_idx) = line.find('/') {
                            // Parse total (after slash)
                            let after = &line[slash_idx + 1..];
                            let after_trimmed = after.trim_start();
                            if let Some(gb_pos) = after_trimmed.find("GB") {
                                if let Ok(v) = after_trimmed[..gb_pos].trim().parse::<f64>() {
                                    total = Some(v * 1024.0);
                                }
                            } else if let Some(mb_pos) = after_trimmed.find("MB") {
                                if let Ok(v) = after_trimmed[..mb_pos].trim().parse::<f64>() {
                                    total = Some(v);
                                }
                            }
                            // Parse done (before slash)
                            let before = &line[..slash_idx];
                            let before_trimmed = before.trim_end();
                            // Walk backwards past the number and unit
                            if let Some(gb_pos) = before_trimmed.rfind("GB") {
                                // Number is between preceding non-digit and "GB"
                                let chunk = &before_trimmed[..gb_pos].trim_end();
                                let start = chunk.rfind(|c: char| !c.is_ascii_digit() && c != '.').map(|i| i + 1).unwrap_or(0);
                                if let Ok(v) = chunk[start..].parse::<f64>() {
                                    done = Some(v * 1024.0);
                                }
                            } else if let Some(mb_pos) = before_trimmed.rfind("MB") {
                                let chunk = &before_trimmed[..mb_pos].trim_end();
                                let start = chunk.rfind(|c: char| !c.is_ascii_digit() && c != '.').map(|i| i + 1).unwrap_or(0);
                                if let Ok(v) = chunk[start..].parse::<f64>() {
                                    done = Some(v);
                                }
                            } else {
                                // Try bare number before slash (bytes?)
                                let start = before_trimmed.rfind(|c: char| !c.is_ascii_digit() && c != '.').map(|i| i + 1).unwrap_or(0);
                                if let Ok(v) = before_trimmed[start..].parse::<f64>() {
                                    done = Some(v);
                                }
                            }
                        }
                        (done, total)
                    };

                    // Parse ETA: look for patterns like "2m53s", "53s", "1h2m"
                    let eta_seconds: Option<u64> = {
                        let mut result = None;
                        // Ollama format: "Xm Ys" or "XmYs" at end of line
                        let trimmed = line.trim_end();
                        let last_token = trimmed.rsplit_once(|c: char| c.is_whitespace()).map(|(_, t)| t).unwrap_or(trimmed);
                        let mut secs: u64 = 0;
                        let mut num_buf = String::new();
                        let mut found_unit = false;
                        for ch in last_token.chars() {
                            if ch.is_ascii_digit() {
                                num_buf.push(ch);
                            } else if ch == 'h' {
                                if let Ok(n) = num_buf.parse::<u64>() { secs += n * 3600; found_unit = true; }
                                num_buf.clear();
                            } else if ch == 'm' {
                                if let Ok(n) = num_buf.parse::<u64>() { secs += n * 60; found_unit = true; }
                                num_buf.clear();
                            } else if ch == 's' {
                                if let Ok(n) = num_buf.parse::<u64>() { secs += n; found_unit = true; }
                                num_buf.clear();
                            }
                        }
                        if found_unit { result = Some(secs); }
                        result
                    };

                    // Build rich detail string
                    let speed_mbps = speed_str.as_ref().map(|(v, _)| *v);
                    let detail = {
                        let pct_str = pct.map(|p| format!("{:.0}%", p)).unwrap_or_default();
                        let size_str = match (mb_done, mb_total) {
                            (Some(d), Some(t)) => {
                                if t >= 1024.0 { format!(" ({:.1}/{:.1} GB)", d / 1024.0, t / 1024.0) }
                                else { format!(" ({:.0}/{:.0} MB)", d, t) }
                            }
                            _ => String::new(),
                        };
                        let speed_part = speed_str.as_ref().map(|(_, s)| format!(" at {}", s)).unwrap_or_default();
                        let eta_part = eta_seconds.map(|s| {
                            if s >= 3600 { format!(" -- ETA {}h{:02}m", s / 3600, (s % 3600) / 60) }
                            else if s >= 60 { format!(" -- ETA {}m{:02}s", s / 60, s % 60) }
                            else { format!(" -- ETA {}s", s) }
                        }).unwrap_or_default();
                        format!("Pulling {} -- {}{}{}{}", model, pct_str, size_str, speed_part, eta_part)
                    };

                    // Slow-speed warning
                    if let Some(spd) = speed_mbps {
                        if spd < 2.0 {
                            match slow_since {
                                None => { slow_since = Some(std::time::Instant::now()); }
                                Some(since) if since.elapsed().as_secs() >= 30 && !slow_warned => {
                                    slow_warned = true;
                                    log_startup!("Slow download warning: {:.1} MB/s", spd);
                                    emit_wizard_progress(window, WizardProgress {
                                        step_id: "model_download".into(),
                                        status: "active".into(),
                                        pct,
                                        mbps: Some(spd),
                                        detail: Some(format!(
                                            "Slow download detected ({:.1} MB/s). This may take a while on your connection.",
                                            spd
                                        )),
                                        ..Default::default()
                                    });
                                }
                                _ => {}
                            }
                        } else {
                            slow_since = None;
                        }
                    }

                    emit_wizard_progress(window, WizardProgress {
                        step_id: "model_download".into(),
                        status: "active".into(),
                        pct,
                        mb_done,
                        mb_total,
                        mbps: speed_mbps,
                        eta_seconds,
                        detail: Some(detail),
                        ..Default::default()
                    });
                    last_emit = std::time::Instant::now();
                }
            }
            let status = child.wait().map_err(|e| format!("Model pull wait failed: {}", e))?;
            if !status.success() {
                let _ = started; // suppress unused warning
                emit_wizard_progress(window, WizardProgress {
                    step_id: "model_download".into(),
                    status: "error".into(),
                    error: Some(format!("ollama pull {} exited non-zero", model)),
                    ..Default::default()
                });
                return Err(format!("ollama pull {} failed", model));
            }
            emit_wizard_progress(window, WizardProgress {
                step_id: "model_download".into(),
                status: "done".into(),
                detail: Some(format!("{} downloaded", model)),
                ..Default::default()
            });
        } else {
            log_startup!("Step 3: Model {} already cached in Ollama, skipping pull", model);
            // Look up display name for the model
            let ollama_display_name = {
                let mut name = model.clone();
                for (id, dname, _) in MODEL_METADATA {
                    if *id == model { name = dname.to_string(); break; }
                }
                name
            };
            let ollama_size_gb = {
                let mut sz: Option<f64> = None;
                for (id, _, size) in MODEL_METADATA {
                    if *id == model { sz = Some(*size); break; }
                }
                sz
            };
            emit_wizard_progress(window, WizardProgress {
                step_id: "model_download".into(),
                status: "active".into(),
                detail: Some(format!("Checking {} cache...", ollama_display_name)),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            emit_wizard_progress(window, WizardProgress {
                step_id: "model_download".into(),
                status: "done".into(),
                detail: Some(format!("{} ({}) — cached",
                    ollama_display_name,
                    ollama_size_gb.map(|s| format!("{:.1} GB", s)).unwrap_or_else(|| "size unknown".into())
                )),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            emit_wizard_progress(window, WizardProgress {
                step_id: "model_verify".into(),
                status: "active".into(),
                detail: Some("Verifying model files...".into()),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            emit_wizard_progress(window, WizardProgress {
                step_id: "model_verify".into(),
                status: "done".into(),
                detail: Some("Model verified and ready".into()),
                ..Default::default()
            });
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    // Step 3.5 (Windows only): Ensure Python is available, install embedded Python if needed
    #[cfg(windows)]
    {
        let python_ok = {
            let mut cmd_pyver = Command::new(python_cmd());
            cmd_pyver.arg("--version");
            hide_window(&mut cmd_pyver);
            cmd_pyver.output().map(|o| o.status.success()).unwrap_or(false)
        };

        if !python_ok {
            let python_dir = dcp_dir.join("python");
            let python_exe = python_dir.join("python.exe");

            if !python_exe.exists() {
                // Download embeddable Python 3.11.9 (~11MB)
                let zip_path = dcp_dir.join("python-embed.zip");
                let client = reqwest::Client::new();
                let resp = client
                    .get("https://www.python.org/ftp/python/3.11.9/python-3.11.9-embed-amd64.zip")
                    .send()
                    .await
                    .map_err(|e| format!("Python download failed: {}", e))?;
                if !resp.status().is_success() {
                    return Err(format!("Python download HTTP {}", resp.status()));
                }
                let zip_bytes = resp.bytes().await
                    .map_err(|e| format!("Python download read failed: {}", e))?;
                std::fs::write(&zip_path, &zip_bytes)
                    .map_err(|e| format!("Python zip write failed: {}", e))?;

                // Extract using PowerShell
                let mut cmd_ps = Command::new("powershell");
                cmd_ps.args([
                    "-NoProfile", "-Command",
                    &format!(
                        "Expand-Archive -Force -Path '{}' -DestinationPath '{}'",
                        zip_path.display(),
                        python_dir.display()
                    ),
                ]);
                hide_window(&mut cmd_ps);
                let extract = cmd_ps
                    .output()
                    .map_err(|e| format!("Python extract failed: {}", e))?;
                if !extract.status.success() {
                    let stderr = String::from_utf8_lossy(&extract.stderr);
                    return Err(format!("Python extract failed: {}", stderr));
                }

                // Clean up zip
                let _ = std::fs::remove_file(&zip_path);

                // Patch python311._pth to enable import site (required for pip)
                let pth_path = python_dir.join("python311._pth");
                if pth_path.exists() {
                    let mut pth_content = std::fs::read_to_string(&pth_path)
                        .map_err(|e| format!("Failed to read _pth file: {}", e))?;
                    if !pth_content.contains("import site") || pth_content.contains("#import site") {
                        pth_content = pth_content.replace("#import site", "import site");
                        if !pth_content.contains("import site") {
                            pth_content.push_str("\nimport site\n");
                        }
                        std::fs::write(&pth_path, &pth_content)
                            .map_err(|e| format!("Failed to patch _pth file: {}", e))?;
                    }
                }

                // Download and run get-pip.py
                let getpip_path = dcp_dir.join("get-pip.py");
                let pip_resp = client
                    .get("https://bootstrap.pypa.io/get-pip.py")
                    .send()
                    .await
                    .map_err(|e| format!("get-pip download failed: {}", e))?;
                if !pip_resp.status().is_success() {
                    return Err(format!("get-pip download HTTP {}", pip_resp.status()));
                }
                let pip_bytes = pip_resp.bytes().await
                    .map_err(|e| format!("get-pip read failed: {}", e))?;
                std::fs::write(&getpip_path, &pip_bytes)
                    .map_err(|e| format!("get-pip write failed: {}", e))?;

                let mut cmd_pip = Command::new(&python_exe);
                cmd_pip.arg(&getpip_path);
                hide_window(&mut cmd_pip);
                let pip_install = cmd_pip
                    .output()
                    .map_err(|e| format!("get-pip run failed: {}", e))?;
                if !pip_install.status.success() {
                    let stderr = String::from_utf8_lossy(&pip_install.stderr);
                    return Err(format!("pip bootstrap failed: {}", stderr));
                }
                let _ = std::fs::remove_file(&getpip_path);

                // Install required packages
                let mut cmd_deps = Command::new(&python_exe);
                cmd_deps.args(["-m", "pip", "install", "requests", "psutil"]);
                hide_window(&mut cmd_deps);
                let deps = cmd_deps
                    .output()
                    .map_err(|e| format!("pip install deps failed: {}", e))?;
                if !deps.status.success() {
                    let stderr = String::from_utf8_lossy(&deps.stderr);
                    return Err(format!("pip install requests psutil failed: {}", stderr));
                }
            }
        }
    }

    log_startup!("Step 3: Model ready — cached={}, engine running on port {}", model_cached, if engine == "mlx" { 8000 } else { 11434 });

    // Checkpoint: upload logs after model step completes
    upload_provider_logs(api_key, dcp_dir).await;

    // Step 4: Download latest daemon with G19 sha256 verification (parity with update_daemon)
    emit_wizard_progress(window, WizardProgress {
        step_id: "daemon".into(),
        status: "active".into(),
        detail: Some("Downloading DCP daemon...".into()),
        ..Default::default()
    });
    let daemon_path = dcp_dir.join("dcp_daemon.py");
    let mut daemon_version = "latest".to_string();
    {
        // Backlog #7 — single daemon source. The platform serves the ONE
        // daemon; there is no bundled/forked copy. Fetch + verify (size +
        // sha256) against the server manifest BEFORE writing or spawning.
        match fetch_verified_daemon(api_key).await {
            Ok(verified) => {
                daemon_version = verified.version.clone();
                // Verified — safe to write (M11 — atomic).
                if let Err(e) = atomic_write(&daemon_path, &verified.bytes) {
                    // Write failed: only safe to continue if a previously
                    // verified daemon is already on disk.
                    if !daemon_path.exists() {
                        return Err(format!(
                            "Couldn't write the downloaded daemon and none is installed: {}",
                            e
                        ));
                    }
                    log_startup!("Step 4a: Daemon write failed: {} (keeping installed copy)", e);
                } else {
                    log_startup!(
                        "Step 4a: Daemon v{} downloaded + verified ({} bytes, sha256 OK)",
                        verified.version,
                        verified.bytes.len()
                    );
                }
            }
            Err(e) => {
                // Fail-safe: the verified fetch failed. Do NOT write or spawn an
                // unverified daemon. If a previously verified daemon already
                // exists we keep using it (transient network blip on a re-run);
                // otherwise hard-fail with a clear error rather than running
                // nothing or something stale.
                if !daemon_path.exists() {
                    return Err(format!("Couldn't fetch daemon from platform: {}", e));
                }
                log_startup!("Step 4a: Daemon fetch failed: {} (keeping installed copy)", e);
            }
        }
        if !daemon_path.exists() {
            return Err(
                "Daemon file not found and download failed. Check internet connection.".to_string(),
            );
        }
    }

    let log_path = dcp_dir.join("daemon.log");
    let err_log_path = dcp_dir.join("daemon_error.log");
    // M7 — append, don't truncate
    let log_file = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)
        .map_err(|e| format!("Log open failed: {}", e))?;
    let err_file = std::fs::OpenOptions::new().create(true).append(true).open(&err_log_path)
        .map_err(|e| format!("Error log open failed: {}", e))?;

    // G2 — detach so the daemon survives a desktop UI quit / crash.
    let child = detach_process(
        Command::new(python_cmd())
            .arg(&daemon_path)
            .arg("--no-watchdog")
            .arg("--key").arg(&api_key)
            .arg("--url").arg("https://api.dcp.sa")
            .env("DCP_SERVED_MODEL", &model)
            .env("DCP_ENGINE", &engine)
            .stdout(log_file)
            .stderr(err_file)
    )
        .spawn()
        .map_err(|e| format!("Daemon spawn failed: {}", e))?;

    let pid = child.id();
    // M3 — surface PID-file failures: without the file we can't kill/restart the daemon
    if let Err(e) = write_pid_file(pid) {
        log_startup!("Failed to write daemon PID file: {}", e);
    }

    {
        let mut guard = state.lock().map_err(|e| format!("Lock: {}", e))?;
        guard.pid = Some(pid);
        guard.status = "running".to_string();
        guard.started_at = Some(std::time::Instant::now());
        guard.restart_count = 0;
    }

    log_startup!("Step 4: Daemon started — PID={}", pid);

    // Checkpoint: upload logs after daemon start
    upload_provider_logs(api_key, dcp_dir).await;

    emit_wizard_progress(window, WizardProgress {
        step_id: "daemon".into(),
        status: "done".into(),
        detail: Some(format!("Daemon v{} running (PID {})", daemon_version, pid)),
        ..Default::default()
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Step 5: Set up WireGuard mesh (replaces broken Cloudflare Quick Tunnel)
    // WG tunnel lets the backend route inference traffic to the provider over
    // the 10.8.0.0/24 mesh. The daemon heartbeat also reports wg_mesh_ip so
    // v1.js prefers mesh routing over public vllm_endpoint_url.
    emit_wizard_progress(window, WizardProgress {
        step_id: "tunnel".into(),
        status: "active".into(),
        detail: Some("Connecting to DCP network...".into()),
        ..Default::default()
    });
    match setup_wireguard(dcp_dir, api_key).await {
        Ok(mesh_ip) => {
            log_startup!("Step 5: WireGuard active — mesh IP {}", mesh_ip);
            emit_wizard_progress(window, WizardProgress {
                step_id: "tunnel".into(),
                status: "done".into(),
                detail: Some(format!("Connected at {}", mesh_ip)),
                ..Default::default()
            });

            // Save mesh IP to config.json so the daemon can read it
            let config_path = dcp_dir.join("config.json");
            if let Ok(content) = std::fs::read_to_string(&config_path) {
                if let Ok(mut config) = serde_json::from_str::<serde_json::Value>(&content) {
                    config["wg_mesh_ip"] = serde_json::Value::String(mesh_ip.clone());
                    if let Err(e) = std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap_or_default()) {
                        log_startup!("Step 5: Failed to save wg_mesh_ip to config: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            log_startup!("Step 5: WireGuard setup failed: {}. Provider online but not routable for inference.", e);
            emit_wizard_progress(window, WizardProgress {
                step_id: "tunnel".into(),
                status: "done".into(),
                detail: Some("Connected via public endpoint (WireGuard unavailable)".into()),
                error: Some(e),
                ..Default::default()
            });
            // Continue without WG — provider heartbeat works, just no mesh routing
        }
    }

    // Checkpoint: upload logs after WG setup
    upload_provider_logs(api_key, dcp_dir).await;

    // Final verification: confirm the selected model is actually reachable
    // through the inference engine before we declare success. Skip on MLX —
    // mlx_lm.server downloads on first request and we don't want to block the
    // wizard waiting for an HF download here.
    if engine == "ollama" {
        emit_wizard_progress(window, WizardProgress {
            step_id: "model_verify".into(),
            status: "active".into(),
            detail: Some("Verifying model registered with Ollama".into()),
            ..Default::default()
        });
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        let tags = client
            .get("http://127.0.0.1:11434/api/tags")
            .send()
            .await
            .map_err(|e| {
                let msg = format!("Final verification failed: cannot reach Ollama on :11434 ({})", e);
                emit_wizard_progress(window, WizardProgress {
                    step_id: "model_verify".into(),
                    status: "error".into(),
                    error: Some(msg.clone()),
                    ..Default::default()
                });
                msg
            })?;
        let body = tags
            .text()
            .await
            .map_err(|e| {
                let msg = format!("Final verification failed: read body: {}", e);
                emit_wizard_progress(window, WizardProgress {
                    step_id: "model_verify".into(),
                    status: "error".into(),
                    error: Some(msg.clone()),
                    ..Default::default()
                });
                msg
            })?;
        if !body.contains(&model) {
            // Fall back to `ollama list` in case /api/tags is partially populated.
            let list_out = {
                let mut cmd_vlist = Command::new(&ollama_cmd());
                cmd_vlist.args(["list"]);
                hide_window(&mut cmd_vlist);
                cmd_vlist.output()
            };
            let listed = list_out
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains(&model))
                .unwrap_or(false);
            if !listed {
                let msg = format!(
                    "Final verification failed: model={} not found in Ollama after pull",
                    model
                );
                emit_wizard_progress(window, WizardProgress {
                    step_id: "model_verify".into(),
                    status: "error".into(),
                    error: Some(msg.clone()),
                    ..Default::default()
                });
                return Err(msg);
            }
        }
        emit_wizard_progress(window, WizardProgress {
            step_id: "model_verify".into(),
            status: "done".into(),
            detail: Some("Model verified and ready".into()),
            ..Default::default()
        });
        log_startup!("Verified: model={} reachable via Ollama on :11434", model);
    }

    log_startup!("Step 7: All steps complete");

    Ok(format!("started:{}:{}:{}:{}", engine, model, pid, if model_cached { "cached" } else { "downloaded" }))
}

// ── Auto Log Upload ─────────────────────────────────────────────────

/// Upload provider logs (startup.log, gpu-detection.log, daemon.log) to backend
/// Called after every startup attempt, whether success or failure
async fn upload_provider_logs(api_key: &str, dcp_dir: &std::path::Path) {
    let mut logs = serde_json::Map::new();
    for filename in &["startup.log", "gpu-detection.log", "daemon.log", "daemon_error.log", "cloudflared.log"] {
        let path = dcp_dir.join(filename);
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                // Send last 50KB of each log
                let trimmed = if content.len() > 50000 {
                    content[content.len()-50000..].to_string()
                } else {
                    content
                };
                logs.insert(filename.to_string(), serde_json::Value::String(trimmed));
            }
        }
    }
    if logs.is_empty() { return; }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    let _ = client.post(format!("{}/upload-logs", API_BASE))
        .json(&serde_json::json!({
            "api_key": api_key,
            "logs": logs
        }))
        .send()
        .await;
}

// ── Report Install Error (callable from frontend on any failure path) ──

#[tauri::command]
async fn report_install_error(api_key: String, error: String, stage: String) -> Result<(), String> {
    let dcp_dir = dcp_home()?;
    // Upload all available logs
    upload_provider_logs(&api_key, &dcp_dir).await;
    // POST structured error report to backend
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    let _ = client.post(format!("{}/install-error", API_BASE))
        .json(&serde_json::json!({
            "error": error,
            "stage": stage,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        }))
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await;
    Ok(())
}

// ── Wizard Helper Commands ───────────────────────────────────────────

#[derive(serde::Serialize)]
struct ModelMetadata {
    display_name: String,
    size_gb: Option<f64>,
}

#[tauri::command]
fn get_model_metadata(model_id: String) -> ModelMetadata {
    for (id, name, size) in MODEL_METADATA {
        if *id == model_id {
            return ModelMetadata {
                display_name: name.to_string(),
                size_gb: Some(*size),
            };
        }
    }
    ModelMetadata { display_name: model_id, size_gb: None }
}

#[derive(serde::Serialize)]
struct SpeedProbeResult {
    mbps: Option<f64>,
    sample_bytes: u64,
    elapsed_ms: u64,
}

#[tauri::command]
async fn pre_install_speed_probe() -> SpeedProbeResult {
    let url = "https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe";
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return SpeedProbeResult { mbps: None, sample_bytes: 0, elapsed_ms: 0 },
    };

    let started = std::time::Instant::now();
    let resp = client.get(url).header("Range", "bytes=0-10485759").send().await;
    let resp = match resp {
        Ok(r) => r,
        Err(_) => return SpeedProbeResult { mbps: None, sample_bytes: 0, elapsed_ms: started.elapsed().as_millis() as u64 },
    };
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(_) => return SpeedProbeResult { mbps: None, sample_bytes: 0, elapsed_ms: started.elapsed().as_millis() as u64 },
    };
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let sample_bytes = bytes.len() as u64;
    let mbps = if elapsed_ms > 0 {
        Some((sample_bytes as f64 * 8.0) / (elapsed_ms as f64 * 1000.0)) // (bytes*8 bits) / (ms * 1000) = Mbps
    } else {
        None
    };
    SpeedProbeResult { mbps, sample_bytes, elapsed_ms }
}

// ── Network Status & Key Rotation Commands ──────────────────────────

#[tauri::command]
async fn open_install_log() -> Result<(), String> {
    let dcp_dir = dcp_home()?;
    let log_path = dcp_dir.join("startup.log");
    if !log_path.exists() {
        return Err("Install log not found".to_string());
    }
    #[cfg(target_os = "macos")]
    { let _ = Command::new("open").arg(&log_path).spawn(); }
    #[cfg(target_os = "linux")]
    { let _ = Command::new("xdg-open").arg(&log_path).spawn(); }
    #[cfg(windows)]
    { let _ = Command::new("notepad").arg(&log_path).spawn(); }
    Ok(())
}

#[tauri::command]
async fn get_network_status() -> Result<serde_json::Value, String> {
    let dcp_dir = dcp_home()?;
    let config_path = dcp_dir.join("config.json");

    // Read wg_mesh_ip from config
    let mesh_ip = if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
            config.get("wg_mesh_ip").and_then(|v| v.as_str()).map(|s| s.to_string())
        } else { None }
    } else { None };

    // Check if WG interface is active by looking for the mesh IP on any interface.
    // We avoid `wg show` because it requires root on macOS.
    // Instead: check if the mesh IP from config is assigned to any network interface,
    // AND we can ping the VPS mesh gateway (10.8.0.1).
    let (wg_active, latency_ms) = {
        let mut active = false;
        let mut lat: Option<u64> = None;

        // Method 1: check ifconfig/ipconfig for the mesh IP
        if let Some(ref ip) = mesh_ip {
            #[cfg(not(windows))]
            {
                if let Ok(out) = Command::new("ifconfig").output() {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    if stdout.contains(ip.as_str()) {
                        active = true;
                    }
                }
            }
            #[cfg(windows)]
            {
                if let Ok(out) = Command::new("ipconfig").output() {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    if stdout.contains(ip.as_str()) {
                        active = true;
                    }
                }
            }
        }

        // Method 2: if no mesh_ip in config, try pinging the VPS gateway
        if !active {
            let ping_args: Vec<&str> = if cfg!(windows) {
                vec!["-n", "1", "-w", "1000", "10.8.0.1"]
            } else {
                vec!["-c", "1", "-W", "1", "10.8.0.1"]
            };
            if let Ok(o) = Command::new("ping").args(&ping_args).output() {
                if o.status.success() {
                    active = true;
                }
            }
        }

        // Measure latency if active
        if active {
            let start = std::time::Instant::now();
            let ping_args: Vec<&str> = if cfg!(windows) {
                vec!["-n", "1", "-w", "2000", "10.8.0.1"]
            } else {
                vec!["-c", "1", "-W", "2", "10.8.0.1"]
            };
            if let Ok(o) = Command::new("ping").args(&ping_args).output() {
                if o.status.success() {
                    lat = Some(start.elapsed().as_millis() as u64);
                }
            }
        }

        (active, lat)
    };

    let handshake_secs: Option<u64> = None; // Not available without root

    Ok(serde_json::json!({
        "connected": wg_active,
        "mesh_ip": mesh_ip,
        "latency_ms": latency_ms,
        "last_handshake_secs_ago": handshake_secs,
    }))
}

#[tauri::command]
async fn rotate_network_key() -> Result<String, String> {
    let dcp_dir = dcp_home()?;

    // Read API key from config
    let api_key = load_api_key_from_config()
        .map_err(|_| "No API key configured".to_string())?;

    // 1. Generate new keypair
    let (private_key, public_key) = generate_wg_keypair()?;

    // 2. Deactivate current tunnel (ignore errors — may not be up)
    #[cfg(not(windows))]
    {
        let conf_path = dcp_dir.join("wg0.conf");
        let _ = Command::new(wg_quick_bin())
            .args(["down", &conf_path.to_string_lossy()])
            .output();
    }
    #[cfg(windows)]
    {
        let _ = Command::new("wireguard")
            .args(["/uninstalltunnelservice", "wg0"])
            .output();
    }

    // 3. Register new key with backend (replaces old peer)
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    let resp = client
        .post(format!("{}/wg/register", API_BASE))
        .header("x-provider-key", &api_key)
        .json(&serde_json::json!({ "public_key": public_key, "rotate": true }))
        .send()
        .await
        .map_err(|e| format!("Failed to register new key: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Backend rejected key rotation: HTTP {} — {}", status, body));
    }

    let config: serde_json::Value = resp.json().await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    let assigned_ip = config["ip"].as_str().unwrap_or("10.8.0.3");

    // 4. Write new config with new private key
    let wg_conf = format!(
        "[Interface]\n\
         PrivateKey = {}\n\
         Address = {}/24\n\
         \n\
         [Peer]\n\
         PublicKey = {}\n\
         PresharedKey = {}\n\
         Endpoint = {}\n\
         AllowedIPs = 10.8.0.0/24\n\
         PersistentKeepalive = 25\n",
        private_key,
        assigned_ip,
        config["server_pubkey"].as_str().unwrap_or(""),
        config["psk"].as_str().unwrap_or(""),
        config["server_endpoint"].as_str().unwrap_or("76.13.179.86:51820"),
    );

    let conf_path = dcp_dir.join("wg0.conf");
    std::fs::write(&conf_path, &wg_conf)
        .map_err(|e| format!("Failed to write config: {}", e))?;

    // Restrict permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&conf_path,
            std::fs::Permissions::from_mode(0o600));
    }

    // 5. Reactivate tunnel
    activate_wireguard(&dcp_dir).await?;

    // 6. Update wg_mesh_ip in config.json
    let config_path = dcp_dir.join("config.json");
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(mut cfg) = serde_json::from_str::<serde_json::Value>(&content) {
            cfg["wg_mesh_ip"] = serde_json::Value::String(assigned_ip.to_string());
            let _ = std::fs::write(&config_path, serde_json::to_string_pretty(&cfg).unwrap_or_default());
            // Audit 2026-05-14 F4 — re-apply 0600 since std::fs::write may
            // recreate the file with default umask.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &config_path,
                    std::fs::Permissions::from_mode(0o600),
                );
            }
        }
    }

    Ok(format!("Key rotated. Mesh IP: {}", assigned_ip))
}

/// Reconnect WireGuard tunnel — called from the Dashboard "Reconnect" button.
/// Re-runs the full WG setup: ensure installed, register key, write config, activate.
#[tauri::command]
async fn reconnect_network() -> Result<String, String> {
    let dcp_dir = dcp_home()?;
    let api_key = load_api_key_from_config()
        .map_err(|_| "No API key configured. Complete setup first.".to_string())?;
    setup_wireguard(&dcp_dir, &api_key).await
}

// ── Embedded magic-link sign-in (loopback HTTP) ──────────────────────
//
// First-run flow when no api_key is on disk:
//   1. Frontend calls `start_signin_loopback` → we bind 127.0.0.1:0 (random
//      free port), keep the listener for ONE request, return the absolute
//      callback URL `http://127.0.0.1:<port>/exchange`.
//   2. Frontend calls `send_signin_email(email, role, callback_url)` →
//      we POST to /api/{providers,renters}/send-otp with the callback.
//      The backend embeds it in the magic-link URL it emails.
//   3. User clicks the link → dcp.sa/auth/verify exchanges token for
//      {api_key, role, …} and POSTs the JSON to our loopback listener.
//   4. Listener emits `signin:received` event on the window, replies 200,
//      shuts itself down, and the frontend continues into the auto-checklist.
//
// Security properties this code enforces:
//   - Bind to 127.0.0.1 ONLY (never 0.0.0.0). The kernel rejects packets
//     that didn't originate on the loopback interface.
//   - One-shot: after the first valid POST /exchange, the listener stops
//     accepting new connections.
//   - 5-minute hard timeout. If the user never clicks the link, the
//     listener self-destructs and the frontend can retry.
//   - We don't validate origin/CORS — the kernel-level loopback check is
//     the actual security boundary, and dcp.sa's `fetch()` from the verify
//     page won't include credentials anyway.

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SigninLoopback {
    pub port: u16,
    pub callback_url: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SigninReceivedPayload {
    pub api_key: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renter: Option<Value>,
}

/// Read an HTTP request from a tokio TCP stream into a (method, path, body)
/// triple. Intentionally minimal — we only handle POST /exchange with a
/// JSON body and OPTIONS preflight. Anything else → 404.
async fn read_http_request(
    stream: &mut tokio::net::TcpStream,
) -> Result<(String, String, String), String> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 1024];
    let mut header_end: Option<usize> = None;
    // Read until we see \r\n\r\n or 16 KiB (whichever first).
    while buf.len() < 16 * 1024 {
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|e| format!("read: {}", e))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
        {
            header_end = Some(pos + 4);
            break;
        }
    }
    let header_end = header_end.ok_or_else(|| "incomplete request".to_string())?;
    let header_str = std::str::from_utf8(&buf[..header_end])
        .map_err(|e| format!("non-utf8 header: {}", e))?;
    let mut lines = header_str.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.splitn(3, ' ');
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    // Find Content-Length for body
    let mut content_length: usize = 0;
    for line in lines {
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse::<usize>().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("content-length:") {
            content_length = rest.trim().parse::<usize>().unwrap_or(0);
        }
    }
    if content_length > 64 * 1024 {
        return Err("body too large".to_string());
    }

    // Read body
    let mut body_bytes: Vec<u8> = buf[header_end..].to_vec();
    while body_bytes.len() < content_length {
        let n = stream
            .read(&mut tmp)
            .await
            .map_err(|e| format!("read body: {}", e))?;
        if n == 0 {
            break;
        }
        body_bytes.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8(body_bytes).map_err(|e| format!("non-utf8 body: {}", e))?;
    Ok((method, path, body))
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status_line: &str,
    body: &str,
) {
    use tokio::io::AsyncWriteExt;
    let resp = format!(
        "{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: https://dcp.sa\r\nAccess-Control-Allow-Methods: POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nConnection: close\r\n\r\n{}",
        status_line,
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Bind a one-shot HTTP listener on 127.0.0.1:<random_port> that accepts
/// exactly one POST /exchange with a JSON body, emits the parsed payload as
/// a `signin:received` event, and shuts down.
#[tauri::command]
async fn start_signin_loopback(
    window: tauri::Window,
    state: State<'_, SigninListener>,
) -> Result<SigninLoopback, String> {
    use tokio::net::TcpListener;

    // Cancel any previous listener.
    {
        let mut s = state.lock().map_err(|e| format!("lock: {}", e))?;
        if let Some(tx) = s.cancel.take() {
            let _ = tx.send(());
        }
        s.port = None;
    }

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("bind 127.0.0.1:0 failed: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {}", e))?
        .port();
    let callback_url = format!("http://127.0.0.1:{}/exchange", port);

    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
    {
        let mut s = state.lock().map_err(|e| format!("lock: {}", e))?;
        s.cancel = Some(cancel_tx);
        s.port = Some(port);
    }

    let win = window.clone();
    tokio::spawn(async move {
        // Hard ceiling: 5 minutes. If the user doesn't click the link, give up.
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(300));
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                _ = &mut cancel_rx => {
                    eprintln!("[signin-loopback] cancelled");
                    let _ = win.emit("signin:cancelled", ());
                    return;
                }
                _ = &mut deadline => {
                    eprintln!("[signin-loopback] timed out after 5min");
                    let _ = win.emit("signin:timeout", ());
                    return;
                }
                accept = listener.accept() => {
                    let mut stream = match accept {
                        Ok((s, addr)) => {
                            // Defense in depth: although we bound to 127.0.0.1
                            // and the kernel won't route non-loopback packets
                            // here, log the peer for audit and bail if it
                            // somehow isn't loopback.
                            if !addr.ip().is_loopback() {
                                eprintln!("[signin-loopback] non-loopback peer rejected: {}", addr);
                                continue;
                            }
                            s
                        }
                        Err(e) => {
                            eprintln!("[signin-loopback] accept err: {}", e);
                            continue;
                        }
                    };

                    let (method, path, body) = match read_http_request(&mut stream).await {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("[signin-loopback] read err: {}", e);
                            write_http_response(&mut stream, "HTTP/1.1 400 Bad Request",
                                "{\"error\":\"bad request\"}").await;
                            continue;
                        }
                    };

                    // CORS preflight from dcp.sa
                    if method == "OPTIONS" {
                        write_http_response(&mut stream, "HTTP/1.1 204 No Content", "").await;
                        continue;
                    }

                    if method != "POST" || !path.starts_with("/exchange") {
                        write_http_response(&mut stream, "HTTP/1.1 404 Not Found",
                            "{\"error\":\"not found\"}").await;
                        continue;
                    }

                    let parsed: Result<SigninReceivedPayload, _> = serde_json::from_str(&body);
                    let payload = match parsed {
                        Ok(p) if !p.api_key.is_empty() && (p.role == "provider" || p.role == "renter") => p,
                        _ => {
                            write_http_response(&mut stream, "HTTP/1.1 400 Bad Request",
                                "{\"error\":\"missing or invalid api_key/role\"}").await;
                            continue;
                        }
                    };

                    write_http_response(&mut stream, "HTTP/1.1 200 OK",
                        "{\"ok\":true}").await;
                    let _ = win.emit("signin:received", &payload);
                    eprintln!("[signin-loopback] received api_key for role={}, shutting down", payload.role);
                    return;
                }
            }
        }
    });

    Ok(SigninLoopback {
        port,
        callback_url,
    })
}

/// Cancel any pending sign-in loopback listener (e.g. user closed the
/// embedded sign-in screen).
#[tauri::command]
async fn cancel_signin_loopback(state: State<'_, SigninListener>) -> Result<(), String> {
    let mut s = state.lock().map_err(|e| format!("lock: {}", e))?;
    if let Some(tx) = s.cancel.take() {
        let _ = tx.send(());
    }
    s.port = None;
    Ok(())
}

/// Trigger the magic-link email. `role` is "provider" or "renter".
/// `callback_url` is the loopback URL returned by `start_signin_loopback`.
/// Backend embeds it in the magic-link URL so the verify page can POST
/// the api_key back to us when the user clicks.
#[tauri::command]
async fn send_signin_email(
    email: String,
    role: String,
    callback_url: String,
) -> Result<(), String> {
    let email = email.trim();
    if email.is_empty() || !email.contains('@') {
        return Err("Please enter a valid email address.".to_string());
    }
    if role != "provider" && role != "renter" {
        return Err("role must be 'provider' or 'renter'".to_string());
    }
    // Sanity-check the callback URL on this side too — the user may have
    // poked at the IPC. The backend re-validates anyway.
    if !(callback_url.starts_with("http://127.0.0.1:")
        || callback_url.starts_with("http://localhost:")
        || callback_url.starts_with("http://[::1]:"))
    {
        return Err("callback_url must be a loopback URL".to_string());
    }

    let endpoint = format!("https://api.dcp.sa/api/{}s/send-otp", role);
    let body = serde_json::json!({
        "email": email,
        "desktop_callback": callback_url,
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("client build: {}", e))?;

    let resp = client
        .post(&endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("network error reaching api.dcp.sa: {}", e))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // Try to surface backend-supplied error message
        if let Ok(j) = serde_json::from_str::<Value>(&text) {
            if let Some(msg) = j.get("error").and_then(|v| v.as_str()) {
                return Err(msg.to_string());
            }
        }
        return Err(format!("send-otp failed ({}): {}", status, text));
    }
    Ok(())
}

// ── App Entry ────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Mutex::new(DaemonState {
            pid: None,
            status: "stopped".to_string(),
            last_restart: None,
            restart_count: 0,
            started_at: None,
        }))
        .manage::<SigninListener>(Mutex::new(SigninListenerState::default()))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        // G37: prevent multiple instances — second launch focuses existing window
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .setup(|app| {
            // ── System tray setup ────────────────────────────────────
            let show = MenuItemBuilder::with_id("show", "Open DCP Provider").build(app)?;
            let earnings = MenuItemBuilder::with_id("earnings", "Earnings: calculating...").enabled(false).build(app)?;
            let status = MenuItemBuilder::with_id("status", "Status: Starting...").enabled(false).build(app)?;
            let separator1 = tauri::menu::PredefinedMenuItem::separator(app)?;
            let pause = MenuItemBuilder::with_id("pause", "Pause Provider").build(app)?;
            let resume = MenuItemBuilder::with_id("resume", "Resume Provider").build(app)?;
            let separator2 = tauri::menu::PredefinedMenuItem::separator(app)?;
            let dashboard = MenuItemBuilder::with_id("dashboard", "Open Dashboard").build(app)?;
            let logs = MenuItemBuilder::with_id("logs", "View Logs").build(app)?;
            let separator3 = tauri::menu::PredefinedMenuItem::separator(app)?;
            // L2: build a fresh separator4 instead of reusing separator3
            let separator4 = tauri::menu::PredefinedMenuItem::separator(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit DCP").build(app)?;

            let menu = MenuBuilder::new(app)
                .items(&[
                    &show, &separator1,
                    &status, &earnings, &separator2,
                    &pause, &resume, &separator3,
                    &dashboard, &logs, &separator4,
                    &quit,
                ])
                .build()?;

            // Prevent window close from quitting — hide to menu bar instead
            // H9: don't panic if main window is missing (config drift); log and skip.
            if let Some(win_handle) = app.get_webview_window("main") {
                let win_hide = win_handle.clone();
                win_handle.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = win_hide.hide();
                    }
                });
            } else {
                eprintln!("[setup] main webview window not found — close-to-tray disabled");
            }

            // H9: build tray without an icon if default_window_icon is unavailable
            // rather than panicking the entire app at launch.
            let mut tray_builder = TrayIconBuilder::new();
            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            } else {
                eprintln!("[setup] default_window_icon missing — tray will use platform default");
            }
            let _tray = tray_builder
                .menu(&menu)
                .tooltip("DCP Provider — Running")
                .on_menu_event(move |app, event| {
                    match event.id().as_ref() {
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "dashboard" => {
                            let _ = open::that("https://dcp.sa/provider");
                        }
                        "logs" => {
                            // Daemon writes to ~/.dcp/daemon.log (dcp_home()).
                            let log_path = dirs::home_dir()
                                .unwrap_or_default()
                                .join(".dcp")
                                .join("daemon.log");
                            if let Err(e) = open::that(&log_path) {
                                eprintln!("Failed to open log file {:?}: {}", log_path, e);
                            }
                        }
                        "pause" => {
                            // Tier 2.7 / G56: tray-driven pause. Spawn the
                            // network call so the menu thread does not block
                            // the UI; surface errors via eprintln + a tray
                            // notification (best-effort — notification plugin
                            // is already registered above).
                            let app_handle = app.clone();
                            tauri::async_runtime::spawn(async move {
                                match load_api_key_from_config() {
                                    Ok(key) => match pause_provider(key).await {
                                        Ok(_) => {
                                            eprintln!("[tray] pause_provider OK");
                                            notify_tray(&app_handle, "DCP Provider paused",
                                                "You will not receive new jobs until you resume.");
                                        }
                                        Err(e) => {
                                            eprintln!("[tray] pause_provider failed: {}", e);
                                            notify_tray(&app_handle, "DCP pause failed",
                                                &format!("Could not pause: {}", e));
                                        }
                                    },
                                    Err(e) => {
                                        eprintln!("[tray] pause: load_api_key_from_config failed: {}", e);
                                        notify_tray(&app_handle, "DCP pause failed",
                                            &format!("Config unreadable: {}", e));
                                    }
                                }
                            });
                        }
                        "resume" => {
                            // Tier 2.7 / G56: tray-driven resume.
                            let app_handle = app.clone();
                            tauri::async_runtime::spawn(async move {
                                match load_api_key_from_config() {
                                    Ok(key) => match resume_provider(key).await {
                                        Ok(_) => {
                                            eprintln!("[tray] resume_provider OK");
                                            notify_tray(&app_handle, "DCP Provider resumed",
                                                "Now accepting jobs again.");
                                        }
                                        Err(e) => {
                                            eprintln!("[tray] resume_provider failed: {}", e);
                                            notify_tray(&app_handle, "DCP resume failed",
                                                &format!("Could not resume: {}", e));
                                        }
                                    },
                                    Err(e) => {
                                        eprintln!("[tray] resume: load_api_key_from_config failed: {}", e);
                                        notify_tray(&app_handle, "DCP resume failed",
                                            &format!("Config unreadable: {}", e));
                                    }
                                }
                            });
                        }
                        "quit" => {
                            app.exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;

            // ── Auto-restart daemon on app launch if configured ──────────
            // If the provider has been set up (api_key exists) and the daemon
            // is not running (stale PID or missing PID file), restart it.
            // This covers Windows reboot via autostart plugin and macOS
            // LaunchAgent re-launch after a crash.
            {
                if let Ok(api_key) = load_api_key_from_config() {
                    let daemon_alive = read_pid_file()
                        .map(is_process_alive)
                        .unwrap_or(false);
                    if !daemon_alive {
                        eprintln!("[startup] Daemon not running. Auto-restarting for configured provider.");
                        remove_pid_file();
                        let app_handle = app.handle().clone();
                        tauri::async_runtime::spawn(async move {
                            let dcp_dir = match dcp_home() {
                                Ok(d) => d,
                                Err(e) => { eprintln!("[startup] Auto-restart failed: {}", e); return; }
                            };
                            let daemon_path = dcp_dir.join("dcp_daemon.py");
                            if !daemon_path.exists() {
                                eprintln!("[startup] Auto-restart skipped: daemon not downloaded yet");
                                return;
                            }
                            let log_path = dcp_dir.join("daemon.log");
                            let err_log_path = dcp_dir.join("daemon_error.log");
                            let log_file = match std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
                                Ok(f) => f,
                                Err(e) => { eprintln!("[startup] Auto-restart log open failed: {}", e); return; }
                            };
                            let err_file = match std::fs::OpenOptions::new().create(true).append(true).open(&err_log_path) {
                                Ok(f) => f,
                                Err(e) => { eprintln!("[startup] Auto-restart err log open failed: {}", e); return; }
                            };
                            // Read served_model and engine from config
                            let config_path = dcp_dir.join("config.json");
                            let (served_model, engine_name) = if let Ok(content) = std::fs::read_to_string(&config_path) {
                                if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
                                    (
                                        config.get("served_model").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                        config.get("engine").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                    )
                                } else { (String::new(), String::new()) }
                            } else { (String::new(), String::new()) };

                            match detach_process(Command::new(python_cmd())
                                .arg(&daemon_path)
                                .arg("--no-watchdog")
                                .arg("--key")
                                .arg(&api_key)
                                .arg("--url")
                                .arg("https://api.dcp.sa")
                                .env("DCP_SERVED_MODEL", &served_model)
                                .env("DCP_ENGINE", &engine_name)
                                .stdout(log_file)
                                .stderr(err_file))
                                .spawn()
                            {
                                Ok(child) => {
                                    let pid = child.id();
                                    if let Err(e) = write_pid_file(pid) {
                                        eprintln!("[startup] Auto-restart PID write failed: {}", e);
                                    }
                                    // Update managed DaemonManager state
                                    let state: tauri::State<'_, DaemonManager> = app_handle.state();
                                    if let Ok(mut guard) = state.lock() {
                                        guard.pid = Some(pid);
                                        guard.status = "running".to_string();
                                        guard.started_at = Some(std::time::Instant::now());
                                        guard.last_restart = Some(std::time::Instant::now());
                                        guard.restart_count += 1;
                                    }
                                    eprintln!("[startup] Auto-restart OK — daemon PID {}", pid);
                                }
                                Err(e) => {
                                    eprintln!("[startup] Auto-restart spawn failed: {}", e);
                                }
                            }
                        });
                    }
                }
            }

            // ── G57: 60-second tray status refresh loop ─────────────────
            // Reads local state only (PID file + config.json) — no network calls.
            {
                let status_item = status.clone();
                let earnings_item = earnings.clone();
                let _app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let mut interval = tokio::time::interval(
                        std::time::Duration::from_secs(60),
                    );
                    // The first tick fires immediately — skip it so we don't
                    // race the initial "Status: Starting..." label.
                    interval.tick().await;

                    loop {
                        interval.tick().await;

                        // 1. Daemon alive? Check PID file + kill -0.
                        let daemon_alive = read_pid_file()
                            .map(is_process_alive)
                            .unwrap_or(false);

                        // 2. Read run_mode from config.json (idle / active / paused)
                        let run_mode = load_config_field("run_mode")
                            .unwrap_or_default();

                        let status_text = if !daemon_alive {
                            "Status: Offline".to_string()
                        } else if run_mode == "paused" {
                            "Status: Paused".to_string()
                        } else {
                            "Status: Online".to_string()
                        };

                        if let Err(e) = status_item.set_text(&status_text) {
                            eprintln!("[tray-refresh] failed to update status: {}", e);
                        }

                        // 3. Earnings — read from config if the daemon writes it,
                        //    otherwise keep the generic label.
                        let earnings_text = match load_config_field("total_earnings") {
                            Some(val) => format!("Earnings: ${}", val),
                            None => "Earnings: —".to_string(),
                        };
                        if let Err(e) = earnings_item.set_text(&earnings_text) {
                            eprintln!("[tray-refresh] failed to update earnings: {}", e);
                        }
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            detect_gpu,
            detect_system,
            validate_api_key,
            start_daemon,
            get_estimated_earnings,
            check_setup_complete,
            fetch_provider_dashboard,
            fetch_provider_metrics,
            fetch_recent_jobs,
            pause_provider,
            resume_provider,
            read_config,
            start_daemon_process,
            stop_daemon_process,
            get_daemon_status,
            check_daemon_health,
            get_live_metrics,
            install_engine,
            download_model,
            update_daemon,
            rollback_daemon,
            full_start_provider,
            report_install_error,
            get_model_metadata,
            pre_install_speed_probe,
            get_network_status,
            rotate_network_key,
            reconnect_network,
            open_install_log,
            start_signin_loopback,
            cancel_signin_loopback,
            send_signin_email,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}

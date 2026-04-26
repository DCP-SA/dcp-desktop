use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::Mutex;
use tauri::{Manager, State};
use tauri::tray::{TrayIconBuilder, MouseButton, MouseButtonState};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
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
pub struct RegistrationResult {
    pub provider_id: String,
    pub api_key: String,
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
    Ok(key.starts_with("dcp-provider-") && key.len() > 20)
}

#[tauri::command]
async fn register_provider(email: String) -> Result<RegistrationResult, String> {
    // In production, this would POST to the API
    // For now, simulate registration with a deterministic key
    if email.is_empty() || !email.contains('@') {
        return Err("Invalid email address".to_string());
    }

    // TODO: Replace with actual HTTP call when backend is ready
    // let client = reqwest::Client::new();
    // let response = client
    //     .post("https://api.dcp.sa/api/providers/register")
    //     .json(&serde_json::json!({ "email": email }))
    //     .send()
    //     .await
    //     .map_err(|e| format!("Registration failed: {}", e))?;

    // Simulated response
    let hash = format!("{:x}", md5_hash(&email));
    Ok(RegistrationResult {
        provider_id: format!("prov_{}", &hash[..12]),
        api_key: format!("dcp-provider-{}", &hash[..32]),
    })
}

/// Simple hash for demo purposes (not cryptographic)
fn md5_hash(input: &str) -> u64 {
    let mut hash: u64 = 5381;
    for byte in input.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u64);
    }
    hash
}

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
    // Rough earnings estimate based on hardware capability
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

    // Return monthly estimate in USD
    Ok(base_rate)
}

// ── Backend API Commands ─────────────────────────────────────────────

#[tauri::command]
async fn fetch_provider_dashboard(api_key: String) -> Result<ProviderDashboard, String> {
    let url = format!("{}/me?key={}", API_BASE, api_key);
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
    let url = format!("{}/me/metrics?key={}", API_BASE, api_key);
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
    let url = format!("{}/me?key={}", API_BASE, api_key);
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
        })
        .collect();

    Ok(jobs)
}

#[tauri::command]
async fn pause_provider(api_key: String) -> Result<(), String> {
    let url = format!("{}/pause", API_BASE);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("x-api-key", &api_key)
        .json(&serde_json::json!({ "api_key": api_key }))
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
    let url = format!("{}/resume", API_BASE);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("x-api-key", &api_key)
        .json(&serde_json::json!({ "api_key": api_key }))
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

/// Find the Ollama executable (checks PATH + known Windows install path)
fn ollama_cmd() -> String {
    if command_exists("ollama") {
        return "ollama".to_string();
    }
    #[cfg(target_os = "windows")]
    {
        // Ollama Inno Setup installs to %LOCALAPPDATA%\Programs\Ollama
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let ollama_path = format!(r"{}\Programs\Ollama\ollama.exe", local_app_data);
            if std::path::Path::new(&ollama_path).exists() {
                return ollama_path;
            }
        }
        // Also check Program Files
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

/// Read the last N lines from a file
fn tail_file(path: &std::path::Path, n: usize) -> Vec<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
            let start = if lines.len() > n { lines.len() - n } else { 0 };
            lines[start..].to_vec()
        }
        Err(_) => Vec::new(),
    }
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

    // 2. Download daemon if not present
    if !daemon_path.exists() {
        let download_url = format!(
            "https://api.dcp.sa/api/providers/download/daemon?key={}",
            api_key
        );
        let client = reqwest::Client::new();
        let resp = client
            .get(&download_url)
            .header("x-api-key", &api_key)
            .send()
            .await
            .map_err(|e| format!("Failed to download daemon: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("Failed to download daemon: HTTP {}", resp.status()));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("Failed to read daemon download: {}", e))?;

        // M11 — atomic write so we never spawn a half-downloaded daemon
        atomic_write(&daemon_path, &bytes)
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
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "--connect-timeout", "5", "https://api.dcp.sa/health"]))
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

    // 10. Sufficient disk space?
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
                        if available_gb >= 20 {
                            checks.push(HealthCheck {
                                name: "Disk Space".to_string(),
                                status: "ok".to_string(),
                                message: format!("{}GB available", available_gb),
                                can_auto_fix: false,
                                fix_action: None,
                            });
                        } else if available_gb >= 10 {
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
                    let (status, msg) = if available_gb >= 20 {
                        ("ok", format!("{}GB available", available_gb))
                    } else if available_gb >= 10 {
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
                        let (status, msg) = if available_gb >= 20 {
                            ("ok", format!("{}GB available", available_gb))
                        } else if available_gb >= 10 {
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

        let output = Command::new(&installer_path)
            .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
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
        let output = Command::new(&ollama_cmd())
            .args(["pull", &model_name])
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

#[tauri::command]
async fn update_daemon(api_key: String) -> Result<String, String> {
    let dcp_dir = dcp_home()?;
    let daemon_path = dcp_dir.join("dcp_daemon.py");

    // 1. Download latest daemon
    let download_url = format!(
        "https://api.dcp.sa/api/providers/download/daemon?key={}",
        api_key
    );
    let client = reqwest::Client::new();
    let resp = client
        .get(&download_url)
        .header("x-api-key", &api_key)
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

    // 2. Compare with current file
    let current_bytes = std::fs::read(&daemon_path).unwrap_or_default();
    if current_bytes == new_bytes.as_ref() {
        return Ok("already_latest".to_string());
    }

    // 3. Stop the daemon if running
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

    // 4. Replace the daemon file (M11 — atomic; G6 backup-before-overwrite is a separate task)
    atomic_write(&daemon_path, &new_bytes)
        .map_err(|e| format!("Failed to write updated daemon: {}", e))?;

    // 5. Try to extract version from the new file
    let content = String::from_utf8_lossy(&new_bytes);
    let version = content
        .lines()
        .find(|l| l.contains("__version__") || l.contains("VERSION"))
        .and_then(|l| {
            l.split('=')
                .last()
                .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());

    Ok(format!("updated:{}", version))
}

// ── Cloudflare Tunnel for NAT Traversal ─────────────────────────────

/// Download cloudflared binary if not present, start tunnel, return the URL
async fn start_cloudflare_tunnel(dcp_dir: &std::path::Path, port: u16) -> Result<String, String> {
    // Kill any existing tunnel
    kill_by_name("cloudflared");
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Determine cloudflared binary path
    let cloudflared_path = dcp_dir.join(if cfg!(windows) { "cloudflared.exe" } else { "cloudflared" });

    // Download if not present
    if !cloudflared_path.exists() {
        let download_url = if cfg!(target_os = "windows") {
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe"
        } else if cfg!(target_os = "macos") {
            if cfg!(target_arch = "aarch64") {
                "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-amd64.tgz"
            } else {
                "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-darwin-amd64.tgz"
            }
        } else {
            "https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64"
        };

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        let resp = client.get(download_url).send().await
            .map_err(|e| format!("cloudflared download failed: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("cloudflared download HTTP {}", resp.status()));
        }
        let bytes = resp.bytes().await
            .map_err(|e| format!("cloudflared read failed: {}", e))?;
        std::fs::write(&cloudflared_path, &bytes)
            .map_err(|e| format!("cloudflared write failed: {}", e))?;

        // Make executable on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&cloudflared_path,
                std::fs::Permissions::from_mode(0o755));
        }
    }

    // Start the tunnel
    let tunnel_log = dcp_dir.join("cloudflared.log");
    // M7 — append, don't truncate
    let log_file = std::fs::OpenOptions::new().create(true).append(true).open(&tunnel_log)
        .map_err(|e| format!("Tunnel log open failed: {}", e))?;

    let _tunnel = hide_window(
        Command::new(&cloudflared_path)
            .args(["tunnel", "--url", &format!("http://localhost:{}", port), "--no-autoupdate"])
            .stdout(std::process::Stdio::null())
            .stderr(log_file)
    ).spawn()
        .map_err(|e| format!("cloudflared start failed: {}", e))?;

    // Wait for tunnel URL to appear in logs (up to 15 seconds)
    let mut tunnel_url = String::new();
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Ok(log_content) = std::fs::read_to_string(&tunnel_log) {
            // cloudflared prints the URL like: https://xxx-yyy.trycloudflare.com
            for line in log_content.lines() {
                if let Some(start) = line.find("https://") {
                    let url_part = &line[start..];
                    if url_part.contains(".trycloudflare.com") {
                        // Extract just the URL
                        let end = url_part.find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                            .unwrap_or(url_part.len());
                        tunnel_url = url_part[..end].to_string();
                        break;
                    }
                }
            }
            if !tunnel_url.is_empty() { break; }
        }
    }

    if tunnel_url.is_empty() {
        return Err("cloudflared started but no tunnel URL found in logs".to_string());
    }

    Ok(tunnel_url)
}

// ── Full Provider Start (chains: engine install → model download → inference server → daemon) ──

#[tauri::command]
async fn full_start_provider(api_key: String, state: State<'_, DaemonManager>) -> Result<String, String> {
    let dcp_dir = dcp_home()?;

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
                if let Ok(o) = Command::new(&path)
                    .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
                    .output()
                {
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
        // ≥12GB (3060Ti 12GB/4070): Dense 8B — 107-197 tok/s
        //  ≥8GB (3060Ti 8GB/4060):  Mistral 7B — fastest at this tier (124-274 tok/s)
        //  <8GB:                     Dense 4B — only option (163-270 tok/s)
        model = if vram_mb >= 20000 {
            "qwen3:30b-a3b".to_string()
        } else if vram_mb >= 10000 {
            "qwen3:8b".to_string()
        } else if vram_mb >= 6000 {
            "mistral:7b".to_string()
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
        }
    } else {
        // Ollama — cross-platform install
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
                // Download OllamaSetup.exe directly — works on all Windows 10+ without winget
                let installer_path = dcp_home()?.join("OllamaSetup.exe");
                let response = reqwest::get("https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe")
                    .await
                    .map_err(|e| format!("Failed to download Ollama installer: {}", e))?;
                let bytes = response.bytes().await
                    .map_err(|e| format!("Failed to read Ollama installer bytes: {}", e))?;
                std::fs::write(&installer_path, &bytes)
                    .map_err(|e| format!("Failed to save OllamaSetup.exe: {}", e))?;

                let install = Command::new(&installer_path)
                    .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
                    .output()
                    .map_err(|e| format!("Failed to run Ollama installer: {}", e))?;

                // Clean up installer
                let _ = std::fs::remove_file(&installer_path);

                if !install.status.success() {
                    return Err("Failed to install Ollama. Install manually from https://ollama.com/download/windows".to_string());
                }
            }
        }
    }

    log_startup!("Step 2: Engine install complete ({})", engine);

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

        // Start mlx_lm.server — it auto-downloads the model on first run
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

        // Write model info to config
        let config_path = dcp_dir.join("config.json");
        if let Ok(config_str) = std::fs::read_to_string(&config_path) {
            if let Ok(mut config) = serde_json::from_str::<serde_json::Value>(&config_str) {
                config["served_model"] = serde_json::Value::String(model.clone());
                config["engine"] = serde_json::Value::String(engine.clone());
                let _ = std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap_or_default());
            }
        } else {
            // Create config if it doesn't exist
            let config = serde_json::json!({
                "api_key": api_key,
                "served_model": model,
                "engine": engine,
            });
            let _ = std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap_or_default());
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
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
            ).spawn()
                .map_err(|e| format!("Failed to start Ollama: {}", e))?;
            // Wait for it to be ready
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap_or_default();
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if client.get("http://localhost:11434/api/tags")
                    .send().await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false) { break; }
            }
        }

        // Check if model already pulled
        let list_output = Command::new(&ollama_cmd()).args(["list"]).output();
        model_cached = list_output.ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&model))
            .unwrap_or(false);

        if !model_cached {
            // Pull the model
            let pull = Command::new(&ollama_cmd())
                .args(["pull", &model])
                .output()
                .map_err(|e| format!("Model pull failed: {}", e))?;
            if !pull.status.success() {
                let stderr = String::from_utf8_lossy(&pull.stderr).to_string();
                return Err(format!("ollama pull {} failed: {}", model, stderr));
            }
        }
    }

    // Step 3.5 (Windows only): Ensure Python is available, install embedded Python if needed
    #[cfg(windows)]
    {
        let python_ok = Command::new(python_cmd())
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

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
                let extract = Command::new("powershell")
                    .args([
                        "-NoProfile", "-Command",
                        &format!(
                            "Expand-Archive -Force -Path '{}' -DestinationPath '{}'",
                            zip_path.display(),
                            python_dir.display()
                        ),
                    ])
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

                let pip_install = Command::new(&python_exe)
                    .arg(&getpip_path)
                    .output()
                    .map_err(|e| format!("get-pip run failed: {}", e))?;
                if !pip_install.status.success() {
                    let stderr = String::from_utf8_lossy(&pip_install.stderr);
                    return Err(format!("pip bootstrap failed: {}", stderr));
                }
                let _ = std::fs::remove_file(&getpip_path);

                // Install required packages
                let deps = Command::new(&python_exe)
                    .args(["-m", "pip", "install", "requests", "psutil"])
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

    // Step 4: Always download latest daemon (auto-update on every start)
    let daemon_path = dcp_dir.join("dcp_daemon.py");
    {
        let url = format!("https://api.dcp.sa/api/providers/download/daemon?key={}", api_key);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.bytes().await {
                    Ok(bytes) => {
                        // M11 — atomic write
                        if let Err(e) = atomic_write(&daemon_path, &bytes) {
                            log_startup!("Step 4a: Daemon write failed: {} (using cached)", e);
                        } else {
                            log_startup!("Step 4a: Daemon downloaded ({} bytes)", bytes.len());
                        }
                    }
                    Err(e) => {
                        log_startup!("Step 4a: Daemon download read failed: {} (using cached)", e);
                    }
                }
            }
            Ok(resp) => {
                log_startup!("Step 4a: Daemon download HTTP {} (using cached)", resp.status());
            }
            Err(e) => {
                log_startup!("Step 4a: Daemon download failed: {} (using cached if exists)", e);
            }
        }
        if !daemon_path.exists() {
            return Err("Daemon file not found and download failed. Check internet connection.".to_string());
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
    let _ = write_pid_file(pid);

    {
        let mut guard = state.lock().map_err(|e| format!("Lock: {}", e))?;
        guard.pid = Some(pid);
        guard.status = "running".to_string();
        guard.started_at = Some(std::time::Instant::now());
        guard.restart_count = 0;
    }

    log_startup!("Step 4: Daemon started — PID={}", pid);

    // Step 5: Start Cloudflare Tunnel for NAT traversal
    // This exposes the local inference server to the internet so the backend can route jobs
    let inference_port = if engine == "mlx" { 8000 } else { 11434 };
    let tunnel_url = match start_cloudflare_tunnel(&dcp_dir, inference_port).await {
        Ok(url) => {
            // Log success
            let _ = std::fs::OpenOptions::new().append(true).create(true)
                .open(dcp_dir.join("startup.log"))
                .and_then(|mut f| { use std::io::Write; writeln!(f, "[tunnel] OK: {}", url) });
            url
        }
        Err(e) => {
            // Log failure — don't silently swallow
            let _ = std::fs::OpenOptions::new().append(true).create(true)
                .open(dcp_dir.join("startup.log"))
                .and_then(|mut f| { use std::io::Write; writeln!(f, "[tunnel] FAILED: {}", e) });
            // Continue without tunnel — provider will be online but unreachable for inference
            String::new()
        }
    };

    // Step 6: Register tunnel URL with backend
    if !tunnel_url.is_empty() {
        let client = reqwest::Client::new();
        let _ = client.post(format!("{}/endpoint", API_BASE))
            .json(&serde_json::json!({
                "key": api_key,
                "vllm_endpoint_url": tunnel_url
            }))
            .send()
            .await;

        // Save tunnel URL to config
        let config_path = dcp_dir.join("config.json");
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(mut config) = serde_json::from_str::<serde_json::Value>(&content) {
                config["tunnel_url"] = serde_json::Value::String(tunnel_url.clone());
                let _ = std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap_or_default());
            }
        }
    }

    // Upload all logs to backend (success case)
    log_startup!("Step 7: All steps complete — uploading logs to backend");
    upload_provider_logs(&api_key, &dcp_dir).await;

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
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_store::Builder::new().build())
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
                            // G55/G32: daemon writes to ~/dc1-provider/logs/daemon.log
                            // (LOG_DIR in dcp_daemon.py:178). The previous ~/.dcp/daemon.log
                            // path was always empty/missing.
                            let log_path = dirs::home_dir()
                                .unwrap_or_default()
                                .join("dc1-provider")
                                .join("logs")
                                .join("daemon.log");
                            if let Err(e) = open::that(&log_path) {
                                eprintln!("Failed to open log file {:?}: {}", log_path, e);
                            }
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

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            detect_gpu,
            detect_system,
            validate_api_key,
            register_provider,
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
            full_start_provider,
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

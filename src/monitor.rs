use crate::data::{GpuData, GpuInfo, ProcessInfo, ThrottleReasons};
use nvml_wrapper::bitmasks::device::ThrottleReasons as NvmlThrottleReasons;
use nvml_wrapper::enum_wrappers::device::{
    Clock, PcieUtilCounter, PerformancePolicy, TemperatureSensor,
};
use nvml_wrapper::enums::device::UsedGpuMemory;
use nvml_wrapper::Nvml;
use thiserror::Error;

use amdgpu_sysfs::gpu_handle::GpuHandle;
use std::path::PathBuf;

#[derive(Error, Debug)]
pub enum MonitorError {
    #[error("NVML initialization failed: {0}")]
    NvmlInit(#[from] nvml_wrapper::error::NvmlError),
    #[error("Failed to get data: {0}")]
    SamplingFailed(String),
}

/// `sample` takes `&mut self` so a backend can cache readings across ticks;
/// the monitor is moved into the sampling thread and owned exclusively by it,
/// so `Send` alone is enough — `Sync` was never needed.
pub trait GpuMonitor: Send {
    fn get_static_info(&self) -> GpuInfo;
    fn sample(&mut self) -> Result<(GpuData, Vec<ProcessInfo>), MonitorError>;
}

/// `nvmlDeviceGetPcieThroughput` averages over a fixed 20 ms internal window,
/// so each of the two calls blocks for ~21 ms. Measured on an RTX 5060 Ti
/// (driver 580.159.03) they cost 42.7 ms of a 43.3 ms sample — 99% of the
/// budget — and the window size is not configurable. Refresh them on their own
/// slower cadence and reuse the previous reading in between.
const PCIE_REFRESH_EVERY: u32 = 10;

/// `nvmlDeviceGetViolationStatus` costs ~0.56 ms/call against 0.000 ms for the
/// throttle bitmask, so the counter reads on its own slower cadence.
const VIOLATION_REFRESH_EVERY: u32 = 10;

/// A sample at or above this utilization counts the GPU as doing work. The
/// power-cap counter advances while idle too — measured +32 ms over a 10 s
/// window at 5% utilization in P5 — so a wall-clock denominator would report
/// throttling on a machine sitting at the desktop.
const BUSY_UTILIZATION_PERCENT: f32 = 10.0;

/// Below this much accumulated busy time the share is noise, not a measurement.
const MIN_BUSY_NANOS: u64 = 5_000_000_000;

/// A gap between successful samples longer than this means the loop stalled —
/// NVML erroring for a while, or the process frozen. The driver's counter runs
/// through such a gap while the utilization samples do not, so the window
/// spanning it cannot be attributed and is thrown away instead of counted.
const SAMPLE_GAP_LIMIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Accumulates how much of the GPU's *busy* time the power limit held clocks
/// down, from the driver's own cumulative counter.
///
/// The counter cannot be split across a window, so a window counts in full or
/// not at all, decided by whether most of its samples saw the GPU working.
/// That costs precision at the boundary between idle and load and buys a
/// number that does not accuse an idle desktop of throttling.
#[derive(Default)]
struct PowerCapAccounting {
    busy_samples: u32,
    window_samples: u32,
    last_counter_nanos: Option<u64>,
    busy_nanos: u64,
    capped_nanos: u64,
}

impl PowerCapAccounting {
    fn observe_sample(&mut self, utilization: f32) {
        self.window_samples += 1;
        if utilization >= BUSY_UTILIZATION_PERCENT {
            self.busy_samples += 1;
        }
    }

    /// Close the current window against a fresh reading of the driver counter.
    fn close_window(&mut self, counter_nanos: u64, window_nanos: u64) {
        let previous = self.last_counter_nanos.replace(counter_nanos);
        let mostly_busy = self.busy_samples * 2 > self.window_samples;
        self.window_samples = 0;
        self.busy_samples = 0;

        // The first reading only establishes a baseline; the counter is
        // cumulative since driver load, not since this process started.
        let Some(previous) = previous else {
            return;
        };
        if mostly_busy {
            self.busy_nanos += window_nanos;
            self.capped_nanos += counter_nanos.saturating_sub(previous);
        }
    }

    /// Abandon the window in progress and re-baseline on the next reading.
    /// What has already been accumulated stays: it was measured correctly.
    fn discard_window(&mut self) {
        self.window_samples = 0;
        self.busy_samples = 0;
        self.last_counter_nanos = None;
    }

    fn capped_share(&self) -> Option<f32> {
        (self.busy_nanos >= MIN_BUSY_NANOS)
            .then(|| (self.capped_nanos as f64 / self.busy_nanos as f64).clamp(0.0, 1.0) as f32)
    }
}

/// NVML asserts several bits that do not describe a limit the user is running
/// into; see [`ThrottleReasons`] for why they are dropped.
fn map_throttle_reasons(raw: NvmlThrottleReasons) -> ThrottleReasons {
    ThrottleReasons {
        power_cap: raw.contains(NvmlThrottleReasons::SW_POWER_CAP),
        thermal: raw.intersects(
            NvmlThrottleReasons::SW_THERMAL_SLOWDOWN | NvmlThrottleReasons::HW_THERMAL_SLOWDOWN,
        ),
        hardware: raw.intersects(
            NvmlThrottleReasons::HW_SLOWDOWN | NvmlThrottleReasons::HW_POWER_BRAKE_SLOWDOWN,
        ),
    }
}

// ── NVIDIA Backend ──────────────────────────────────────────────────────────

pub struct NvmlMonitor {
    nvml: Nvml,
    device_index: u32,
    start_time: std::time::Instant,
    /// Last PCIe throughput reading in MB/s, refreshed every
    /// `PCIE_REFRESH_EVERY` samples and reused in between.
    pcie_throughput: (f64, f64),
    ticks_since_pcie: u32,
    power_cap: PowerCapAccounting,
    ticks_since_violation: u32,
    last_violation_read: std::time::Instant,
    last_sample_at: std::time::Instant,
}

impl NvmlMonitor {
    pub fn new(device_index: u32) -> Result<Self, MonitorError> {
        let nvml = Nvml::init()?;
        // Check if the device exists
        nvml.device_by_index(device_index)?;
        Ok(Self {
            nvml,
            device_index,
            start_time: std::time::Instant::now(),
            pcie_throughput: (0.0, 0.0),
            ticks_since_pcie: 0,
            power_cap: PowerCapAccounting::default(),
            ticks_since_violation: 0,
            last_violation_read: std::time::Instant::now(),
            last_sample_at: std::time::Instant::now(),
        })
    }
}

impl GpuMonitor for NvmlMonitor {
    fn get_static_info(&self) -> GpuInfo {
        let driver_version = self
            .nvml
            .sys_driver_version()
            .unwrap_or_else(|_| "N/A".to_string());

        let device_count = self.nvml.device_count().unwrap_or(1);

        let Ok(device) = self.nvml.device_by_index(self.device_index) else {
            return GpuInfo {
                name: "N/A".to_string(),
                driver_version,
                pcie_gen: 0,
                pcie_width: 0,
                device_count,
                per_process_supported: true,
            };
        };

        GpuInfo {
            name: device.name().unwrap_or_else(|_| "N/A".to_string()),
            driver_version,
            pcie_gen: device.current_pcie_link_gen().unwrap_or(0),
            pcie_width: device.current_pcie_link_width().unwrap_or(0),
            device_count,
            per_process_supported: true,
        }
    }

    fn sample(&mut self) -> Result<(GpuData, Vec<ProcessInfo>), MonitorError> {
        // Temporarily get the device object when needed
        let device = self.nvml.device_by_index(self.device_index)?;

        // Utilization and memory are the core metrics — without them the
        // sample is meaningless, so their errors still fail the call. All
        // remaining sensors degrade to 0 individually (some are unavailable
        // on vGPU/laptop setups), matching the AMD backend's convention.
        let util = device.utilization_rates()?;
        let mem = device.memory_info()?;
        let temp = device.temperature(TemperatureSensor::Gpu).unwrap_or(0);

        let gpu_clock = device.clock_info(Clock::Graphics).unwrap_or(0);
        let mem_clock = device.clock_info(Clock::Memory).unwrap_or(0);

        let power_usage = device
            .power_usage()
            .map(|v| v as f64 / 1000.0)
            .unwrap_or(0.0);
        let power_limit = device
            .power_management_limit()
            .map(|v| v as f64 / 1000.0)
            .unwrap_or(0.0);

        let fan_speed = device.fan_speed(0).unwrap_or(0);

        if self.ticks_since_pcie == 0 {
            self.pcie_throughput = (
                device
                    .pcie_throughput(PcieUtilCounter::Send)
                    .map(|v| v as f64 / 1024.0)
                    .unwrap_or(0.0),
                device
                    .pcie_throughput(PcieUtilCounter::Receive)
                    .map(|v| v as f64 / 1024.0)
                    .unwrap_or(0.0),
            );
        }
        self.ticks_since_pcie = (self.ticks_since_pcie + 1) % PCIE_REFRESH_EVERY;
        let (pcie_tx, pcie_rx) = self.pcie_throughput;

        // The bitmask is free to read; the cumulative counter is not.
        let throttle = device
            .current_throttle_reasons()
            .ok()
            .map(map_throttle_reasons);

        // Sampling can stall — NVML erroring after a GPU reset, or the whole
        // process suspended. The counter keeps running through it, so a window
        // spanning the gap would charge unobserved time against observed load.
        let now = std::time::Instant::now();
        if now.duration_since(self.last_sample_at) > SAMPLE_GAP_LIMIT {
            self.power_cap.discard_window();
            self.last_violation_read = now;
        }
        self.last_sample_at = now;

        self.power_cap.observe_sample(util.gpu as f32);
        self.ticks_since_violation = (self.ticks_since_violation + 1) % VIOLATION_REFRESH_EVERY;
        if self.ticks_since_violation == 0 {
            if let Ok(violation) = device.violation_status(PerformancePolicy::Power) {
                let now = std::time::Instant::now();
                let window = now.duration_since(self.last_violation_read);
                self.last_violation_read = now;
                self.power_cap
                    .close_window(violation.violation_time, window.as_nanos() as u64);
            }
        }

        let gpu_data = GpuData {
            timestamp: self.start_time.elapsed().as_secs_f64(),
            utilization: util.gpu as f32,
            memory_used: mem.used as f64 / 1024.0 / 1024.0 / 1024.0,
            memory_total: mem.total as f64 / 1024.0 / 1024.0 / 1024.0,
            temperature: temp,
            gpu_clock,
            memory_clock: mem_clock,
            power_usage,
            power_limit,
            fan_speed,
            pcie_throughput_tx: pcie_tx,
            pcie_throughput_rx: pcie_rx,
            throttle,
            power_capped_share: self.power_cap.capped_share(),
        };

        // NVML reports graphics (OpenGL/Vulkan/X) and compute (CUDA) workloads
        // through two separate endpoints; querying only one hides the other class.
        let graphics = device
            .running_graphics_processes()
            .map(to_process_infos)
            .unwrap_or_default();
        let compute = device
            .running_compute_processes()
            .map(to_process_infos)
            .unwrap_or_default();
        let process_infos = merge_process_lists(graphics, compute);

        Ok((gpu_data, process_infos))
    }
}

fn to_process_infos(
    procs: Vec<nvml_wrapper::struct_wrappers::device::ProcessInfo>,
) -> Vec<ProcessInfo> {
    procs
        .into_iter()
        .map(|proc| ProcessInfo {
            pid: proc.pid,
            name: read_process_name(proc.pid),
            memory_usage: match proc.used_gpu_memory {
                UsedGpuMemory::Used(v) => v,
                _ => 0,
            },
        })
        .collect()
}

/// The kernel truncates `/proc/<pid>/comm` to TASK_COMM_LEN - 1 bytes.
const COMM_MAX_LEN: usize = 15;

fn read_process_name(pid: u32) -> String {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    pick_process_name(&comm, &cmdline).unwrap_or_else(|| "unknown".to_string())
}

/// Basename of argv[0], which `/proc/<pid>/cmdline` stores NUL-separated.
/// Only the first component is useful: Electron and Chromium processes carry
/// kilobytes of switches after it.
fn basename_of_argv0(cmdline: &[u8]) -> Option<String> {
    let argv0 = cmdline.split(|&b| b == 0).find(|part| !part.is_empty())?;
    let argv0 = String::from_utf8_lossy(argv0);
    let base = argv0.rsplit('/').next().unwrap_or_default();
    (!base.is_empty()).then(|| base.to_string())
}

/// `comm` is what a process calls itself and is the better label, but the
/// kernel cuts it at 15 bytes — "xdg-desktop-por", "nvidia-persiste". cmdline
/// is never truncated, yet its argv[0] is the interpreter for scripted
/// programs ("python3") and the branded name for others ("Code", not "code").
///
/// So prefer comm, and reach for cmdline only in the one case where it is
/// strictly more informative: comm sits exactly at the truncation limit and
/// cmdline's basename continues it.
fn pick_process_name(comm: &str, cmdline: &[u8]) -> Option<String> {
    let comm = comm.trim();
    let argv_name = basename_of_argv0(cmdline);
    if comm.is_empty() {
        return argv_name;
    }
    match argv_name {
        Some(argv) if comm.len() >= COMM_MAX_LEN && argv.starts_with(comm) => Some(argv),
        _ => Some(comm.to_string()),
    }
}

/// A process can appear in both the graphics and compute lists; keep one
/// entry per PID with the larger reported memory figure.
fn merge_process_lists(mut base: Vec<ProcessInfo>, extra: Vec<ProcessInfo>) -> Vec<ProcessInfo> {
    for proc in extra {
        if let Some(existing) = base.iter_mut().find(|p| p.pid == proc.pid) {
            existing.memory_usage = existing.memory_usage.max(proc.memory_usage);
        } else {
            base.push(proc);
        }
    }
    base
}

// ── AMD Backend ─────────────────────────────────────────────────────────────

pub struct AmdgpuMonitor {
    gpu_handle: GpuHandle,
    start_time: std::time::Instant,
    device_count: u32,
}

impl AmdgpuMonitor {
    /// Try to find and initialise the first AMD GPU driven by `amdgpu`.
    pub fn new() -> Result<Self, MonitorError> {
        let devices = Self::find_amdgpu_devices();
        let sysfs_path = devices
            .first()
            .cloned()
            .ok_or_else(|| MonitorError::SamplingFailed("No amdgpu device found".into()))?;

        let gpu_handle = GpuHandle::new_from_path(sysfs_path)
            .map_err(|e| MonitorError::SamplingFailed(format!("amdgpu_sysfs init: {e}")))?;

        Ok(Self {
            gpu_handle,
            start_time: std::time::Instant::now(),
            device_count: devices.len() as u32,
        })
    }

    /// Scan `/sys/class/drm/card*/device/` for devices using the `amdgpu`
    /// kernel driver, in card order.
    fn find_amdgpu_devices() -> Vec<PathBuf> {
        let Ok(drm_dir) = std::fs::read_dir("/sys/class/drm") else {
            return Vec::new();
        };
        let mut cards: Vec<_> = drm_dir
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                // Match "card0", "card1", ... but not "card0-DP-1" etc.
                name.starts_with("card") && name[4..].chars().all(|c| c.is_ascii_digit())
            })
            .collect();
        cards.sort_by_key(|e| e.file_name());

        cards
            .into_iter()
            .filter_map(|entry| {
                let device_path = entry.path().join("device");
                let uevent = std::fs::read_to_string(device_path.join("uevent")).ok()?;
                uevent
                    .lines()
                    .any(|l| l == "DRIVER=amdgpu")
                    .then_some(device_path)
            })
            .collect()
    }

    /// Read the "edge" (or first available) temperature in °C from hwmon.
    fn read_temperature(&self) -> u32 {
        if let Some(hw_mon) = self.gpu_handle.hw_monitors.first() {
            let temps = hw_mon.get_temps();
            // Prefer "edge", fall back to any available sensor
            if let Some(t) = temps.get("edge") {
                return t.current.unwrap_or(0.0) as u32;
            }
            if let Some(t) = temps.values().next() {
                return t.current.unwrap_or(0.0) as u32;
            }
        }
        0
    }

    /// Fan speed as a percentage (0-100). Returns 0 for fanless iGPUs.
    fn read_fan_speed(&self) -> u32 {
        if let Some(hw_mon) = self.gpu_handle.hw_monitors.first() {
            // PWM value is 0-255, convert to percentage
            if let Ok(pwm) = hw_mon.get_fan_pwm() {
                return (pwm as u32 * 100) / 255;
            }
        }
        0
    }

    /// Parse strings like "8.0 GT/s PCIe" to PCIe generation.
    fn parse_pcie_gen(speed: &str) -> Option<u32> {
        let rate = speed
            .split_whitespace()
            .find_map(|part| part.parse::<f32>().ok())?;

        if rate >= 31.5 {
            Some(5)
        } else if rate >= 15.5 {
            Some(4)
        } else if rate >= 7.5 {
            Some(3)
        } else if rate >= 4.5 {
            Some(2)
        } else if rate >= 2.4 {
            Some(1)
        } else {
            None
        }
    }
}

impl GpuMonitor for AmdgpuMonitor {
    fn get_static_info(&self) -> GpuInfo {
        let name = self
            .gpu_handle
            .get_pci_id()
            .map(|(vendor, device)| format!("AMD GPU [{vendor}:{device}]"))
            .unwrap_or_else(|| "AMD GPU".to_string());

        let driver_version = self.gpu_handle.get_driver().to_string();

        // PCIe link width is reported as a string like "16" – parse to u32
        let pcie_width = self
            .gpu_handle
            .get_current_link_width()
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0);

        // PCIe speed string like "8.0 GT/s PCIe"
        let pcie_gen = self
            .gpu_handle
            .get_current_link_speed()
            .ok()
            .and_then(|s| Self::parse_pcie_gen(&s))
            .unwrap_or(0);

        GpuInfo {
            name,
            driver_version,
            pcie_gen,
            pcie_width,
            device_count: self.device_count,
            // amdgpu exposes per-process usage through DRM fdinfo, which this
            // backend does not read yet.
            per_process_supported: false,
        }
    }

    fn sample(&mut self) -> Result<(GpuData, Vec<ProcessInfo>), MonitorError> {
        let utilization = self.gpu_handle.get_busy_percent().unwrap_or(0) as f32;

        // VRAM – may be unavailable on iGPUs
        let memory_used =
            self.gpu_handle.get_used_vram().unwrap_or(0) as f64 / 1024.0 / 1024.0 / 1024.0;
        let memory_total =
            self.gpu_handle.get_total_vram().unwrap_or(0) as f64 / 1024.0 / 1024.0 / 1024.0;

        let temperature = self.read_temperature();

        // Clocks from hwmon
        let (gpu_clock, memory_clock) = if let Some(hw_mon) = self.gpu_handle.hw_monitors.first() {
            (
                hw_mon.get_gpu_clockspeed().unwrap_or(0) as u32,
                hw_mon.get_vram_clockspeed().unwrap_or(0) as u32,
            )
        } else {
            (0, 0)
        };

        // Power from hwmon
        let (power_usage, power_limit) = if let Some(hw_mon) = self.gpu_handle.hw_monitors.first() {
            let usage = hw_mon
                .get_power_average()
                .or_else(|_| hw_mon.get_power_input())
                .unwrap_or(0.0);
            let cap = hw_mon.get_power_cap().unwrap_or(0.0);
            (usage, cap)
        } else {
            (0.0, 0.0)
        };

        let fan_speed = self.read_fan_speed();

        let gpu_data = GpuData {
            timestamp: self.start_time.elapsed().as_secs_f64(),
            utilization,
            memory_used,
            memory_total,
            temperature,
            gpu_clock,
            memory_clock,
            power_usage,
            power_limit,
            fan_speed,
            // amdgpu sysfs does not expose PCIe throughput counters
            pcie_throughput_tx: 0.0,
            pcie_throughput_rx: 0.0,
            // amdgpu reports throttler status through the versioned binary
            // gpu_metrics blob, which this backend does not parse.
            throttle: None,
            power_capped_share: None,
        };

        // amdgpu_sysfs does not provide per-process GPU usage
        Ok((gpu_data, Vec::new()))
    }
}

// ── Factory ─────────────────────────────────────────────────────────────────

pub fn create_monitor() -> Result<Box<dyn GpuMonitor>, String> {
    // Try NVIDIA first
    let nvml_err = match NvmlMonitor::new(0) {
        Ok(monitor) => {
            println!("✅ NVML monitor initialized successfully.");
            return Ok(Box::new(monitor));
        }
        Err(e) => e,
    };

    // Try AMD (amdgpu driver via sysfs)
    let amd_err = match AmdgpuMonitor::new() {
        Ok(monitor) => {
            println!("✅ AMDGPU monitor initialized successfully.");
            return Ok(Box::new(monitor));
        }
        Err(e) => e,
    };

    // Keep both concrete errors: a broken NVIDIA driver install looks very
    // different from "no GPU present", and the user needs to know which.
    Err(format!(
        "NVIDIA (NVML): {nvml_err}\nAMD (amdgpu sysfs): {amd_err}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, memory_usage: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: format!("proc{pid}"),
            memory_usage,
        }
    }

    #[test]
    fn merge_keeps_distinct_pids_from_both_lists() {
        let merged = merge_process_lists(vec![proc(1, 100)], vec![proc(2, 200)]);
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(|p| p.pid == 1 && p.memory_usage == 100));
        assert!(merged.iter().any(|p| p.pid == 2 && p.memory_usage == 200));
    }

    #[test]
    fn merge_dedupes_shared_pid_keeping_max_memory() {
        let merged = merge_process_lists(vec![proc(7, 100)], vec![proc(7, 300)]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].memory_usage, 300);

        let merged = merge_process_lists(vec![proc(7, 500)], vec![proc(7, 300)]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].memory_usage, 500);
    }

    #[test]
    fn merge_with_empty_lists() {
        assert!(merge_process_lists(Vec::new(), Vec::new()).is_empty());
        let merged = merge_process_lists(Vec::new(), vec![proc(3, 42)]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].pid, 3);
    }

    // The comm/cmdline pairs below were read off /proc on a live desktop, so
    // they cover the shapes that actually reach the GPU process table.

    #[test]
    fn truncated_comm_is_completed_from_cmdline() {
        assert_eq!(
            pick_process_name("xdg-desktop-por", b"xdg-desktop-portal-gnome\0").as_deref(),
            Some("xdg-desktop-portal-gnome")
        );
        assert_eq!(
            pick_process_name("systemd-journal", b"/usr/lib/systemd/systemd-journald\0").as_deref(),
            Some("systemd-journald")
        );
    }

    #[test]
    fn truncated_comm_survives_an_interpreter_cmdline() {
        // python3 running a script that renamed itself: cmdline's argv[0] is
        // the interpreter, so the truncated comm is still the better label.
        assert_eq!(
            pick_process_name(
                "unattended-upgr",
                b"/usr/bin/python3\0/usr/bin/unattended-upgrade\0"
            )
            .as_deref(),
            Some("unattended-upgr")
        );
    }

    #[test]
    fn untruncated_comm_wins_over_a_branded_argv0() {
        assert_eq!(
            pick_process_name("code", b"/usr/share/code/Code\0--shared-files\0").as_deref(),
            Some("code")
        );
        assert_eq!(
            pick_process_name("claude-desktop", b"Claude\0--disable-logging\0").as_deref(),
            Some("claude-desktop")
        );
    }

    #[test]
    fn falls_back_to_cmdline_when_comm_is_unreadable() {
        assert_eq!(
            pick_process_name("", b"/usr/bin/gnome-shell\0").as_deref(),
            Some("gnome-shell")
        );
        // Kernel threads have an empty cmdline instead.
        assert_eq!(
            pick_process_name("kworker/0:1", b"").as_deref(),
            Some("kworker/0:1")
        );
        assert_eq!(pick_process_name("", b""), None);
    }

    #[test]
    fn only_argv0_is_used_from_a_long_cmdline() {
        let cmdline = b"/opt/google/chrome/chrome\0--type=gpu-process\0--ozone-platform=x11\0";
        assert_eq!(basename_of_argv0(cmdline).as_deref(), Some("chrome"));
        assert_eq!(basename_of_argv0(b"\0\0\0"), None);
    }

    const SECOND: u64 = 1_000_000_000;

    /// Feed `windows` one-second windows at the given utilization, each adding
    /// `capped_per_window` nanoseconds to the driver's cumulative counter.
    fn run_windows(
        acc: &mut PowerCapAccounting,
        windows: u32,
        utilization: f32,
        capped_per_window: u64,
    ) {
        let mut counter = acc.last_counter_nanos.unwrap_or(0);
        for _ in 0..windows {
            for _ in 0..10 {
                acc.observe_sample(utilization);
            }
            counter += capped_per_window;
            acc.close_window(counter, SECOND);
        }
    }

    #[test]
    fn first_window_only_establishes_a_baseline() {
        let mut acc = PowerCapAccounting::default();
        // The counter is cumulative since driver load, so its first absolute
        // value must not be charged to this session.
        acc.observe_sample(90.0);
        acc.close_window(900 * SECOND, SECOND);
        assert_eq!(acc.busy_nanos, 0);
        assert_eq!(acc.capped_nanos, 0);
    }

    #[test]
    fn idle_windows_are_not_counted() {
        let mut acc = PowerCapAccounting::default();
        // 5% utilization with the counter still advancing is the measured
        // idle-desktop case; charging it would report throttling at rest.
        run_windows(&mut acc, 30, 5.0, SECOND / 100);
        assert_eq!(acc.busy_nanos, 0);
        assert_eq!(acc.capped_share(), None);
    }

    #[test]
    fn busy_windows_yield_a_share_of_busy_time() {
        let mut acc = PowerCapAccounting::default();
        run_windows(&mut acc, 1, 95.0, 0); // baseline
        run_windows(&mut acc, 20, 95.0, SECOND / 10);
        assert_eq!(acc.busy_nanos, 20 * SECOND);
        let share = acc.capped_share().expect("enough busy time");
        assert!((share - 0.1).abs() < 1e-6, "got {share}");
    }

    #[test]
    fn share_is_withheld_until_busy_time_is_meaningful() {
        let mut acc = PowerCapAccounting::default();
        run_windows(&mut acc, 1, 95.0, 0); // baseline
        run_windows(&mut acc, 4, 95.0, SECOND / 10);
        assert_eq!(acc.capped_share(), None, "4 s of load is not a measurement");
        run_windows(&mut acc, 1, 95.0, SECOND / 10);
        assert!(acc.capped_share().is_some(), "5 s crosses the threshold");
    }

    #[test]
    fn share_stays_within_bounds_if_the_counter_outruns_the_window() {
        let mut acc = PowerCapAccounting::default();
        run_windows(&mut acc, 1, 95.0, 0); // baseline
                                           // A window that idled is skipped, but its counter growth still shows
                                           // up in the next busy window's delta, which can exceed the window.
        run_windows(&mut acc, 10, 95.0, 3 * SECOND);
        assert_eq!(acc.capped_share(), Some(1.0));
    }

    #[test]
    fn a_stalled_window_is_thrown_away_rather_than_counted() {
        let mut acc = PowerCapAccounting::default();
        run_windows(&mut acc, 1, 95.0, 0); // baseline
        run_windows(&mut acc, 10, 95.0, SECOND / 10);
        let (busy, capped) = (acc.busy_nanos, acc.capped_nanos);

        // Sampling stalls for a minute; the driver counter runs the whole
        // time. Charging that window would dilute the share with unobserved
        // time, so the window is dropped and the next reading re-baselines.
        for _ in 0..10 {
            acc.observe_sample(95.0);
        }
        acc.discard_window();
        acc.close_window(999 * SECOND, 61 * SECOND);
        assert_eq!((acc.busy_nanos, acc.capped_nanos), (busy, capped));

        // Measurement resumes cleanly afterwards.
        run_windows(&mut acc, 5, 95.0, SECOND / 10);
        assert_eq!(acc.busy_nanos, busy + 5 * SECOND);
    }

    #[test]
    fn a_counter_reset_does_not_underflow() {
        let mut acc = PowerCapAccounting::default();
        run_windows(&mut acc, 1, 95.0, 0);
        for _ in 0..10 {
            acc.observe_sample(95.0);
        }
        // A driver reload restarts the counter below the previous reading.
        acc.close_window(0, SECOND);
        assert_eq!(acc.capped_nanos, 0);
    }

    #[test]
    fn nvml_bits_map_to_reasons_worth_showing() {
        let idle = map_throttle_reasons(NvmlThrottleReasons::GPU_IDLE);
        assert_eq!(idle, ThrottleReasons::default(), "idle is not throttling");

        let capped = map_throttle_reasons(NvmlThrottleReasons::SW_POWER_CAP);
        assert!(capped.power_cap && !capped.thermal && !capped.hardware);

        let hot = map_throttle_reasons(
            NvmlThrottleReasons::HW_THERMAL_SLOWDOWN | NvmlThrottleReasons::HW_SLOWDOWN,
        );
        assert!(hot.thermal && hot.hardware && !hot.power_cap);

        // The software thermal bit is the one a consumer card asserts when it
        // settles at its temperature target, which is the everyday case; the
        // hardware bit is the emergency one. Dropping it would report a card
        // that is visibly held back by heat as "unthrottled".
        let warm = map_throttle_reasons(NvmlThrottleReasons::SW_THERMAL_SLOWDOWN);
        assert!(warm.thermal && !warm.hardware && !warm.power_cap);

        let brake = map_throttle_reasons(NvmlThrottleReasons::HW_POWER_BRAKE_SLOWDOWN);
        assert!(brake.hardware && !brake.power_cap);
    }

    #[test]
    fn parse_pcie_gen_maps_nominal_rates() {
        assert_eq!(AmdgpuMonitor::parse_pcie_gen("2.5 GT/s PCIe"), Some(1));
        assert_eq!(AmdgpuMonitor::parse_pcie_gen("5.0 GT/s PCIe"), Some(2));
        assert_eq!(AmdgpuMonitor::parse_pcie_gen("8.0 GT/s PCIe"), Some(3));
        assert_eq!(AmdgpuMonitor::parse_pcie_gen("16.0 GT/s PCIe"), Some(4));
        assert_eq!(AmdgpuMonitor::parse_pcie_gen("32.0 GT/s PCIe"), Some(5));
    }

    #[test]
    fn parse_pcie_gen_rejects_unparseable_input() {
        assert_eq!(AmdgpuMonitor::parse_pcie_gen(""), None);
        assert_eq!(AmdgpuMonitor::parse_pcie_gen("Unknown"), None);
        assert_eq!(AmdgpuMonitor::parse_pcie_gen("GT/s"), None);
        // Below the Gen1 threshold
        assert_eq!(AmdgpuMonitor::parse_pcie_gen("1.0 GT/s"), None);
    }
}

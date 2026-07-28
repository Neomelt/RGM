use crate::data::{GpuData, GpuInfo, ProcessInfo};
use nvml_wrapper::enum_wrappers::device::{Clock, PcieUtilCounter, TemperatureSensor};
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

pub trait GpuMonitor: Send + Sync {
    fn get_static_info(&self) -> GpuInfo;
    fn sample(&self) -> Result<(GpuData, Vec<ProcessInfo>), MonitorError>;
}

// ── NVIDIA Backend ──────────────────────────────────────────────────────────

pub struct NvmlMonitor {
    nvml: Nvml,
    device_index: u32,
    start_time: std::time::Instant,
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
            };
        };

        GpuInfo {
            name: device.name().unwrap_or_else(|_| "N/A".to_string()),
            driver_version,
            pcie_gen: device.current_pcie_link_gen().unwrap_or(0),
            pcie_width: device.current_pcie_link_width().unwrap_or(0),
            device_count,
        }
    }

    fn sample(&self) -> Result<(GpuData, Vec<ProcessInfo>), MonitorError> {
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

        let pcie_tx = device
            .pcie_throughput(PcieUtilCounter::Send)
            .map(|v| v as f64 / 1024.0)
            .unwrap_or(0.0);
        let pcie_rx = device
            .pcie_throughput(PcieUtilCounter::Receive)
            .map(|v| v as f64 / 1024.0)
            .unwrap_or(0.0);

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
            name: std::fs::read_to_string(format!("/proc/{}/comm", proc.pid))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
            memory_usage: match proc.used_gpu_memory {
                UsedGpuMemory::Used(v) => v,
                _ => 0,
            },
        })
        .collect()
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
        }
    }

    fn sample(&self) -> Result<(GpuData, Vec<ProcessInfo>), MonitorError> {
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

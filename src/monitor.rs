use crate::data::{GpuData, GpuInfo, ProcessInfo};
use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::{Clock, PcieUtilCounter, TemperatureSensor};
use nvml_wrapper::enums::device::UsedGpuMemory;
use thiserror::Error;

use amdgpu_sysfs::gpu_handle::GpuHandle;
use std::path::PathBuf;

#[derive(Error, Debug)]
pub enum MonitorError {
    #[error("NVML initialization failed: {0}")]
    NvmlInit(#[from] nvml_wrapper::error::NvmlError),
    #[error("Device not found at index {0}")]
    DeviceNotFound(u32),
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
        // Temporarily get the device object when needed
        let device = self.nvml.device_by_index(self.device_index).unwrap();

        GpuInfo {
            name: device.name().unwrap_or_else(|_| "N/A".to_string()),
            uuid: device.uuid().unwrap_or_else(|_| "N/A".to_string()),
            driver_version: self
                .nvml
                .sys_driver_version()
                .unwrap_or_else(|_| "N/A".to_string()),
            vbios_version: device.vbios_version().unwrap_or_else(|_| "N/A".to_string()),
            pcie_gen: device.current_pcie_link_gen().unwrap_or(0),
            pcie_width: device.current_pcie_link_width().unwrap_or(0),
        }
    }

    fn sample(&self) -> Result<(GpuData, Vec<ProcessInfo>), MonitorError> {
        // Temporarily get the device object when needed
        let device = self.nvml.device_by_index(self.device_index)?;

        let (util, mem, temp) = (
            device.utilization_rates()?,
            device.memory_info()?,
            device.temperature(TemperatureSensor::Gpu)?,
        );

        let gpu_clock = device.clock_info(Clock::Graphics).unwrap_or(0);
        let mem_clock = device.clock_info(Clock::Memory).unwrap_or(0);

        let (power_usage, power_limit) =
            match (device.power_usage(), device.power_management_limit()) {
                (Ok(usage), Ok(limit)) => (usage as f64 / 1000.0, limit as f64 / 1000.0),
                _ => (0.0, 0.0),
            };

        let fan_speed = device.fan_speed(0).unwrap_or(0);

        let (pcie_tx, pcie_rx) = match (
            device.pcie_throughput(PcieUtilCounter::Send),
            device.pcie_throughput(PcieUtilCounter::Receive),
        ) {
            (Ok(rx), Ok(tx)) => (tx as f64 / 1024.0, rx as f64 / 1024.0),
            _ => (0.0, 0.0),
        };

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

        let mut process_infos = Vec::new();
        if let Ok(procs) = device.running_graphics_processes() {
            for proc in procs {
                let proc_name = std::fs::read_to_string(format!("/proc/{}/comm", proc.pid))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "unknown".to_string());
                let memory_usage = match proc.used_gpu_memory {
                    UsedGpuMemory::Used(v) => v,
                    _ => 0,
                };
                process_infos.push(ProcessInfo {
                    pid: proc.pid,
                    name: proc_name,
                    memory_usage,
                    cpu_percent: 0.0,
                });
            }
        }

        Ok((gpu_data, process_infos))
    }
}

// ── AMD Backend ─────────────────────────────────────────────────────────────

pub struct AmdgpuMonitor {
    gpu_handle: GpuHandle,
    start_time: std::time::Instant,
}

impl AmdgpuMonitor {
    /// Try to find and initialise the first AMD GPU driven by `amdgpu`.
    pub fn new() -> Result<Self, MonitorError> {
        let sysfs_path = Self::find_amdgpu_device()
            .ok_or_else(|| MonitorError::SamplingFailed("No amdgpu device found".into()))?;

        let gpu_handle = GpuHandle::new_from_path(sysfs_path)
            .map_err(|e| MonitorError::SamplingFailed(format!("amdgpu_sysfs init: {e}")))?;

        Ok(Self {
            gpu_handle,
            start_time: std::time::Instant::now(),
        })
    }

    /// Scan `/sys/class/drm/card*/device/` for the first device using the
    /// `amdgpu` kernel driver.
    fn find_amdgpu_device() -> Option<PathBuf> {
        let drm_dir = std::fs::read_dir("/sys/class/drm").ok()?;
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

        for entry in cards {
            let device_path = entry.path().join("device");
            let uevent_path = device_path.join("uevent");
            if let Ok(uevent) = std::fs::read_to_string(&uevent_path) {
                if uevent.lines().any(|l| l == "DRIVER=amdgpu") {
                    return Some(device_path);
                }
            }
        }
        None
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
}

impl GpuMonitor for AmdgpuMonitor {
    fn get_static_info(&self) -> GpuInfo {
        let name = self
            .gpu_handle
            .get_pci_id()
            .map(|(vendor, device)| format!("AMD GPU [{vendor}:{device}]"))
            .unwrap_or_else(|| "AMD GPU".to_string());

        let driver_version = self.gpu_handle.get_driver().to_string();

        let vbios_version = self
            .gpu_handle
            .get_vbios_version()
            .unwrap_or_else(|_| "N/A".to_string());

        // PCIe link width is reported as a string like "16" – parse to u32
        let pcie_width = self
            .gpu_handle
            .get_current_link_width()
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0);

        // PCIe speed string like "8.0 GT/s PCIe" – extract gen number heuristically
        let pcie_gen = self
            .gpu_handle
            .get_current_link_speed()
            .ok()
            .and_then(|s| {
                let s = s.trim().to_string();
                if s.contains("32") || s.contains("5.0") {
                    Some(5)
                } else if s.contains("16") || s.contains("4.0") {
                    Some(4)
                } else if s.contains("8.0") {
                    Some(3)
                } else if s.contains("5.0") {
                    Some(2)
                } else if s.contains("2.5") {
                    Some(1)
                } else {
                    None
                }
            })
            .unwrap_or(0);

        GpuInfo {
            name,
            uuid: "N/A".to_string(),
            driver_version,
            vbios_version,
            pcie_gen,
            pcie_width,
        }
    }

    fn sample(&self) -> Result<(GpuData, Vec<ProcessInfo>), MonitorError> {
        let utilization = self.gpu_handle.get_busy_percent().unwrap_or(0) as f32;

        // VRAM – may be unavailable on iGPUs
        let memory_used = self.gpu_handle.get_used_vram().unwrap_or(0) as f64
            / 1024.0 / 1024.0 / 1024.0;
        let memory_total = self.gpu_handle.get_total_vram().unwrap_or(0) as f64
            / 1024.0 / 1024.0 / 1024.0;

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
        let (power_usage, power_limit) = if let Some(hw_mon) = self.gpu_handle.hw_monitors.first()
        {
            let usage = hw_mon.get_power_average()
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

pub fn create_monitor() -> Option<Box<dyn GpuMonitor>> {
    // Try NVIDIA first
    if let Ok(monitor) = NvmlMonitor::new(0) {
        println!("✅ NVML monitor initialized successfully.");
        return Some(Box::new(monitor));
    }

    // Try AMD (amdgpu driver via sysfs)
    if let Ok(monitor) = AmdgpuMonitor::new() {
        println!("✅ AMDGPU monitor initialized successfully.");
        return Some(Box::new(monitor));
    }

    println!("❌ No compatible GPU monitors found.");
    None
}

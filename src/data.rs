// GPU data structure, storing dynamic information
#[derive(Clone, Debug)]
pub struct GpuData {
    pub timestamp: f64,
    pub utilization: f32,
    pub memory_used: f64,
    pub memory_total: f64,
    pub temperature: u32,
    pub gpu_clock: u32,
    pub memory_clock: u32,
    pub power_usage: f64,
    pub power_limit: f64,
    pub fan_speed: u32,
    pub pcie_throughput_tx: f64,
    pub pcie_throughput_rx: f64,
}

// GPU information structure, storing static information
#[derive(Clone, Debug, Default)]
pub struct GpuInfo {
    pub name: String,
    pub pcie_gen: u32,
    pub pcie_width: u32,
    pub driver_version: String,
    /// Number of GPUs the backend can see; only device 0 is monitored today,
    /// so the UI labels the device "GPU 0 of N" when N > 1.
    pub device_count: u32,
    /// Whether this backend can enumerate per-process GPU memory at all. It
    /// separates "nothing is using the GPU" from "we cannot tell", which an
    /// empty process table alone would conflate.
    pub per_process_supported: bool,
}

// Process information structure, storing information about GPU processes
#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub memory_usage: u64,
}

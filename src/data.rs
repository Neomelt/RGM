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
    /// Why clocks are held below maximum right now, or `None` on a backend
    /// that cannot report it.
    pub throttle: Option<ThrottleReasons>,
    /// Share (0.0..=1.0) of this session's *busy* time that the power limit
    /// held clocks down. `None` until enough busy time has accumulated for the
    /// figure to mean anything.
    pub power_capped_share: Option<f32>,
}

/// Why the GPU is holding its clocks below maximum. Vendor-neutral: an AMD
/// implementation would fill the same fields from `gpu_metrics`.
///
/// NVML also reports GPU_IDLE, APPLICATIONS_CLOCKS_SETTING, SYNC_BOOST and
/// DISPLAY_CLOCK_SETTING. None of them describe a limit the user is running
/// into — an idle card is not being throttled — so they are not mapped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThrottleReasons {
    /// The power limit is capping clocks. Expected behaviour for a card under
    /// sustained load, and on its own not a fault.
    pub power_cap: bool,
    /// A thermal limit is active, in software or hardware.
    pub thermal: bool,
    /// Hardware slowdown, including the external power brake. The severe one.
    pub hardware: bool,
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

use crate::data::{GpuData, GpuInfo, ProcessInfo};
use crate::monitor::create_monitor;
use crossbeam_channel::{bounded, Receiver};
use eframe::egui::{self, Color32};
use egui_plot::{Legend, Line, Plot, PlotPoints};
use std::collections::VecDeque;
use std::time::Instant;
use std::{thread, time::Duration};

/// Sampling thread cadence; the UI repaint rate is throttled to match.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);

/// After this long without a fresh sample the UI flags the data as stale.
const STALE_AFTER: Duration = Duration::from_secs(2);

// Main application structure
pub struct RgmApp {
    data: VecDeque<GpuData>,
    receiver: Receiver<(GpuData, Vec<ProcessInfo>)>,
    display_duration: f64,
    gpu_info: GpuInfo,
    processes: Vec<ProcessInfo>,
    /// Set when no GPU monitor could be initialized; the UI then shows the
    /// error instead of metrics (a desktop-launched app has no visible stderr).
    init_error: Option<String>,
    /// Wall-clock time of the last received sample, used to flag stale data
    /// when the sampling thread keeps erroring (e.g. after a GPU reset).
    last_sample_at: Option<Instant>,
    /// Staleness baseline before any sample arrives, so sampling that fails
    /// from the very first attempt is also visible in the UI.
    started_at: Instant,
}

impl RgmApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (sender, receiver) = bounded(100);
        let data = VecDeque::with_capacity(120);
        let processes = Vec::new();

        let (gpu_info, init_error) = match create_monitor() {
            Ok(monitor) => {
                let gpu_info = monitor.get_static_info();
                thread::spawn(move || loop {
                    match monitor.sample() {
                        Ok((gpu_data, proc_infos)) => {
                            if sender.send((gpu_data, proc_infos)).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            eprintln!("Error sampling GPU data: {}", e);
                        }
                    }
                    thread::sleep(SAMPLE_INTERVAL);
                });
                (gpu_info, None)
            }
            Err(err) => {
                eprintln!("❌ No compatible GPU monitor found.\n{err}");
                (GpuInfo::default(), Some(err))
            }
        };

        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals.dark_mode = true;
        cc.egui_ctx.set_style(style);

        Self {
            data,
            receiver,
            display_duration: 10.0,
            gpu_info,
            processes,
            init_error,
            last_sample_at: None,
            started_at: Instant::now(),
        }
    }
}

impl eframe::App for RgmApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(err) = &self.init_error {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    ui.heading("⚠ No supported GPU detected");
                    ui.add_space(16.0);
                    ui.label(err);
                    ui.add_space(16.0);
                    ui.label(
                        "RGM requires an NVIDIA GPU with the official driver (NVML) \
                         or an AMD GPU using the amdgpu kernel driver.",
                    );
                });
            });
            return;
        }

        while let Ok((gpu_data, proc_infos)) = self.receiver.try_recv() {
            let now = gpu_data.timestamp;
            let window_start_time = (now - self.display_duration).max(0.0);
            self.data.push_back(gpu_data);
            while self
                .data
                .front()
                .is_some_and(|d| d.timestamp < window_start_time)
            {
                self.data.pop_front();
            }
            self.processes = proc_infos;
            self.last_sample_at = Some(Instant::now());
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("🚀 GPU Monitor");
            let device_label = if self.gpu_info.device_count > 1 {
                // Only device 0 is monitored; tell multi-GPU users the others exist.
                format!(
                    "GPU 0 of {}: {}",
                    self.gpu_info.device_count, self.gpu_info.name
                )
            } else {
                self.gpu_info.name.clone()
            };
            ui.label(format!(
                "{} - Driver: {}",
                device_label, self.gpu_info.driver_version
            ));
            let stale_msg = match self.last_sample_at {
                Some(last) if last.elapsed() > STALE_AFTER => Some(format!(
                    "⚠ Data is stale ({:.0}s old) — sampling is failing, see terminal output",
                    last.elapsed().as_secs_f64()
                )),
                None if self.started_at.elapsed() > STALE_AFTER => Some(format!(
                    "⚠ No samples received in {:.0}s — sampling is failing, see terminal output",
                    self.started_at.elapsed().as_secs_f64()
                )),
                _ => None,
            };
            if let Some(msg) = stale_msg {
                ui.label(egui::RichText::new(msg).color(Color32::from_rgb(255, 180, 0)));
            }
            ui.add_space(8.0);

            let latest = self.data.back();

            if let Some(latest) = latest {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "GPU Utilization: {}%",
                                    latest.utilization
                                ))
                                .color(Color32::GREEN)
                                .size(22.0)
                                .strong(),
                            );
                            ui.label(format!("Temperature: {}°C", latest.temperature));
                            ui.label(format!("Fan Speed: {}%", latest.fan_speed));
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.label(format!(
                                "Memory: {:.2}/{:.2} GB",
                                latest.memory_used, latest.memory_total
                            ));
                            ui.label(format!(
                                "Power: {:.2}/{:.2} W",
                                latest.power_usage, latest.power_limit
                            ));
                            ui.label(format!("GPU Clock: {} MHz", latest.gpu_clock));
                            ui.label(format!("Memory Clock: {} MHz", latest.memory_clock));
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.label(format!(
                                "PCIe: Gen {} x{}",
                                self.gpu_info.pcie_gen, self.gpu_info.pcie_width
                            ));
                            ui.label(format!("PCIe TX: {:.2} MB/s", latest.pcie_throughput_tx));
                            ui.label(format!("PCIe RX: {:.2} MB/s", latest.pcie_throughput_rx));
                        });
                    });
                });
            }

            ui.add_space(12.0);
            ui.separator();
            ui.heading("📈 Real-time GPU Metrics (Last 10 Seconds)");

            let latest_timestamp = self.data.back().map_or(0.0, |d| d.timestamp);
            let to_relative_points = |mapper: Box<dyn Fn(&GpuData) -> f64>| -> PlotPoints {
                self.data
                    .iter()
                    .map(|data| {
                        let x = latest_timestamp - data.timestamp;
                        [x.max(0.0), mapper(data)]
                    })
                    .collect()
            };
            let gpu_util_points: PlotPoints =
                to_relative_points(Box::new(|d| d.utilization as f64));
            let memory_points: PlotPoints = to_relative_points(Box::new(|d| {
                if d.memory_total > f64::EPSILON {
                    d.memory_used / d.memory_total * 100.0
                } else {
                    0.0
                }
            }));
            let temp_points: PlotPoints = to_relative_points(Box::new(|d| d.temperature as f64));
            let power_points: PlotPoints = self
                .data
                .iter()
                .filter(|data| data.power_limit > 0.0)
                .map(|data| {
                    let x = latest_timestamp - data.timestamp;
                    [x.max(0.0), data.power_usage / data.power_limit * 100.0]
                })
                .collect();

            Plot::new("gpu_metrics_plot")
                .view_aspect(2.5)
                .legend(Legend::default())
                .include_y(0.0)
                .include_y(100.0)
                .include_x(0.0)
                .include_x(self.display_duration)
                .x_axis_label("Seconds Ago (0 = now)")
                .show_x(true)
                .show_y(true)
                .show(ui, |plot_ui| {
                    plot_ui
                        .line(Line::new("GPU Utilization", gpu_util_points).color(Color32::GREEN));
                    plot_ui.line(
                        Line::new("Memory Usage (%)", memory_points)
                            .color(Color32::from_rgb(0, 128, 255)),
                    );
                    plot_ui.line(
                        Line::new("Temperature (°C)", temp_points)
                            .color(Color32::from_rgb(255, 128, 0)),
                    );
                    plot_ui.line(
                        Line::new("Power Usage (%)", power_points)
                            .color(Color32::from_rgb(255, 0, 128)),
                    );
                });

            ui.add_space(12.0);
            ui.separator();
            ui.heading("🧩 GPU Processes");
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    egui::Grid::new("processes_grid")
                        .striped(true)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("PID").strong());
                            ui.label(egui::RichText::new("Name").strong());
                            ui.label(egui::RichText::new("Memory (MB)").strong());
                            ui.end_row();
                            for proc in self.processes.iter() {
                                ui.label(proc.pid.to_string());
                                ui.label(&proc.name);
                                ui.label(format!(
                                    "{:.1}",
                                    proc.memory_usage as f64 / 1024.0 / 1024.0
                                ));
                                ui.end_row();
                            }
                        });
                });
        });

        // New data arrives every SAMPLE_INTERVAL; repainting faster than that
        // only re-renders identical frames. Input-driven repaints still fire
        // immediately, egui handles those on its own.
        ctx.request_repaint_after(SAMPLE_INTERVAL);
    }
}

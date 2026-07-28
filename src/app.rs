use crate::data::{GpuData, GpuInfo, ProcessInfo};
use crate::monitor::create_monitor;
use crossbeam_channel::{bounded, Receiver};
use eframe::egui::{self, Color32, RichText, Sense};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Line, Plot, PlotPoints};
use std::collections::VecDeque;
use std::time::Instant;
use std::{thread, time::Duration};

/// Sampling thread cadence; the UI repaint rate is throttled to match.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);

/// After this long without a fresh sample the UI flags the data as stale.
const STALE_AFTER: Duration = Duration::from_secs(2);

// Metric palette, validated for CVD separation and contrast on the dark
// surface in this on-screen adjacency order (blue, orange, aqua, yellow).
// One color per metric, used consistently across cards and sparklines.
const COL_UTIL: Color32 = Color32::from_rgb(0x39, 0x87, 0xE5);
const COL_TEMP: Color32 = Color32::from_rgb(0xD9, 0x59, 0x26);
const COL_MEM: Color32 = Color32::from_rgb(0x19, 0x9E, 0x70);
const COL_POWER: Color32 = Color32::from_rgb(0xC9, 0x85, 0x00);

const SURFACE: Color32 = Color32::from_rgb(0x1A, 0x1A, 0x19);
const CARD_FILL: Color32 = Color32::from_rgb(0x24, 0x24, 0x22);
const PLOT_BG: Color32 = Color32::from_rgb(0x14, 0x14, 0x13);
const TEXT_SECONDARY: Color32 = Color32::from_rgb(0xC3, 0xC2, 0xB7);
const WARN: Color32 = Color32::from_rgb(0xFF, 0xB4, 0x00);

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
                thread::spawn(move || {
                    let mut monitor = monitor;
                    loop {
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
                    }
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
        style.visuals.panel_fill = SURFACE;
        style.visuals.extreme_bg_color = PLOT_BG;
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
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

    fn header(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("GPU Monitor").size(20.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let device_label = if self.gpu_info.device_count > 1 {
                    // Only device 0 is monitored; tell multi-GPU users the others exist.
                    format!(
                        "GPU 0 of {}: {}",
                        self.gpu_info.device_count, self.gpu_info.name
                    )
                } else {
                    self.gpu_info.name.clone()
                };
                ui.label(
                    RichText::new(format!(
                        "{} · Driver {}",
                        device_label, self.gpu_info.driver_version
                    ))
                    .color(TEXT_SECONDARY)
                    .size(12.0),
                );
            });
        });

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
            ui.label(RichText::new(msg).color(WARN));
        }
    }

    fn stat_cards(&self, ui: &mut egui::Ui, latest: &GpuData) {
        ui.columns(4, |cols| {
            stat_card(
                &mut cols[0],
                COL_UTIL,
                "Utilization",
                format!("{:.0}%", latest.utilization),
                format!("GPU {} MHz", latest.gpu_clock),
                Some(latest.utilization / 100.0),
            );
            stat_card(
                &mut cols[1],
                COL_TEMP,
                "Temperature",
                format!("{}°C", latest.temperature),
                format!("Fan {}%", latest.fan_speed),
                Some(latest.temperature as f32 / 100.0),
            );
            let (mem_sub, mem_frac) = if latest.memory_total > f64::EPSILON {
                (
                    format!(
                        "of {:.2} GB · {} MHz",
                        latest.memory_total, latest.memory_clock
                    ),
                    Some((latest.memory_used / latest.memory_total) as f32),
                )
            } else {
                (format!("VRAM · {} MHz", latest.memory_clock), None)
            };
            stat_card(
                &mut cols[2],
                COL_MEM,
                "Memory",
                format!("{:.2} GB", latest.memory_used),
                mem_sub,
                mem_frac,
            );
            let (power_sub, power_frac) = if latest.power_limit > 0.0 {
                (
                    format!("limit {:.0} W", latest.power_limit),
                    Some((latest.power_usage / latest.power_limit) as f32),
                )
            } else {
                ("no reported limit".to_string(), None)
            };
            stat_card(
                &mut cols[3],
                COL_POWER,
                "Power",
                format!("{:.1} W", latest.power_usage),
                power_sub,
                power_frac,
            );
        });

        ui.label(
            RichText::new(format!(
                "PCIe Gen {} ×{} · TX {:.2} MB/s · RX {:.2} MB/s",
                self.gpu_info.pcie_gen,
                self.gpu_info.pcie_width,
                latest.pcie_throughput_tx,
                latest.pcie_throughput_rx
            ))
            .color(TEXT_SECONDARY)
            .size(11.0),
        );
    }

    fn sparklines(&self, ui: &mut egui::Ui, latest: &GpuData) {
        let latest_timestamp = self.data.back().map_or(0.0, |d| d.timestamp);
        // X is negative "seconds before now", so fresh data enters at the
        // right edge, the direction monitoring tools conventionally scroll.
        let points = |mapper: &dyn Fn(&GpuData) -> f64| -> PlotPoints {
            self.data
                .iter()
                .map(|d| [(d.timestamp - latest_timestamp).min(0.0), mapper(d)])
                .collect()
        };

        let plot_height = ((ui.available_height() - 220.0) / 2.0 - 24.0).clamp(90.0, 160.0);

        ui.columns(2, |cols| {
            self.sparkline(
                &mut cols[0],
                "plot_util",
                COL_UTIL,
                format!("Utilization — {:.0}%", latest.utilization),
                points(&|d| d.utilization as f64),
                Some(100.0),
                plot_height,
                false,
            );
            self.sparkline(
                &mut cols[1],
                "plot_temp",
                COL_TEMP,
                format!("Temperature — {}°C", latest.temperature),
                points(&|d| d.temperature as f64),
                Some(100.0),
                plot_height,
                false,
            );
        });
        ui.columns(2, |cols| {
            self.sparkline(
                &mut cols[0],
                "plot_mem",
                COL_MEM,
                format!("Memory — {:.2} GB", latest.memory_used),
                points(&|d| d.memory_used),
                (latest.memory_total > f64::EPSILON).then_some(latest.memory_total),
                plot_height,
                true,
            );
            self.sparkline(
                &mut cols[1],
                "plot_power",
                COL_POWER,
                format!("Power — {:.1} W", latest.power_usage),
                points(&|d| d.power_usage),
                (latest.power_limit > 0.0).then_some(latest.power_limit),
                plot_height,
                true,
            );
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn sparkline(
        &self,
        ui: &mut egui::Ui,
        id: &str,
        color: Color32,
        title: String,
        points: PlotPoints<'static>,
        y_max: Option<f64>,
        height: f32,
        show_x_label: bool,
    ) {
        ui.horizontal(|ui| {
            identity_dot(ui, color);
            ui.label(RichText::new(title).color(TEXT_SECONDARY).size(12.0));
        });
        let mut plot = Plot::new(id)
            .height(height)
            // Uniform axis width keeps the plot areas of all sparklines
            // left-aligned regardless of tick label widths (100 vs 10).
            .y_axis_min_width(34.0)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .include_x(-self.display_duration)
            .include_x(0.0)
            .include_y(0.0)
            // Horizontal gridlines guide value reading; vertical ones only
            // clutter a 10-second sliding window.
            .show_grid([false, true])
            .show_axes([show_x_label, true])
            .show_x(true)
            .show_y(true);
        if let Some(m) = y_max {
            plot = plot.include_y(m);
        }
        if show_x_label {
            plot = plot
                .x_grid_spacer(|_input| {
                    [-10.0, -5.0, 0.0]
                        .iter()
                        .map(|&value| egui_plot::GridMark {
                            value,
                            step_size: 5.0,
                        })
                        .collect()
                })
                .x_axis_formatter(|mark, _range| {
                    if mark.value >= -0.01 {
                        "now".to_string()
                    } else {
                        format!("{:.0}s ago", -mark.value)
                    }
                });
        }
        plot.show(ui, |plot_ui| {
            plot_ui.line(
                Line::new("", points)
                    .color(color)
                    .width(2.0_f32)
                    .fill(0.0_f32)
                    .fill_alpha(0.15_f32),
            );
        });
    }

    fn process_table(&self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Processes").color(TEXT_SECONDARY).size(12.0));
        let mut procs: Vec<&ProcessInfo> = self.processes.iter().collect();
        procs.sort_by_key(|p| std::cmp::Reverse(p.memory_usage));

        TableBuilder::new(ui)
            .striped(true)
            .column(Column::exact(64.0))
            .column(Column::remainder())
            .column(Column::exact(110.0))
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.label(RichText::new("PID").strong().size(12.0));
                });
                header.col(|ui| {
                    ui.label(RichText::new("Name").strong().size(12.0));
                });
                header.col(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("Memory (MB)").strong().size(12.0));
                    });
                });
            })
            .body(|mut body| {
                for proc in procs {
                    body.row(18.0, |mut row| {
                        row.col(|ui| {
                            ui.label(proc.pid.to_string());
                        });
                        row.col(|ui| {
                            ui.label(&proc.name);
                        });
                        row.col(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(format!(
                                        "{:.1}",
                                        proc.memory_usage as f64 / 1024.0 / 1024.0
                                    ));
                                },
                            );
                        });
                    });
                }
            });
    }
}

/// A compact stat card: identity dot + label, large value, secondary line,
/// and an optional slim fill bar when the metric has a natural maximum.
fn stat_card(
    ui: &mut egui::Ui,
    accent: Color32,
    label: &str,
    value: String,
    sub: String,
    frac: Option<f32>,
) {
    egui::Frame::new()
        .fill(CARD_FILL)
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                identity_dot(ui, accent);
                ui.label(RichText::new(label).color(TEXT_SECONDARY).size(12.0));
            });
            ui.label(RichText::new(value).size(22.0).strong());
            ui.label(RichText::new(sub).color(TEXT_SECONDARY).size(11.0));
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 4.0), Sense::hover());
            if let Some(f) = frac {
                let painter = ui.painter();
                painter.rect_filled(rect, 2, Color32::from_gray(50));
                let mut fill = rect;
                fill.set_width(rect.width() * f.clamp(0.0, 1.0));
                painter.rect_filled(fill, 2, accent);
            }
        });
}

fn identity_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
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
            self.header(ui);

            if let Some(latest) = self.data.back().cloned() {
                ui.add_space(4.0);
                self.stat_cards(ui, &latest);
                ui.add_space(6.0);
                self.sparklines(ui, &latest);
                ui.add_space(6.0);
                ui.separator();
                self.process_table(ui);
            } else {
                ui.add_space(8.0);
                ui.label(RichText::new("Waiting for the first sample…").color(TEXT_SECONDARY));
            }
        });

        // New data arrives every SAMPLE_INTERVAL; repainting faster than that
        // only re-renders identical frames. Input-driven repaints still fire
        // immediately, egui handles those on its own.
        ctx.request_repaint_after(SAMPLE_INTERVAL);
    }
}

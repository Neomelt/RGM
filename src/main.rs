mod app;
mod data;
mod monitor;

use app::RgmApp;
use eframe::egui::ViewportBuilder;

fn main() {
    let native_options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([1000.0, 700.0])
            // Without this, egui-winit never calls set_app_id on Wayland, so
            // the window has no app_id for compositors to match against
            // rgm.desktop or a tiling rule. Must equal the desktop file's
            // basename. X11 already derives WM_CLASS from the binary name.
            .with_app_id("rgm"),
        ..Default::default()
    };

    eframe::run_native(
        "RGM",
        native_options,
        Box::new(|cc| Ok(Box::new(RgmApp::new(cc)))),
    )
    .expect("Failed to start application");
}

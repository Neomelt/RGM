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
            // basename.
            //
            // It is not free on X11: egui-winit passes the id through as
            // winit's shared `name` field with an empty instance, so WM_CLASS
            // goes from ("rgm", "rgm") to ("", "rgm"). Rules matching the
            // class still work, ones matching the instance do not, and eframe
            // exposes no way to set them separately.
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

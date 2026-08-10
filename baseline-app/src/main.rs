mod app;
mod baseline_mode;
mod calibration_mode;
mod channel;
mod flux_mode;
mod observation_mode;
mod plot_export;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1400.0, 900.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Baseline Mode",
        options,
        Box::new(|_cc| Ok(Box::new(app::App::default()))),
    )
}

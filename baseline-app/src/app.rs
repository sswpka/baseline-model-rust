//! Top-level app

use crate::baseline_mode::BaselineMode;
use crate::calibration_mode::CalibrationMode;
use crate::flux_mode::FluxMode;
use crate::observation_mode::ObservationMode;
use crate::plot_export::PlotExportQueue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Baseline,
    Calibration,
    Flux,
    Observation,
}

pub struct App {
    mode: Mode,
    baseline: BaselineMode,
    calibration: CalibrationMode,
    flux: FluxMode,
    observation: ObservationMode,
    export: PlotExportQueue,
}

impl Default for App {
    fn default() -> Self {
        Self {
            mode: Mode::Baseline,
            baseline: BaselineMode::default(),
            calibration: CalibrationMode::default(),
            flux: FluxMode::default(),
            observation: ObservationMode::default(),
            export: PlotExportQueue::default(),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.export.process_screenshot(ctx);

        egui::TopBottomPanel::top("mode_nav").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.mode, Mode::Baseline, "Baseline");
                ui.selectable_value(&mut self.mode, Mode::Calibration, "Calibration");
                ui.selectable_value(&mut self.mode, Mode::Flux, "Flux");
                ui.selectable_value(&mut self.mode, Mode::Observation, "Observation");
            });
        });

        match self.mode {
            Mode::Baseline => self.baseline.update(ctx, &mut self.export),
            Mode::Calibration => self.calibration.update(ctx, &mut self.export),
            Mode::Flux => self.flux.update(ctx, &mut self.export),
            Mode::Observation => self.observation.update(ctx, &mut self.export),
        }

        self.export.show_open_windows(ctx);
    }
}

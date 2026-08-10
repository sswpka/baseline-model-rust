//! Top-level app shell, matching `ModeSelectorWindow` + `UnifiedMainWindow`:
//! a persistent top navigation bar picks the mode, shown alongside that
//! mode's screen. All four modes are wired up; see each mode module's doc
//! comment for what was deliberately deferred (detail/zoom windows,
//! manual-fit UI, coincidence-matrix heatmap).

use crate::baseline_mode::BaselineMode;
use crate::calibration_mode::CalibrationMode;
use crate::flux_mode::FluxMode;
use crate::observation_mode::ObservationMode;

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
}

impl Default for App {
    fn default() -> Self {
        Self {
            mode: Mode::Baseline,
            baseline: BaselineMode::default(),
            calibration: CalibrationMode::default(),
            flux: FluxMode::default(),
            observation: ObservationMode::default(),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("mode_nav").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.mode, Mode::Baseline, "Baseline");
                ui.selectable_value(&mut self.mode, Mode::Calibration, "Calibration");
                ui.selectable_value(&mut self.mode, Mode::Flux, "Flux");
                ui.selectable_value(&mut self.mode, Mode::Observation, "Observation");
            });
        });

        match self.mode {
            Mode::Baseline => self.baseline.update(ctx),
            Mode::Calibration => self.calibration.update(ctx),
            Mode::Flux => self.flux.update(ctx),
            Mode::Observation => self.observation.update(ctx),
        }
    }
}

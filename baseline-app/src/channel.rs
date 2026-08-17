//! Port of the plotting-relevant subset of `ChannelViewModel` (manual-fit
//! mode with lock checkboxes and `OptimizeManual` are deferred - see project
//! notes; this covers the auto-fit display pipeline that `ProcessData` drives).

use crate::plot_export::{self, PlotExportQueue};
use egui::Color32;
use egui_plot::{Bar, BarChart, Line, Plot, PlotPoints};

#[derive(Debug, Clone)]
pub struct FitCurve {
    pub curve: Vec<f64>,
    pub color: Color32,
    pub label: String,
}

#[derive(Debug, Clone, Default)]
pub struct ChannelState {
    pub channel_index: usize,
    pub title: String,
    pub stats_text: String,
    pub bin_centers: Vec<f64>,
    pub counts: Vec<f64>,
    pub active_fits: Vec<FitCurve>,
    pub is_fitting: bool,
    pub is_log_scale: bool,

    // Primary-fit stats (from the last successfully computed fit, mirrors
    // ChannelViewModel's Mu/Sigma/Peak/FWHM/Resolution fields).
    pub mu: f64,
    pub sigma: f64,
    pub fwhm: f64,
    pub resolution: f64,
}

impl ChannelState {
    pub fn ui(&mut self, ui: &mut egui::Ui, export: &mut PlotExportQueue) {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.strong(&self.title);
                if self.is_fitting {
                    ui.spinner();
                }
                ui.checkbox(&mut self.is_log_scale, "log");
            });
            ui.label(&self.stats_text);

            let id_source = format!("channel_plot_{}", self.channel_index);
            let plot = Plot::new(id_source.clone())
                .height(220.0)
                .allow_scroll(true)
                .legend(egui_plot::Legend::default());

            plot_export::show(ui, export, &id_source, &self.title, plot, |plot_ui| {
                if !self.counts.is_empty() && self.bin_centers.len() == self.counts.len() {
                    // Data renders as a bar histogram (matching the original's
                    // `AddBar`), except in log scale where bars of near-zero
                    // height are unreadable, so it falls back to the original's
                    // marker-scatter line instead. Fit curves always render as
                    // a smooth line overlay on top.
                    if self.is_log_scale {
                        let points: PlotPoints = self
                            .bin_centers
                            .iter()
                            .zip(self.counts.iter())
                            .map(|(&x, &y)| [x, y.max(0.1).log10()])
                            .collect();
                        plot_ui.line(Line::new(points).name("Data").color(Color32::LIGHT_BLUE));

                        for fit in &self.active_fits {
                            if fit.curve.len() == self.bin_centers.len() {
                                let fit_points: PlotPoints = self
                                    .bin_centers
                                    .iter()
                                    .zip(fit.curve.iter())
                                    .map(|(&x, &y)| [x, y.max(0.1).log10()])
                                    .collect();
                                plot_ui
                                    .line(Line::new(fit_points).name(&fit.label).color(fit.color));
                            }
                        }
                    } else {
                        // Bins are evenly spaced (see `build_histogram`/`build_histogram_avg_centers`),
                        // so deriving width from the full span is equivalent to using adjacent
                        // centers but more robust to how exactly those centers were computed.
                        let n = self.bin_centers.len();
                        let bar_width = if n > 1 {
                            (self.bin_centers[n - 1] - self.bin_centers[0]) / (n - 1) as f64
                        } else {
                            1.0
                        };

                        // Baseline mode builds full 16384-bin histograms (unlike
                        // Calibration mode's 500), so both bars and any fit overlay are
                        // cropped to the same populated-index range here: zero-count bins
                        // draw nothing anyway (so cropping the bars doesn't change what's
                        // visible), and without also cropping the fit curve to match, its
                        // full 0..16384 span would win the plot's auto-bounds and squash
                        // the bars down to a sliver at one edge.
                        let first = self.counts.iter().position(|&y| y > 0.0);
                        let last = self.counts.iter().rposition(|&y| y > 0.0);
                        if let (Some(first), Some(last)) = (first, last) {
                            // Bars get their solid `fill` set directly (rather than via
                            // `BarChart::color`, which would fill at 20% opacity for a
                            // stroke pairing) so the histogram reads as solid-color; the
                            // chart-level `.color()` below only sets `default_color` for
                            // the legend swatch, since it skips bars that already have a
                            // fill/stroke.
                            let bars: Vec<Bar> = (first..=last)
                                .filter(|&i| self.counts[i] > 0.0)
                                .map(|i| {
                                    Bar::new(self.bin_centers[i], self.counts[i])
                                        .width(bar_width)
                                        .fill(Color32::LIGHT_BLUE)
                                        .stroke(egui::Stroke::NONE)
                                })
                                .collect();
                            plot_ui.bar_chart(
                                BarChart::new(bars).name("Data").color(Color32::LIGHT_BLUE),
                            );

                            for fit in &self.active_fits {
                                if fit.curve.len() == self.bin_centers.len() {
                                    let fit_points: PlotPoints = (first..=last)
                                        .map(|i| [self.bin_centers[i], fit.curve[i]])
                                        .collect();
                                    plot_ui.line(
                                        Line::new(fit_points).name(&fit.label).color(fit.color),
                                    );
                                }
                            }
                        }
                    }
                }
            });
        });
    }
}

/// Renders a block of channels (e.g. one detector direction) as two
/// equal-width sub-columns under a shared heading, so every plot in the
/// block gets the same area regardless of how much text/stats the
/// neighboring channel happens to have. Shared by Baseline and Calibration
/// mode, which both split their 16 channels into two 8-channel blocks.
pub fn channel_block_ui(
    ui: &mut egui::Ui,
    heading: &str,
    channels: &mut [ChannelState],
    export: &mut PlotExportQueue,
) {
    ui.vertical(|ui| {
        ui.heading(heading);
        ui.separator();
        ui.columns(2, |cols| {
            for (i, ch) in channels.iter_mut().enumerate() {
                ch.ui(&mut cols[i % 2], export);
            }
        });
    });
}

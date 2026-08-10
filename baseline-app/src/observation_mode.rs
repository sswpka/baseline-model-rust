//! Port of Observation mode (`ObservationViewModel` + `.Commands.cs` +
//! `.FileOperations.cs`). Deferred (see project notes): the
//! `ObservationDetailWindow` (per-histogram peak-locking), and the BGO
//! adaptive/Kalman/Z-score filters - this
//! covers file ingestion, Excel export, and the DSSD pulse-height/strip and
//! BGO gain histograms that `AnalyzeFiles`/`ObservationDataProcessor`
//! actually compute, switchable via the DSSD/X-Strip/Y-Strip/BGO view-mode
//! selector (mirrors the original's DSSD/BGO tabs and their inner
//! Pulse-Height/X-Strip/Y-Strip tabs). Unlike the original (a single
//! Gaussian/Lorentzian `ComboBox` per section), the fit-overlay controls
//! here are independent checkboxes - Gaussian, Lorentzian, and HEMG can all
//! be plotted at once, matching Baseline mode's fit UI.

use baseline_core::io;
use baseline_core::math::MathService;
use baseline_core::models::observation::{BgoLayer, DetectorLayer};
use baseline_core::observation::ObservationDataProcessor;
use egui::Color32;
use egui_plot::{Bar, BarChart, Legend, Line, Plot, PlotPoints};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

/// A fit curve computed over a cropped window of a histogram (see
/// `compute_fits`): `curve[i]` corresponds to channel `start + i`, not
/// channel `i` - unlike `channel::FitCurve`, which is always index-aligned
/// 1:1 with its full `bin_centers`.
struct ObsFitCurve {
    start: usize,
    curve: Vec<f64>,
    color: Color32,
    label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservationViewMode {
    DssdPulseHeight,
    XStrip,
    YStrip,
    Bgo,
}

enum WorkerMsg {
    Status(String, Color32),
    Busy(bool),
    InputFilesInfo(String),
    OutputFileName(String),
    HistogramData(HashMap<String, Vec<i32>>),
    LastSavedPath(String),
    Error(String),
}

pub struct ObservationMode {
    input_files: Vec<PathBuf>,
    input_files_info: String,
    output_file_name: String,
    output_directory_path: PathBuf,

    status_message: String,
    is_busy: bool,
    progress_value: f64,
    data_count_str: String,
    last_saved_file_path: String,

    view_mode: ObservationViewMode,
    selected_layer: DetectorLayer,
    selected_bgo_layer: BgoLayer,
    histogram_data: HashMap<String, Vec<i32>>,

    /// Mirrors the original's `TxtDSSDXMin`/`TxtDSSDXMax` textboxes: raw
    /// text so the field can be edited freely (including transiently
    /// empty/invalid states) while typing. Applied to the DSSD Pulse
    /// Height/X-Strip/Y-Strip views only, matching the original's shared
    /// per-tab textboxes. Left blank (or unparsable/inverted), the view
    /// shows the full channel range - same as before this control existed.
    dssd_x_min: String,
    dssd_x_max: String,

    show_gaussian_fit: bool,
    show_lorentzian_fit: bool,
    show_hemg_fit: bool,
    /// Fit curves computed lazily per histogram key and cached until the
    /// underlying histogram data or the fit checkboxes change - a fresh
    /// Levenberg-Marquardt solve per frame would be far too slow for
    /// immediate-mode redraw.
    fit_cache: HashMap<String, Vec<ObsFitCurve>>,
    math: MathService,

    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
}

impl Default for ObservationMode {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let output_directory_path = std::env::var_os("USERPROFILE")
            .map(|p| PathBuf::from(p).join("Documents").join("BaselineModeOutputs"))
            .unwrap_or_else(|| PathBuf::from("."));

        Self {
            input_files: Vec::new(),
            input_files_info: "No files selected".to_string(),
            output_file_name: String::new(),
            output_directory_path,
            status_message: "Ready".to_string(),
            is_busy: false,
            progress_value: 0.0,
            data_count_str: "-".to_string(),
            last_saved_file_path: String::new(),
            view_mode: ObservationViewMode::DssdPulseHeight,
            selected_layer: DetectorLayer::L1,
            selected_bgo_layer: BgoLayer::L3,
            histogram_data: HashMap::new(),
            dssd_x_min: String::new(),
            dssd_x_max: String::new(),
            show_gaussian_fit: true,
            show_lorentzian_fit: false,
            show_hemg_fit: false,
            fit_cache: HashMap::new(),
            math: MathService::new(),
            tx,
            rx,
        }
    }
}

impl ObservationMode {
    pub fn update(&mut self, ctx: &egui::Context, export: &mut crate::plot_export::PlotExportQueue) {
        self.drain_messages();

        egui::TopBottomPanel::top("observation_top").show(ctx, |ui| {
            ui.heading("Observation Mode");
            ui.separator();
            ui.label(&self.input_files_info);
            if ui.add_enabled(!self.is_busy, egui::Button::new("Select Files...")).clicked() {
                self.select_files();
            }

            ui.horizontal(|ui| {
                ui.label("Output name:");
                ui.text_edit_singleline(&mut self.output_file_name);
            });
            ui.horizontal(|ui| {
                ui.label("Output dir:");
                ui.monospace(self.output_directory_path.display().to_string());
                if ui.button("Browse...").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Select Output Root Folder").pick_folder() {
                        self.output_directory_path = dir;
                    }
                }
            });

            ui.horizontal(|ui| {
                if ui.add_enabled(!self.is_busy && !self.input_files.is_empty(), egui::Button::new("Convert to Excel")).clicked() {
                    self.convert_files_to_excel();
                }
                if ui.add_enabled(!self.is_busy && !self.input_files.is_empty(), egui::Button::new("Analyze Files")).clicked() {
                    self.analyze_files();
                }
                if ui.button("Reset").clicked() {
                    self.reset();
                }
            });

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label("View:");
                ui.selectable_value(&mut self.view_mode, ObservationViewMode::DssdPulseHeight, "DSSD Pulse Height");
                ui.selectable_value(&mut self.view_mode, ObservationViewMode::XStrip, "X-Strip");
                ui.selectable_value(&mut self.view_mode, ObservationViewMode::YStrip, "Y-Strip");
                ui.selectable_value(&mut self.view_mode, ObservationViewMode::Bgo, "BGO");
            });

            ui.horizontal(|ui| {
                if self.view_mode == ObservationViewMode::Bgo {
                    egui::ComboBox::from_label("BGO Layer")
                        .selected_text(format!("{:?}", self.selected_bgo_layer))
                        .show_ui(ui, |ui| {
                            for layer in [BgoLayer::L3, BgoLayer::L4, BgoLayer::L5] {
                                ui.selectable_value(&mut self.selected_bgo_layer, layer, format!("{layer:?}"));
                            }
                        });
                } else {
                    egui::ComboBox::from_label("DSSD Layer")
                        .selected_text(format!("{:?}", self.selected_layer))
                        .show_ui(ui, |ui| {
                            for layer in [DetectorLayer::L1, DetectorLayer::L2, DetectorLayer::L6, DetectorLayer::L7] {
                                ui.selectable_value(&mut self.selected_layer, layer, format!("{layer:?}"));
                            }
                        });

                    ui.label("X Min:");
                    ui.add(egui::TextEdit::singleline(&mut self.dssd_x_min).desired_width(60.0));
                    ui.label("X Max:");
                    ui.add(egui::TextEdit::singleline(&mut self.dssd_x_max).desired_width(60.0));
                    if ui.button("Clear Range").clicked() {
                        self.dssd_x_min.clear();
                        self.dssd_x_max.clear();
                    }
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label("Fits:");
                if ui.checkbox(&mut self.show_gaussian_fit, "Gaussian").changed() {
                    self.fit_cache.clear();
                }
                if ui.checkbox(&mut self.show_lorentzian_fit, "Lorentzian").changed() {
                    self.fit_cache.clear();
                }
                if ui.checkbox(&mut self.show_hemg_fit, "HEMG").changed() {
                    self.fit_cache.clear();
                }
            });

            ui.separator();
            ui.colored_label(Color32::LIGHT_GRAY, &self.status_message);
            if self.is_busy {
                ui.add(egui::ProgressBar::new((self.progress_value / 100.0) as f32).show_percentage());
            }
            ui.label(format!("Data count: {}", self.data_count_str));
            if !self.last_saved_file_path.is_empty() {
                ui.label(format!("Saved: {}", self.last_saved_file_path));
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.view_mode {
                ObservationViewMode::DssdPulseHeight => self.dssd_pulse_height_ui(ui, export),
                ObservationViewMode::XStrip => self.strip_grid_ui(ui, 'X', export),
                ObservationViewMode::YStrip => self.strip_grid_ui(ui, 'Y', export),
                ObservationViewMode::Bgo => self.bgo_ui(ui, export),
            });
        });

        if self.is_busy {
            ctx.request_repaint();
        }
    }

    /// Parses `dssd_x_min`/`dssd_x_max` into an active view range, mirroring
    /// the original's `TxtDSSDXMin`/`TxtDSSDXMax`-driven zoom in
    /// `PlotHistogram`/`PlotStripHistogram`. Returns `None` (full range,
    /// today's behavior) unless both fields hold a valid `min < max` pair.
    fn dssd_x_range(&self) -> Option<(f64, f64)> {
        let min = self.dssd_x_min.trim().parse::<f64>().ok()?;
        let max = self.dssd_x_max.trim().parse::<f64>().ok()?;
        (max > min).then_some((min, max))
    }

    fn dssd_pulse_height_ui(&mut self, ui: &mut egui::Ui, export: &mut crate::plot_export::PlotExportQueue) {
        let layer_name = format!("{:?}", self.selected_layer);
        let x_key = format!("DSSD{layer_name}_X");
        let y_key = format!("DSSD{layer_name}_Y");
        let x_range = self.dssd_x_range();

        ui.label(format!("{layer_name} - Pulse Height X"));
        self.histogram_plot(ui, &x_key, "obs_x_plot", x_range, export);
        ui.separator();
        ui.label(format!("{layer_name} - Pulse Height Y"));
        self.histogram_plot(ui, &y_key, "obs_y_plot", x_range, export);
    }

    /// `axis` is `'X'` or `'Y'`, selecting the X-Strip or Y-Strip view.
    fn strip_grid_ui(&mut self, ui: &mut egui::Ui, axis: char, export: &mut crate::plot_export::PlotExportQueue) {
        let layer_name = format!("{:?}", self.selected_layer);
        ui.label(format!("{layer_name} - {axis}-Strip Pulse Height (1-8)"));
        ui.separator();
        let x_range = self.dssd_x_range();

        ui.columns(2, |cols| {
            for strip in 1..=8 {
                let key = format!("DSSD{layer_name}_Strip{axis}{strip}");
                let col = &mut cols[(strip - 1) % 2];
                col.label(format!("Strip {axis}{strip}"));
                self.strip_bar_plot(col, &key, &format!("obs_strip_{axis}{strip}"), x_range, export);
                col.separator();
            }
        });
    }

    fn bgo_ui(&mut self, ui: &mut egui::Ui, export: &mut crate::plot_export::PlotExportQueue) {
        let layer_name = format!("{:?}", self.selected_bgo_layer);
        let high_key = format!("BGO{layer_name}_High");
        let low_key = format!("BGO{layer_name}_Low");

        ui.columns(2, |cols| {
            cols[0].label(format!("{layer_name} - BGO High Gain"));
            self.strip_bar_plot(&mut cols[0], &high_key, "obs_bgo_high", None, export);
            cols[1].label(format!("{layer_name} - BGO Low Gain"));
            self.strip_bar_plot(&mut cols[1], &low_key, "obs_bgo_low", None, export);
        });
    }

    fn histogram_plot(&mut self, ui: &mut egui::Ui, key: &str, id: &str, x_range: Option<(f64, f64)>, export: &mut crate::plot_export::PlotExportQueue) {
        self.histogram_plot_sized(ui, key, id, 260.0, x_range, export);
    }

    /// Bar-chart rendering (one bar per raw ADC channel, matching the
    /// original's `AddBar(hist, binMidpoints)`) at a configurable height,
    /// with any enabled Gaussian/Lorentzian/HEMG fits overlaid as line
    /// curves on top (mirrors the `PlotStripHistogram`/`PlotBGOHistogram`
    /// bar+fit rendering). `x_range`, when set (see `dssd_x_range`),
    /// restricts both the bars and the fit curve to `[min, max]` - mirroring
    /// the original's `TxtDSSDXMin`/`TxtDSSDXMax`-driven zoom - by leaving
    /// out-of-range points out of what's drawn, so the plot's auto-bounds
    /// settle on that window. Zero-count channels are skipped: they draw
    /// nothing anyway, and real data only ever populates a narrow window of
    /// the 16384-channel range, so this keeps the draw count small without
    /// changing what's visible.
    fn histogram_plot_sized(&mut self, ui: &mut egui::Ui, key: &str, id: &str, height: f32, x_range: Option<(f64, f64)>, export: &mut crate::plot_export::PlotExportQueue) {
        self.ensure_fits(key);
        let hist = self.histogram_data.get(key);
        let fits = self.fit_cache.get(key);
        let in_range = |x: f64| x_range.map_or(true, |(lo, hi)| x >= lo && x <= hi);

        let plot = Plot::new(id).height(height).legend(Legend::default());
        crate::plot_export::show(ui, export, id, id, plot, |plot_ui| {
            if let Some(hist) = hist {
                let bars: Vec<Bar> = hist
                    .iter()
                    .enumerate()
                    .filter(|&(x, &c)| c > 0 && in_range(x as f64))
                    .map(|(x, &c)| Bar::new(x as f64, c as f64).width(1.0).fill(Color32::LIGHT_BLUE))
                    .collect();
                plot_ui.bar_chart(BarChart::new(bars).name("Data"));
            }
            if let Some(fits) = fits {
                for fit in fits {
                    let points: PlotPoints = fit
                        .curve
                        .iter()
                        .enumerate()
                        .map(|(i, &y)| ((fit.start + i) as f64, y))
                        .filter(|&(x, _)| in_range(x))
                        .map(|(x, y)| [x, y])
                        .collect();
                    plot_ui.line(Line::new(points).name(&fit.label).color(fit.color));
                }
            }
        });
    }

    fn strip_bar_plot(&mut self, ui: &mut egui::Ui, key: &str, id: &str, x_range: Option<(f64, f64)>, export: &mut crate::plot_export::PlotExportQueue) {
        self.histogram_plot_sized(ui, key, id, 180.0, x_range, export);

        let hist = self.histogram_data.get(key);
        let counts: i64 = hist.map(|h| h.iter().map(|&c| c as i64).sum()).unwrap_or(0);
        let peak_channel = hist.and_then(|h| h.iter().enumerate().max_by_key(|&(_, &c)| c).filter(|&(_, &c)| c > 0).map(|(i, _)| i));

        match peak_channel {
            Some(ch) => ui.label(format!("Peak: {ch}   Counts: {counts}")),
            None => ui.label("Peak: -   Counts: 0"),
        };
    }

    /// Computes (and caches) the enabled fit curves for `key`'s histogram,
    /// if not already cached. The cache is cleared whenever the fit
    /// checkboxes or the underlying histogram data change (see
    /// `drain_messages`), so a cache hit here always reflects the current
    /// selection.
    fn ensure_fits(&mut self, key: &str) {
        if self.fit_cache.contains_key(key) {
            return;
        }
        let fits = self.compute_fits(key);
        self.fit_cache.insert(key.to_string(), fits);
    }

    /// Crops to a window around the histogram's peak before fitting,
    /// mirroring the original's `win=100` peak-crop in
    /// `RefreshDSSDPlots`/`RefreshBGOPlots`: a Levenberg-Marquardt solve
    /// over the full mostly-zero 16384-channel histogram both converges
    /// poorly and is far slower than it needs to be, and an uncropped fit
    /// curve (`vec![0.0; 16384]` outside the peak) would blow out the
    /// plot's auto-bounds and squash the populated-channels-only bar chart
    /// down to a sliver.
    const FIT_WINDOW: usize = 100;

    fn compute_fits(&self, key: &str) -> Vec<ObsFitCurve> {
        if !(self.show_gaussian_fit || self.show_lorentzian_fit || self.show_hemg_fit) {
            return Vec::new();
        }
        let Some(hist) = self.histogram_data.get(key) else {
            return Vec::new();
        };
        let Some((peak_idx, _)) = hist.iter().enumerate().max_by_key(|&(_, &c)| c).filter(|&(_, &c)| c > 0) else {
            return Vec::new();
        };

        let start = peak_idx.saturating_sub(Self::FIT_WINDOW);
        let end = (peak_idx + Self::FIT_WINDOW + 1).min(hist.len());
        if end - start < 5 {
            return Vec::new();
        }

        let x_data: Vec<f64> = (start..end).map(|i| i as f64).collect();
        let y_data: Vec<f64> = hist[start..end].iter().map(|&c| c as f64).collect();

        let mut fits = Vec::new();
        if self.show_gaussian_fit {
            let res = self.math.gaussian_fit(&x_data, &y_data);
            if res.is_valid && !res.fit_curve.is_empty() {
                fits.push(ObsFitCurve { start, curve: res.fit_curve, color: Color32::from_rgb(50, 220, 50), label: "Gaussian".to_string() });
            }
        }
        if self.show_lorentzian_fit {
            let res = self.math.lorentzian_fit(&x_data, &y_data);
            if res.is_valid && !res.fit_curve.is_empty() {
                fits.push(ObsFitCurve { start, curve: res.fit_curve, color: Color32::from_rgb(0, 220, 220), label: "Lorentzian".to_string() });
            }
        }
        if self.show_hemg_fit {
            let res = self.math.hemg_double_sided_fit(&x_data, &y_data, None, None);
            if res.is_valid && !res.fit_curve.is_empty() {
                fits.push(ObsFitCurve { start, curve: res.fit_curve, color: Color32::RED, label: "HEMG".to_string() });
            }
        }
        fits
    }

    fn drain_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Status(s, _c) => self.status_message = s,
                WorkerMsg::Busy(b) => self.is_busy = b,
                WorkerMsg::InputFilesInfo(s) => self.input_files_info = s,
                WorkerMsg::OutputFileName(s) => self.output_file_name = s,
                WorkerMsg::HistogramData(data) => {
                    // Every processed particle contributes exactly one entry to each
                    // layer's pulse-height histograms, so any single layer's X series
                    // (here L1) gives the total particle count without double-counting
                    // across the now much larger strip/BGO series in `data`.
                    self.data_count_str = data.get("DSSDL1_X").map(|v| v.iter().sum::<i32>()).unwrap_or(0).to_string();
                    self.histogram_data = data;
                    self.fit_cache.clear();
                }
                WorkerMsg::LastSavedPath(p) => self.last_saved_file_path = p,
                WorkerMsg::Error(e) => {
                    self.status_message = e;
                    self.is_busy = false;
                }
            }
        }
    }

    fn reset(&mut self) {
        self.input_files.clear();
        self.input_files_info = "No files selected".to_string();
        self.output_file_name.clear();
        self.histogram_data.clear();
        self.fit_cache.clear();
        self.data_count_str = "-".to_string();
        self.last_saved_file_path.clear();
        self.status_message = "Ready".to_string();
        self.progress_value = 0.0;
    }

    fn select_files(&mut self) {
        let Some(files) = rfd::FileDialog::new().add_filter("Text Files", &["txt"]).pick_files() else {
            return;
        };

        if files.len() == 1 {
            self.output_file_name = files[0].file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            self.input_files = files;
            self.input_files_info = "1 file selected.".to_string();
            self.status_message = "1 file selected.".to_string();
        } else {
            self.is_busy = true;
            self.status_message = format!("Combining {} files...", files.len());
            let tx = self.tx.clone();
            std::thread::spawn(move || combine_files_worker(files, tx));
        }
    }

    fn convert_files_to_excel(&mut self) {
        if self.output_file_name.trim().is_empty() {
            self.status_message = "Please provide a valid output Excel file name.".to_string();
            return;
        }
        self.is_busy = true;
        self.status_message = "Converting to Excel...".to_string();

        let files = self.input_files.clone();
        let output_name = self.output_file_name.clone();
        let output_dir = self.output_directory_path.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || convert_to_excel_worker(files, output_name, output_dir, tx));
    }

    fn analyze_files(&mut self) {
        self.is_busy = true;
        self.progress_value = 0.0;
        self.status_message = "Processing...".to_string();

        let files = self.input_files.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || analyze_files_worker(files, tx));
    }
}

fn combine_files_worker(files: Vec<PathBuf>, tx: Sender<WorkerMsg>) {
    match io::file_helper::combine_files(&files, "CombinedData.txt") {
        Ok(path) => {
            let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let _ = tx.send(WorkerMsg::InputFilesInfo(format!("{} file(s) combined.", files.len())));
            let _ = tx.send(WorkerMsg::OutputFileName(stem));
            let _ = tx.send(WorkerMsg::Status(format!("Files combined successfully into {}", path.display()), Color32::from_rgb(50, 200, 50)));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Error combining files: {e}")));
        }
    }
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn convert_to_excel_worker(files: Vec<PathBuf>, output_name: String, output_dir: PathBuf, tx: Sender<WorkerMsg>) {
    let segments = match io::segment_filter::filter_e225_segments_from_files(
        &files,
        baseline_core::models::shared::AppConstants::PACKET_HEX_LENGTH,
    ) {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(e));
            let _ = tx.send(WorkerMsg::Busy(false));
            return;
        }
    };

    if segments.is_empty() {
        let _ = tx.send(WorkerMsg::Status("No valid segments found in the selected files.".to_string(), Color32::RED));
        let _ = tx.send(WorkerMsg::Busy(false));
        return;
    }

    let today = chrono::Local::now().date_naive();
    let dir = match baseline_core::baseline_processing::get_daily_output_directory(&output_dir, today) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(e.to_string()));
            let _ = tx.send(WorkerMsg::Busy(false));
            return;
        }
    };
    let final_path = dir.join(format!("{}.xlsx", output_name.trim()));

    match io::excel::save_lines_to_excel(&segments, &final_path) {
        Ok(()) => {
            let _ = tx.send(WorkerMsg::LastSavedPath(final_path.display().to_string()));
            let _ = tx.send(WorkerMsg::Status(format!("Successfully processed {} file(s). Saved to {}", files.len(), final_path.display()), Color32::from_rgb(50, 200, 50)));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(e));
        }
    }
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn analyze_files_worker(files: Vec<PathBuf>, tx: Sender<WorkerMsg>) {
    let mut processor = ObservationDataProcessor::new();
    match processor.process_files(&files) {
        Ok(histogram_data) => {
            let _ = tx.send(WorkerMsg::HistogramData(histogram_data));
            let _ = tx.send(WorkerMsg::Status("Processing complete!".to_string(), Color32::from_rgb(50, 200, 50)));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Error! {e}")));
        }
    }
    let _ = tx.send(WorkerMsg::Busy(false));
}

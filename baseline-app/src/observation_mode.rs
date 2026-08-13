//! Port of Observation mode (`ObservationViewModel` + `.Commands.cs` +
//! `.FileOperations.cs`). Deferred (see project notes): the
//! `ObservationDetailWindow` (per-histogram peak-locking), and the BGO
//! adaptive/Kalman/Z-score filters - this
//! covers file ingestion and the DSSD pulse-height/strip and
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
use chrono::{DateTime, Utc};
use egui::Color32;
use egui_plot::{Bar, BarChart, Legend, Line, Plot, PlotPoints};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

/// DSSD/BGO layers in the Data Table tab's fixed column order
const DSSD_TABLE_LAYERS: [DetectorLayer; 4] = [DetectorLayer::L1, DetectorLayer::L2, DetectorLayer::L6, DetectorLayer::L7];
const BGO_TABLE_LAYERS: [BgoLayer; 3] = [BgoLayer::L3, BgoLayer::L4, BgoLayer::L5];

/// Byte offsets within one particle's 34-byte payload that each DSSD X/Y
/// pulse height is decoded from - mirrors the index comments in
/// `ObservationDataProcessor::process_particle_data` ("L1: ... X-Ph idx 3-4,
/// Y-Ph idx 5-6", etc.) - shown in the Data Table's column headers.
fn dssd_byte_range(layer: DetectorLayer) -> (&'static str, &'static str) {
    match layer {
        DetectorLayer::L1 => ("3-4", "5-6"),
        DetectorLayer::L2 => ("8-9", "10-11"),
        DetectorLayer::L6 => ("25-26", "27-28"),
        DetectorLayer::L7 => ("30-31", "32-33"),
    }
}

/// Byte offsets within one particle's 34-byte payload that each BGO
/// High/Low gain is decoded from - mirrors `process_particle_data`'s "L3: H
/// idx 12-13, L idx 14-15" comments - shown in the Data Table's column
/// headers.
fn bgo_byte_range(layer: BgoLayer) -> (&'static str, &'static str) {
    match layer {
        BgoLayer::L3 => ("12-13", "14-15"),
        BgoLayer::L4 => ("16-17", "18-19"),
        BgoLayer::L5 => ("20-21", "22-23"),
    }
}

/// Whether the Data Table should show DSSD or BGO columns for the current
/// top `View:` selector - the two groups are mutually exclusive, matching
/// the Graph View tab's own DSSD-vs-BGO split (`ObservationViewMode`).
fn data_table_shows_dssd(view_mode: ObservationViewMode) -> bool {
    view_mode != ObservationViewMode::Bgo
}

/// One decoded particle event for the Data Table tab: every series (each
/// DSSD layer's (X, Y) pulse height, each BGO layer's (High, Low) gain) at
/// the timestamp they all share, since they're decoded from the same
/// particle payload (see `ParticleResult::time`). `dssd`/`bgo` are
/// positional against `DSSD_TABLE_LAYERS`/`BGO_TABLE_LAYERS` (converted once
/// from `ParticleResult`'s `HashMap`s in `analyze_files_worker`) rather than
/// keyed, since the Data Table tab re-reads every event every frame and a
/// large file can decode well over 100k of them.
///
/// Both `dssd` and `bgo` are raw ADC (0-16383), straight from
/// `ParticleResult::dssd_pulses`/`bgo_pulses` - the Data Table converts
/// `dssd` to volts at render/export time when `DssdDataUnit::Voltage` is
/// selected (see `adc_to_volts`, `data_table_ui`), rather than baking a unit
/// choice into the stored event.
#[derive(Debug, Clone)]
struct EventRow {
    /// Line header fields surrounding the timecode (see
    /// `ParticleResult`/`LineHeaderFields`) - same value for every particle
    /// on the same line. `packet_sync`/`data_type` are raw hex (e.g.
    /// `"E225"`); the rest are decimal.
    packet_sync: String,
    package_id: i32,
    packet_sequence: i32,
    packet_data_length: i32,
    time: DateTime<Utc>,
    data_type: String,
    dssd: [(i32, i32); 4],
    bgo: [(i32, i32); 3],
}

/// 14-bit ADC channel -> volts
fn adc_to_volts(channel: i32) -> f64 {
    channel as f64 / 16384.0 * 5.0
}

/// "YYYY-MM-DD HH:MM:SS.mmm", matching the timecode format
fn format_event_time(time: DateTime<Utc>) -> String {
    time.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// A fixed-width stand-in for any `format_event_time` output (same digit/
/// separator layout, so same rendered width regardless of the actual date)
/// - used to size the Time column against its content, not just its header.
const TIME_SAMPLE_TEXT: &str = "0000-00-00 00:00:00.000";

/// Rendered pixel width of `text` in the current `Body` text style, via
/// egui's memoized text layout - cheap even called repeatedly per frame.
fn text_render_width(ui: &egui::Ui, text: &str) -> f32 {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    ui.fonts(|f| f.layout_no_wrap(text.to_string(), font_id, Color32::WHITE).size().x)
}

/// Data Table column width for a given header label: the wider of the
/// header text and (for Time) `TIME_SAMPLE_TEXT`, plus padding for the cell
/// margin, with a floor so short headers don't get too cramped.
fn header_column_width(ui: &egui::Ui, header: &str) -> f32 {
    const PADDING: f32 = 20.0;
    const MIN_WIDTH: f32 = 60.0;
    let mut text_width = text_render_width(ui, header);
    if header.starts_with("Time ") {
        text_width = text_width.max(text_render_width(ui, TIME_SAMPLE_TEXT));
    }
    (text_width + PADDING).max(MIN_WIDTH)
}

/// A fit curve computed over a cropped window of a histogram. Also carries the fit's scalar
/// parameters (`FittingResult`'s `peak`/`mu`/`sigma`/`fwhm`/`resolution`)
struct ObsFitCurve {
    start: usize,
    curve: Vec<f64>,
    color: Color32,
    label: String,
    peak: f64,
    mu: f64,
    sigma: f64,
    fwhm: f64,
    resolution: f64,
}

const EMPTY_HISTOGRAM_STATS: &str = "Peak: -   Counts: 0   Mean: -   RMS: -   FWHM: -   Res: -";

/// Descriptive stats line for one histogram (Peak/Counts/Mean/RMS/FWHM/Res),
/// matching the original's `PlotHistogram`/`PlotStripHistogram`/
/// `PlotBGOHistogram`:
/// - When a fit is active, Peak/Mean/RMS/FWHM/Res come from the *fit*
///   (`FittingResult::peak`/`mu`/`sigma`/`fwhm`/`resolution`), not the raw
///   data - e.g. the original's `PlotHistogram` sets `peakLabel.Text =
///   fitResult.Peak`, not the histogram's raw max bin. `fits` is
///   `compute_fits`'s output for this histogram; when more than one fit
///   checkbox is enabled at once (this port's independent-checkboxes design,
///   unlike the original's single fit-method `ComboBox`), the first entry
///   wins - `compute_fits` always tries Gaussian, then Lorentzian, then HEMG,
///   in that order.
/// - When no fit is active/valid, Peak/Mean/RMS fall back to raw histogram
///   stats (mirroring `PlotStripHistogram`'s non-fit `else` branch: raw max
///   bin, arithmetic mean, population std of the raw samples) - but FWHM/Res
///   are left as "-", matching the original, which never computes those two
///   without a successful fit.
/// - Counts is independent of fitting (raw positive-sample count), but -
///   like the fit itself (see `compute_fits`) - restricted to `x_range`
///   when set, matching the original's `filteredData.Length` (counted
///   *after* the `v >= xMin && v <= xMax` filter, not before).
fn format_histogram_stats(math: &MathService, hist: &[i32], fits: Option<&[ObsFitCurve]>, x_range: Option<(f64, f64)>) -> String {
    let (range_start, range_end) = match x_range {
        Some((lo, hi)) => ((lo.max(0.0) as usize).min(hist.len()), ((hi.max(0.0) as usize) + 1).min(hist.len())),
        None => (0, hist.len()),
    };
    if range_end <= range_start {
        return EMPTY_HISTOGRAM_STATS.to_string();
    }
    let domain = &hist[range_start..range_end];

    let counts: i64 = domain.iter().map(|&c| c as i64).sum();
    if counts == 0 {
        return EMPTY_HISTOGRAM_STATS.to_string();
    }

    if let Some(primary) = fits.and_then(|f| f.first()) {
        return format!(
            "Peak: {:.2}   Counts: {counts}   Mean: {:.2}   RMS: {:.2}   FWHM: {:.2}   Res: {:.2}%",
            primary.peak, primary.mu, primary.sigma, primary.fwhm, primary.resolution
        );
    }

    let x_data: Vec<f64> = (range_start..range_end).map(|i| i as f64).collect();
    let y_data: Vec<f64> = domain.iter().map(|&c| c as f64).collect();
    let (mean, _sigma, peak) = math.calculate_moments(&x_data, &y_data);
    let rms = math.calculate_rms(&x_data, &y_data, mean);

    format!("Peak: {peak:.0}   Counts: {counts}   Mean: {mean:.2}   RMS: {rms:.2}   FWHM: -   Res: -")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservationViewMode {
    DssdPulseHeight,
    XStrip,
    YStrip,
    Bgo,
}

/// Which unit the Data Table's DSSD X/Y columns are shown in - a per-table
/// toggle, not tied to `ObservationViewMode`, since ADC vs Voltage is a
/// display choice orthogonal to which layer/view is selected. BGO has no
/// equivalent: it's always shown raw (see `data_table_ui`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DssdDataUnit {
    Adc,
    Voltage,
}

/// Which of the two top-level tabs the central panel is showing: the
/// grid-of-plots view, or a flat, event-by-event table (see `EventRow`) of
/// every decoded particle's DSSD/BGO series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservationTab {
    GraphView,
    DataTable,
}

enum WorkerMsg {
    Status(String, Color32),
    Busy(bool),
    InputFilesInfo(String),
    /// Sent once file selection has settled on the actual set of files to
    /// analyze - mirrors the original's `InputFileList = [combinedPath]`
    /// (`ObservationViewModel.FileOperations.cs`'s `LoadFiles`): a
    /// multi-file pick is combined into one file first, and *that* combined
    /// path becomes `input_files`, not the originals.
    InputFiles(Vec<PathBuf>),
    HistogramData(HashMap<String, Vec<i32>>),
    EventData(Vec<EventRow>),
    Error(String),
}

pub struct ObservationMode {
    input_files: Vec<PathBuf>,
    input_files_info: String,

    status_message: String,
    is_busy: bool,
    progress_value: f64,
    data_count_str: String,

    active_tab: ObservationTab,
    view_mode: ObservationViewMode,
    selected_layer: DetectorLayer,
    selected_bgo_layer: BgoLayer,
    histogram_data: HashMap<String, Vec<i32>>,
    /// Data Table tab's event-by-event rows, populated alongside
    /// `histogram_data` by `analyze_files_worker`.
    events: Vec<EventRow>,
    /// Data Table tab's ADC-vs-Voltage toggle for its DSSD X/Y columns (see
    /// `DssdDataUnit`).
    dssd_data_unit: DssdDataUnit,

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

        Self {
            input_files: Vec::new(),
            input_files_info: "No files selected".to_string(),
            status_message: "Ready".to_string(),
            is_busy: false,
            progress_value: 0.0,
            data_count_str: "-".to_string(),
            active_tab: ObservationTab::GraphView,
            view_mode: ObservationViewMode::DssdPulseHeight,
            selected_layer: DetectorLayer::L1,
            selected_bgo_layer: BgoLayer::L3,
            histogram_data: HashMap::new(),
            events: Vec::new(),
            dssd_data_unit: DssdDataUnit::Voltage,
            // Matches the original's `TxtDSSDXMin`/`TxtDSSDXMax` XAML
            // defaults ("0"/"4096") - see `dssd_x_range`'s doc comment for
            // why this default restriction matters, not just for the view.
            dssd_x_min: "0".to_string(),
            dssd_x_max: "4096".to_string(),
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
                    if ui.add(egui::TextEdit::singleline(&mut self.dssd_x_min).desired_width(60.0)).changed() {
                        self.fit_cache.clear();
                    }
                    ui.label("X Max:");
                    if ui.add(egui::TextEdit::singleline(&mut self.dssd_x_max).desired_width(60.0)).changed() {
                        self.fit_cache.clear();
                    }
                    if ui.button("Clear Range").clicked() {
                        self.dssd_x_min.clear();
                        self.dssd_x_max.clear();
                        self.fit_cache.clear();
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
            ui.colored_label(Color32::WHITE, &self.status_message);
            if self.is_busy {
                ui.add(egui::ProgressBar::new((self.progress_value / 100.0) as f32).show_percentage());
            }
            ui.label(format!("Data count: {}", self.data_count_str));
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active_tab, ObservationTab::GraphView, "Grid Graph Visualize");
                ui.selectable_value(&mut self.active_tab, ObservationTab::DataTable, "Data Table");
            });
            ui.separator();
            

            match self.active_tab {
                ObservationTab::GraphView => {
                    egui::ScrollArea::vertical().id_salt("obs_graph_scroll").show(ui, |ui| {
                        // Left/right breathing room so plots/columns don't
                        // run edge-to-edge against the panel border.
                        egui::Frame::none().inner_margin(egui::Margin::symmetric(16.0, 0.0)).show(ui, |ui| match self.view_mode {
                            ObservationViewMode::DssdPulseHeight => self.dssd_pulse_height_ui(ui, export),
                            ObservationViewMode::XStrip => self.strip_grid_ui(ui, 'X', export),
                            ObservationViewMode::YStrip => self.strip_grid_ui(ui, 'Y', export),
                            ObservationViewMode::Bgo => self.bgo_ui(ui, export),
                        });
                    });
                }
                ObservationTab::DataTable => self.data_table_ui(ui),
            }
        });

        if self.is_busy {
            ctx.request_repaint();
        }
    }

    /// Parses `dssd_x_min`/`dssd_x_max` into an active range
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
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(10.0);
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
                col.add_space(10.0);
                col.separator();
                col.add_space(10.0);
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

    /// Data Table tab: one row per decoded particle event. Columns are
    /// scoped to the current top `View:` selector - DSSD Pulse
    /// Height/X-Strip/Y-Strip show only the DSSD X/Y columns, BGO shows only
    /// the BGO High/Low columns (see `data_table_shows_dssd`) - and every
    /// column header names the byte offsets (within one particle's 34-byte
    /// payload) it was decoded from. When DSSD columns are shown, the ADC/
    /// Voltage toggle (`dssd_data_unit`) picks whether they display the raw
    /// decoded channel or `adc_to_volts` of it; BGO is always raw.
    ///
    /// The five line-header columns (Packet Sync Code/Package ID/Packet
    /// Seq/Packet Data Len before Time, Data Type after) are always shown,
    /// in their actual byte order, since they're line-level, not
    /// DSSD/BGO-specific. Packet Sync Code and Data Type display raw hex
    /// (e.g. `"E225"`); the other three are decimal.
    ///
    /// Each column's width is measured from its own header text
    /// (`header_column_width`) rather than one fixed width shared by every
    /// column - the header row and every body row are built from the same
    /// `headers`-derived `widths` so they line up.
    fn data_table_ui(&mut self, ui: &mut egui::Ui) {
        let show_dssd = data_table_shows_dssd(self.view_mode);

        ui.horizontal(|ui| {
            if ui.add_enabled(!self.events.is_empty(), egui::Button::new("Export Table as CSV...")).clicked() {
                self.export_table_csv();
            }
            ui.label(format!("{} event(s)", self.events.len()));
            if show_dssd {
                ui.separator();
                ui.label("DSSD values:");
                ui.radio_value(&mut self.dssd_data_unit, DssdDataUnit::Adc, "ADC");
                ui.radio_value(&mut self.dssd_data_unit, DssdDataUnit::Voltage, "Voltage");
            }
        });
        ui.separator();

        let mut headers = vec![
            "Packet Sync Code (Byte 0-1)".to_string(),
            "Package ID (Byte 2-3)".to_string(),
            "Packet Seq (Byte 4-5)".to_string(),
            "Packet Data Len (Byte 6-7)".to_string(),
            "Time (Byte 8-13)".to_string(),
            "Data Type (Byte 14-15)".to_string(),
        ];
        if show_dssd {
            for layer in DSSD_TABLE_LAYERS {
                let (x_range, y_range) = dssd_byte_range(layer);
                headers.push(format!("{layer:?}X (Byte {x_range})"));
                headers.push(format!("{layer:?}Y (Byte {y_range})"));
            }
        } else {
            for layer in BGO_TABLE_LAYERS {
                let (h_range, l_range) = bgo_byte_range(layer);
                headers.push(format!("{layer:?}H (Byte {h_range})"));
                headers.push(format!("{layer:?}L (Byte {l_range})"));
            }
        }
        let widths: Vec<f32> = headers.iter().map(|h| header_column_width(ui, h)).collect();

        egui::ScrollArea::horizontal().id_salt("obs_data_table_hscroll").show(ui, |ui| {
            let row_height = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;

            egui::Grid::new("obs_data_table_header").show(ui, |ui| {
                for (header, &width) in headers.iter().zip(&widths) {
                    ui.add_sized([width, row_height], egui::Label::new(egui::RichText::new(header).strong()));
                }
                ui.end_row();
            });

            egui::ScrollArea::vertical().id_salt("obs_data_table_vscroll").show_rows(ui, row_height, self.events.len(), |ui, visible_range| {
                egui::Grid::new("obs_data_table_body").striped(true).start_row(visible_range.start).show(ui, |ui| {
                    for event in &self.events[visible_range] {
                        let mut cells = vec![
                            event.packet_sync.clone(),
                            event.package_id.to_string(),
                            event.packet_sequence.to_string(),
                            event.packet_data_length.to_string(),
                            format_event_time(event.time),
                            event.data_type.clone(),
                        ];
                        if show_dssd {
                            for (x, y) in event.dssd {
                                cells.push(self.format_dssd_value(x));
                                cells.push(self.format_dssd_value(y));
                            }
                        } else {
                            for (h, l) in event.bgo {
                                cells.push(h.to_string());
                                cells.push(l.to_string());
                            }
                        }
                        for (cell, &width) in cells.iter().zip(&widths) {
                            ui.add_sized([width, row_height], egui::Label::new(cell));
                        }
                        ui.end_row();
                    }
                });
            });
        });
    }

    /// Renders one DSSD X/Y cell per `dssd_data_unit`: the raw ADC channel,
    /// or its `adc_to_volts` conversion to 4 decimal places.
    fn format_dssd_value(&self, adc: i32) -> String {
        match self.dssd_data_unit {
            DssdDataUnit::Adc => adc.to_string(),
            DssdDataUnit::Voltage => format!("{:.4}", adc_to_volts(adc)),
        }
    }

    /// Export the data table as csv. Column set and header text mirror
    /// `data_table_ui` exactly - same DSSD-vs-BGO filtering (`show_dssd`),
    /// same "(Byte x-y)" labels, same ADC-vs-Voltage formatting
    /// (`format_dssd_value`) - so the exported file always matches what's
    /// currently on screen.
    fn export_table_csv(&mut self) {
        if self.events.is_empty() {
            self.status_message = "No events to export - run Analyze Files first.".to_string();
            return;
        }
        let Some(path) = rfd::FileDialog::new().set_file_name("observation_events.csv").add_filter("CSV", &["csv"]).save_file() else {
            return;
        };

        let show_dssd = data_table_shows_dssd(self.view_mode);

        let mut csv = String::from(
            "Packet Sync Code (Byte 0-1),Package ID (Byte 2-3),Packet Seq (Byte 4-5),Packet Data Len (Byte 6-7),Time (Byte 8-13),Data Type (Byte 14-15)",
        );
        if show_dssd {
            for layer in DSSD_TABLE_LAYERS {
                let (x_range, y_range) = dssd_byte_range(layer);
                csv.push_str(&format!(",{layer:?}X (Byte {x_range}),{layer:?}Y (Byte {y_range})"));
            }
        } else {
            for layer in BGO_TABLE_LAYERS {
                let (h_range, l_range) = bgo_byte_range(layer);
                csv.push_str(&format!(",{layer:?}H (Byte {h_range}),{layer:?}L (Byte {l_range})"));
            }
        }
        csv.push('\n');

        for event in &self.events {
            csv.push_str(&format!(
                "{},{},{},{},{},{}",
                event.packet_sync,
                event.package_id,
                event.packet_sequence,
                event.packet_data_length,
                format_event_time(event.time),
                event.data_type
            ));
            if show_dssd {
                for (x, y) in event.dssd {
                    csv.push_str(&format!(",{},{}", self.format_dssd_value(x), self.format_dssd_value(y)));
                }
            } else {
                for (h, l) in event.bgo {
                    csv.push_str(&format!(",{h},{l}"));
                }
            }
            csv.push('\n');
        }

        match std::fs::write(&path, csv) {
            Ok(()) => self.status_message = format!("Exported {} event(s) to {}", self.events.len(), path.display()),
            Err(e) => self.status_message = format!("Failed to export CSV: {e}"),
        }
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
    ///
    /// A stats line (Peak/Counts/Mean/RMS/FWHM/Res, see
    /// `format_histogram_stats`) is drawn below every plot this way. Both
    /// the fit (see `compute_fits`) and this stats line are restricted to
    /// the same `x_range` used for the view - not just what's drawn - per
    /// `dssd_x_range`'s doc comment.
    fn histogram_plot_sized(&mut self, ui: &mut egui::Ui, key: &str, id: &str, height: f32, x_range: Option<(f64, f64)>, export: &mut crate::plot_export::PlotExportQueue) {
        self.ensure_fits(key, x_range);
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
                    .map(|(x, &c)| Bar::new(x as f64, c as f64).width(1.0).fill(Color32::BLACK))
                    .collect();
                plot_ui.bar_chart(BarChart::new(bars).name("Data").color(Color32::BLACK));
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

        ui.add_space(10.0);
        ui.colored_label(
            Color32::from_rgb(230, 230, 230),
            match hist {
                Some(h) => format_histogram_stats(&self.math, h, fits.map(|f| f.as_slice()), x_range),
                None => EMPTY_HISTOGRAM_STATS.to_string(),
            },
        );
        ui.add_space(10.0);
    }

    fn strip_bar_plot(&mut self, ui: &mut egui::Ui, key: &str, id: &str, x_range: Option<(f64, f64)>, export: &mut crate::plot_export::PlotExportQueue) {
        self.histogram_plot_sized(ui, key, id, 180.0, x_range, export);
    }

    fn ensure_fits(&mut self, key: &str, x_range: Option<(f64, f64)>) {
        if self.fit_cache.contains_key(key) {
            return;
        }
        let fits = self.compute_fits(key, x_range);
        self.fit_cache.insert(key.to_string(), fits);
    }
    const FIT_WINDOW: usize = 100;

    /// `x_range`, when set, restricts *which indices* of the histogram are
    /// even considered for peak detection and fitting - not just the
    /// window drawn - mirroring the original's raw-sample range filter (see
    /// `dssd_x_range`'s doc comment for why this matters, particularly for
    /// sparse per-strip histograms).
    fn compute_fits(&self, key: &str, x_range: Option<(f64, f64)>) -> Vec<ObsFitCurve> {
        if !(self.show_gaussian_fit || self.show_lorentzian_fit || self.show_hemg_fit) {
            return Vec::new();
        }
        let Some(hist) = self.histogram_data.get(key) else {
            return Vec::new();
        };

        let (range_start, range_end) = match x_range {
            Some((lo, hi)) => ((lo.max(0.0) as usize).min(hist.len()), ((hi.max(0.0) as usize) + 1).min(hist.len())),
            None => (0, hist.len()),
        };
        if range_end <= range_start {
            return Vec::new();
        }
        let domain = &hist[range_start..range_end];

        let Some((rel_peak_idx, _)) = domain.iter().enumerate().max_by_key(|&(_, &c)| c).filter(|&(_, &c)| c > 0) else {
            return Vec::new();
        };
        let peak_idx = range_start + rel_peak_idx;

        let start = peak_idx.saturating_sub(Self::FIT_WINDOW).max(range_start);
        let end = (peak_idx + Self::FIT_WINDOW + 1).min(range_end);
        if end - start < 5 {
            return Vec::new();
        }

        let x_data: Vec<f64> = (start..end).map(|i| i as f64).collect();
        let y_data: Vec<f64> = hist[start..end].iter().map(|&c| c as f64).collect();

        let mut fits = Vec::new();
        if self.show_gaussian_fit {
            let res = self.math.gaussian_fit(&x_data, &y_data);
            if res.is_valid && !res.fit_curve.is_empty() {
                fits.push(ObsFitCurve {
                    start,
                    curve: res.fit_curve,
                    color: Color32::from_rgb(50, 220, 50),
                    label: "Gaussian".to_string(),
                    peak: res.peak,
                    mu: res.mu,
                    sigma: res.sigma,
                    fwhm: res.fwhm,
                    resolution: res.resolution,
                });
            }
        }
        if self.show_lorentzian_fit {
            let res = self.math.lorentzian_fit(&x_data, &y_data);
            if res.is_valid && !res.fit_curve.is_empty() {
                fits.push(ObsFitCurve {
                    start,
                    curve: res.fit_curve,
                    color: Color32::from_rgb(0, 220, 220),
                    label: "Lorentzian".to_string(),
                    peak: res.peak,
                    mu: res.mu,
                    sigma: res.sigma,
                    fwhm: res.fwhm,
                    resolution: res.resolution,
                });
            }
        }
        if self.show_hemg_fit {
            let res = self.math.hemg_double_sided_fit(&x_data, &y_data, None, None);
            if res.is_valid && !res.fit_curve.is_empty() {
                fits.push(ObsFitCurve {
                    start,
                    curve: res.fit_curve,
                    color: Color32::RED,
                    label: "HEMG".to_string(),
                    peak: res.peak,
                    mu: res.mu,
                    sigma: res.sigma,
                    fwhm: res.fwhm,
                    resolution: res.resolution,
                });
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
                WorkerMsg::InputFiles(files) => self.input_files = files,
                WorkerMsg::HistogramData(data) => {
                    self.histogram_data = data;
                    self.fit_cache.clear();
                }
                WorkerMsg::EventData(events) => {
                    self.data_count_str = events.len().to_string();
                    self.events = events;
                }
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
        self.histogram_data.clear();
        self.events.clear();
        self.fit_cache.clear();
        self.data_count_str = "-".to_string();
        self.status_message = "Ready".to_string();
        self.progress_value = 0.0;
    }

    fn select_files(&mut self) {
        let Some(files) = rfd::FileDialog::new().add_filter("Text Files", &["txt"]).pick_files() else {
            return;
        };

        if files.len() == 1 {
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
    let file_count = files.len();
    match io::file_helper::combine_files(&files, "CombinedData.txt") {
        Ok(path) => {
            let _ = tx.send(WorkerMsg::InputFilesInfo(format!("{file_count} file(s) combined.")));
            let _ = tx.send(WorkerMsg::Status(format!("Files combined successfully into {}", path.display()), Color32::from_rgb(50, 200, 50)));
            let _ = tx.send(WorkerMsg::InputFiles(vec![path]));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Error combining files: {e}")));
        }
    }
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn analyze_files_worker(files: Vec<PathBuf>, tx: Sender<WorkerMsg>) {
    let mut processor = ObservationDataProcessor::new();
    match processor.process_files(&files) {
        Ok(histogram_data) => {
            let events: Vec<EventRow> = processor
                .results
                .into_iter()
                .map(|r| EventRow {
                    packet_sync: r.packet_sync,
                    package_id: r.package_id,
                    packet_sequence: r.packet_sequence,
                    packet_data_length: r.packet_data_length,
                    time: r.time,
                    data_type: r.data_type,
                    dssd: DSSD_TABLE_LAYERS.map(|layer| r.dssd_pulses.get(&layer).copied().unwrap_or((0, 0))),
                    bgo: BGO_TABLE_LAYERS.map(|layer| r.bgo_pulses.get(&layer).copied().unwrap_or((0, 0))),
                })
                .collect();
            let _ = tx.send(WorkerMsg::HistogramData(histogram_data));
            let _ = tx.send(WorkerMsg::EventData(events));
            let _ = tx.send(WorkerMsg::Status("Processing complete!".to_string(), Color32::from_rgb(50, 200, 50)));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Error! {e}")));
        }
    }
    let _ = tx.send(WorkerMsg::Busy(false));
}


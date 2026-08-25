//! Port of Observation mode

use baseline_core::math::MathService;
use baseline_core::models::observation::{BgoLayer, DetectorLayer};
use baseline_core::observation::{ObservationDataProcessor, LINE_TAIL_FIELDS};
use chrono::{DateTime, Utc};
use egui::Color32;
use egui_plot::{Bar, BarChart, Legend, Line, Plot, PlotPoints};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

/// DSSD/BGO layers in the Data Table tab's fixed column order
const DSSD_TABLE_LAYERS: [DetectorLayer; 4] = [
    DetectorLayer::L1,
    DetectorLayer::L2,
    DetectorLayer::L6,
    DetectorLayer::L7,
];
const BGO_TABLE_LAYERS: [BgoLayer; 3] = [BgoLayer::L3, BgoLayer::L4, BgoLayer::L5];

/// 0-indexed byte offsets within one particle's 34-byte payload for each
/// DSSD layer's XY Position / X Pulse Height / Y Pulse Height fields.
fn dssd_byte_ranges(layer: DetectorLayer) -> (&'static str, &'static str, &'static str) {
    match layer {
        DetectorLayer::L1 => ("2", "3-4", "5-6"),
        DetectorLayer::L2 => ("7", "8-9", "10-11"),
        DetectorLayer::L6 => ("24", "25-26", "27-28"),
        DetectorLayer::L7 => ("29", "30-31", "32-33"),
    }
}

/// 0-indexed byte offsets within one particle's 34-byte payload for each
/// BGO layer's High-Gain / Low-Gain Pulse Height fields.
fn bgo_byte_ranges(layer: BgoLayer) -> (&'static str, &'static str) {
    match layer {
        BgoLayer::L3 => ("12-13", "14-15"),
        BgoLayer::L4 => ("16-17", "18-19"),
        BgoLayer::L5 => ("20-21", "22-23"),
    }
}

fn particle_byte_label(range: &str) -> String {
    format!("(Particle Byte {range})")
}

fn data_table_shows_dssd(view_mode: ObservationViewMode) -> bool {
    view_mode != ObservationViewMode::Bgo
}

/// "{label} (Byte {n})" for a 1-byte field, "{label} (Byte {n}-{m})" for a
/// 2-byte one - matches the "(Byte X-Y)" style of the other Data Table headers.
fn line_extra_header(field: &baseline_core::observation::LineTailField) -> String {
    let byte_range = if field.byte_len == 1 {
        field.byte_start.to_string()
    } else {
        format!("{}-{}", field.byte_start, field.byte_start + field.byte_len - 1)
    };
    format!("{} (Byte {byte_range})", field.label)
}

#[derive(Debug, Clone)]
struct EventRow {
    packet_sync: String,
    package_id: i32,
    packet_sequence: i32,
    packet_data_length: i32,
    time: DateTime<Utc>,
    data_type: String,
    particle_time_raw: i32,
    dssd_position: [i32; 4],
    dssd: [(i32, i32); 4],
    bgo: [(i32, i32); 3],
    line_extra: Arc<[String]>,
}

/// 14-bit ADC channel -> volts
fn adc_to_volts(channel: i32) -> f64 {
    channel as f64 / 16384.0 * 5.0
}

/// "YYYY-MM-DD HH:MM:SS.mmm", matching the timecode format
fn format_event_time(time: DateTime<Utc>) -> String {
    time.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

const TIME_SAMPLE_TEXT: &str = "0000-00-00 00:00:00.000";

/// Rendered pixel width of `text` in the current `Body` text style
fn text_render_width(ui: &egui::Ui, text: &str) -> f32 {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    ui.fonts(|f| {
        f.layout_no_wrap(text.to_string(), font_id, Color32::WHITE)
            .size()
            .x
    })
}

/// Func for fitting the width of the Data table's column headers to their content
fn header_column_width(ui: &egui::Ui, header: &str) -> f32 {
    const PADDING: f32 = 20.0;
    const MIN_WIDTH: f32 = 60.0;
    let mut text_width = text_render_width(ui, header);
    if header.starts_with("Time ") {
        text_width = text_width.max(text_render_width(ui, TIME_SAMPLE_TEXT));
    }
    (text_width + PADDING).max(MIN_WIDTH)
}

/// A fit curve computed over the selected Observation ROI. Also carries the
/// fit's scalar parameters used by the stats line.
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

/// Descriptive stats line for one histogram
fn format_histogram_stats(
    math: &MathService,
    hist: &[i32],
    fits: Option<&[ObsFitCurve]>,
    x_range: Option<(f64, f64)>,
) -> String {
    let (range_start, range_end) = histogram_bounds(hist.len(), x_range);
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

    format!(
        "Peak: {peak:.0}   Counts: {counts}   Mean: {mean:.2}   RMS: {rms:.2}   FWHM: -   Res: -"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservationViewMode {
    DssdPulseHeight,
    XStrip,
    YStrip,
    Bgo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DssdDataUnit {
    Adc,
    Voltage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservationTab {
    GraphView,
    DataTable,
}

enum WorkerMsg {
    Status {
        run_id: u64,
        text: String,
    },
    Progress {
        run_id: u64,
        processed: u64,
        total: u64,
    },
    Complete {
        run_id: u64,
        histogram_data: HashMap<String, Vec<i32>>,
        raw_histogram_data: HashMap<String, Vec<i32>>,
        events: Vec<EventRow>,
    },
    Error {
        run_id: u64,
        text: String,
    },
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
    raw_histogram_data: HashMap<String, Vec<i32>>,
    /// Data Table tab's event-by-event rows, populated alongside
    /// `histogram_data` by `analyze_files_worker`.
    events: Vec<EventRow>,
    dssd_data_unit: DssdDataUnit,

    /// Fit/View ROI text fields for the DSSD Pulse Height/X-Strip/Y-Strip
    /// views. Raw text lets the field be edited freely (including
    /// transiently empty/invalid states); a valid range drives the view,
    /// stats, peak selection, and fit. Blank or invalid input shows all bins.
    dssd_x_min: String,
    dssd_x_max: String,
    bgo_x_min: String,
    bgo_x_max: String,
    dssd_adaptive_threshold: bool,
    run_id: u64,

    show_gaussian_fit: bool,
    show_lorentzian_fit: bool,
    show_hemg_fit: bool,

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
            raw_histogram_data: HashMap::new(),
            events: Vec::new(),
            dssd_data_unit: DssdDataUnit::Voltage,
            // Full 14-bit DSSD range, matching the original WinForms
            // XaxisMinDSSD/XaxisMaxDSSD defaults.
            dssd_x_min: "0".to_string(),
            dssd_x_max: "16384".to_string(),
            bgo_x_min: "0".to_string(),
            bgo_x_max: "4095".to_string(),
            dssd_adaptive_threshold: false,
            run_id: 0,
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
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        export: &mut crate::plot_export::PlotExportQueue,
    ) {
        self.drain_messages();

        egui::TopBottomPanel::top("observation_top").show(ctx, |ui| {
            ui.heading("Observation Mode");
            ui.separator();
            ui.label(&self.input_files_info);
            if ui
                .add_enabled(!self.is_busy, egui::Button::new("Select Files"))
                .clicked()
            {
                self.select_files();
            }

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.is_busy && !self.input_files.is_empty(),
                        egui::Button::new("Process Data"),
                    )
                    .clicked()
                {
                    self.analyze_files();
                }
                if ui.button("Reset").clicked() {
                    self.reset();
                }
            });

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label("View:");
                ui.selectable_value(
                    &mut self.view_mode,
                    ObservationViewMode::DssdPulseHeight,
                    "DSSD Pulse Height",
                );
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
                                ui.selectable_value(
                                    &mut self.selected_bgo_layer,
                                    layer,
                                    format!("{layer:?}"),
                                );
                            }
                        });
                    ui.label("Fit/View X Min:");
                    if ui
                        .add(egui::TextEdit::singleline(&mut self.bgo_x_min).desired_width(60.0))
                        .changed()
                    {
                        self.fit_cache.clear();
                    }
                    ui.label("Fit/View X Max:");
                    if ui
                        .add(egui::TextEdit::singleline(&mut self.bgo_x_max).desired_width(60.0))
                        .changed()
                    {
                        self.fit_cache.clear();
                    }
                    if ui.button("Clear Range").clicked() {
                        self.bgo_x_min.clear();
                        self.bgo_x_max.clear();
                        self.fit_cache.clear();
                    }
                } else {
                    egui::ComboBox::from_label("DSSD Layer")
                        .selected_text(format!("{:?}", self.selected_layer))
                        .show_ui(ui, |ui| {
                            for layer in [
                                DetectorLayer::L1,
                                DetectorLayer::L2,
                                DetectorLayer::L6,
                                DetectorLayer::L7,
                            ] {
                                ui.selectable_value(
                                    &mut self.selected_layer,
                                    layer,
                                    format!("{layer:?}"),
                                );
                            }
                        });

                    ui.label("Fit/View X Min:");
                    if ui
                        .add(egui::TextEdit::singleline(&mut self.dssd_x_min).desired_width(60.0))
                        .changed()
                    {
                        self.fit_cache.clear();
                    }
                    ui.label("Fit/View X Max:");
                    if ui
                        .add(egui::TextEdit::singleline(&mut self.dssd_x_max).desired_width(60.0))
                        .changed()
                    {
                        self.fit_cache.clear();
                    }
                    if ui.button("Clear Range").clicked() {
                        self.dssd_x_min.clear();
                        self.dssd_x_max.clear();
                        self.fit_cache.clear();
                    }
                    if self.view_mode == ObservationViewMode::DssdPulseHeight
                        && ui
                            .checkbox(
                                &mut self.dssd_adaptive_threshold,
                                "Adaptive threshold (k=1.0)",
                            )
                            .changed()
                    {
                        self.fit_cache.clear();
                    }
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label("Fits:");
                if ui
                    .checkbox(&mut self.show_gaussian_fit, "Gaussian")
                    .changed()
                {
                    self.fit_cache.clear();
                }
                if ui
                    .checkbox(&mut self.show_lorentzian_fit, "Lorentzian")
                    .changed()
                {
                    self.fit_cache.clear();
                }
                if ui.checkbox(&mut self.show_hemg_fit, "HEMG").changed() {
                    self.fit_cache.clear();
                }
            });

            ui.separator();
            ui.colored_label(Color32::WHITE, &self.status_message);
            if self.is_busy {
                ui.add(
                    egui::ProgressBar::new((self.progress_value / 100.0) as f32).show_percentage(),
                );
            }
            ui.label(format!("Data count: {}", self.data_count_str));
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.active_tab,
                    ObservationTab::GraphView,
                    "Graph Visualization",
                );
                ui.selectable_value(
                    &mut self.active_tab,
                    ObservationTab::DataTable,
                    "Data Table",
                );
            });
            ui.separator();

            match self.active_tab {
                ObservationTab::GraphView => {
                    egui::ScrollArea::vertical()
                        .id_salt("obs_graph_scroll")
                        .show(ui, |ui| {
                            // Left/right breathing room so plots/columns don't
                            // run edge-to-edge against the panel border.
                            egui::Frame::none()
                                .inner_margin(egui::Margin::symmetric(16.0, 0.0))
                                .show(ui, |ui| match self.view_mode {
                                    ObservationViewMode::DssdPulseHeight => {
                                        self.dssd_pulse_height_ui(ui, export)
                                    }
                                    ObservationViewMode::XStrip => {
                                        self.strip_grid_ui(ui, 'X', export)
                                    }
                                    ObservationViewMode::YStrip => {
                                        self.strip_grid_ui(ui, 'Y', export)
                                    }
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

    /// Parses the DSSD Fit/View ROI fields into an active range.
    fn dssd_x_range(&self) -> Option<(f64, f64)> {
        let min = self.dssd_x_min.trim().parse::<usize>().ok()?;
        let max = self.dssd_x_max.trim().parse::<usize>().ok()?;
        (max > min).then_some((min as f64, max as f64))
    }

    fn bgo_x_range(&self) -> Option<(f64, f64)> {
        let min = self.bgo_x_min.trim().parse::<usize>().ok()?;
        let max = self.bgo_x_max.trim().parse::<usize>().ok()?;
        (max > min).then_some((min as f64, max as f64))
    }

    fn dssd_pulse_height_ui(
        &mut self,
        ui: &mut egui::Ui,
        export: &mut crate::plot_export::PlotExportQueue,
    ) {
        let layer_name = format!("{:?}", self.selected_layer);
        let x_key = format!("DSSD{layer_name}_X");
        let y_key = format!("DSSD{layer_name}_Y");
        let x_range = self.dssd_x_range();

        let header = ui.label(format!("{layer_name} - Pulse Height X"));
        self.histogram_plot(ui, &x_key, "obs_x_plot", Some(header.rect), x_range, export);
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(10.0);
        let header = ui.label(format!("{layer_name} - Pulse Height Y"));
        self.histogram_plot(ui, &y_key, "obs_y_plot", Some(header.rect), x_range, export);
    }

    /// `axis` is `'X'` or `'Y'`, selecting the X-Strip or Y-Strip view.
    fn strip_grid_ui(
        &mut self,
        ui: &mut egui::Ui,
        axis: char,
        export: &mut crate::plot_export::PlotExportQueue,
    ) {
        let layer_name = format!("{:?}", self.selected_layer);
        ui.label(format!("{layer_name} - {axis}-Strip Pulse Height (1-8)"));
        ui.separator();
        let x_range = self.dssd_x_range();

        ui.columns(2, |cols| {
            for strip in 1..=8 {
                let key = format!("DSSD{layer_name}_Strip{axis}{strip}");
                let col = &mut cols[(strip - 1) % 2];
                let header = col.label(format!("Strip {axis}{strip}"));
                self.strip_bar_plot(
                    col,
                    &key,
                    &format!("obs_strip_{axis}{strip}"),
                    Some(header.rect),
                    x_range,
                    export,
                );
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
        let x_range = self.bgo_x_range();

        ui.columns(2, |cols| {
            let header = cols[0].label(format!("{layer_name} - BGO High Gain"));
            self.strip_bar_plot(
                &mut cols[0],
                &high_key,
                "obs_bgo_high",
                Some(header.rect),
                x_range,
                export,
            );
            let header = cols[1].label(format!("{layer_name} - BGO Low Gain"));
            self.strip_bar_plot(
                &mut cols[1],
                &low_key,
                "obs_bgo_low",
                Some(header.rect),
                x_range,
                export,
            );
        });
    }

    /// Data Table tab: one row per decoded particle event, with the DSSD/BGO columns
    fn data_table_ui(&mut self, ui: &mut egui::Ui) {
        let show_dssd = data_table_shows_dssd(self.view_mode);

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.events.is_empty(),
                    egui::Button::new("Export Table as CSV..."),
                )
                .clicked()
            {
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
            format!("Particle Time {}", particle_byte_label("0-1")),
        ];
        if show_dssd {
            for layer in DSSD_TABLE_LAYERS {
                let (pos_range, x_range, y_range) = dssd_byte_ranges(layer);
                headers.push(format!("{layer:?} XY Position {}", particle_byte_label(pos_range)));
                headers.push(format!("{layer:?} X Pulse Height {}", particle_byte_label(x_range)));
                headers.push(format!("{layer:?} Y Pulse Height {}", particle_byte_label(y_range)));
            }
        } else {
            for layer in BGO_TABLE_LAYERS {
                let (h_range, l_range) = bgo_byte_ranges(layer);
                headers.push(format!("{layer:?} High-Gain Pulse Height {}", particle_byte_label(h_range)));
                headers.push(format!("{layer:?} Low-Gain Pulse Height {}", particle_byte_label(l_range)));
            }
        }
        for field in LINE_TAIL_FIELDS {
            headers.push(line_extra_header(field));
        }
        let widths: Vec<f32> = headers.iter().map(|h| header_column_width(ui, h)).collect();

        egui::ScrollArea::horizontal()
            .id_salt("obs_data_table_hscroll")
            .show(ui, |ui| {
                let row_height =
                    ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;

                egui::Grid::new("obs_data_table_header").show(ui, |ui| {
                    for (header, &width) in headers.iter().zip(&widths) {
                        ui.add_sized(
                            [width, row_height],
                            egui::Label::new(egui::RichText::new(header).strong()),
                        );
                    }
                    ui.end_row();
                });

                egui::ScrollArea::vertical()
                    .id_salt("obs_data_table_vscroll")
                    .show_rows(ui, row_height, self.events.len(), |ui, visible_range| {
                        egui::Grid::new("obs_data_table_body")
                            .striped(true)
                            .start_row(visible_range.start)
                            .show(ui, |ui| {
                                for event in &self.events[visible_range] {
                                    let mut cells = vec![
                                        event.packet_sync.clone(),
                                        event.package_id.to_string(),
                                        event.packet_sequence.to_string(),
                                        event.packet_data_length.to_string(),
                                        format_event_time(event.time),
                                        event.data_type.clone(),
                                        event.particle_time_raw.to_string(),
                                    ];
                                    if show_dssd {
                                        for (pos, (x, y)) in event
                                            .dssd_position
                                            .into_iter()
                                            .zip(event.dssd)
                                        {
                                            cells.push(pos.to_string());
                                            cells.push(self.format_dssd_value(x));
                                            cells.push(self.format_dssd_value(y));
                                        }
                                    } else {
                                        for (h, l) in event.bgo {
                                            cells.push(h.to_string());
                                            cells.push(l.to_string());
                                        }
                                    }
                                    for value in event.line_extra.iter() {
                                        cells.push(value.clone());
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
    fn format_dssd_value(&self, adc: i32) -> String {
        match self.dssd_data_unit {
            DssdDataUnit::Adc => adc.to_string(),
            DssdDataUnit::Voltage => format!("{:.4}", adc_to_volts(adc)),
        }
    }

    /// Export the data table as csv
    fn export_table_csv(&mut self) {
        if self.events.is_empty() {
            self.status_message = "No events to export - run Analyze Files first.".to_string();
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("observation_events.csv")
            .add_filter("CSV", &["csv"])
            .save_file()
        else {
            return;
        };

        let show_dssd = data_table_shows_dssd(self.view_mode);

        let mut csv = String::from(
            "Packet Sync Code (Byte 0-1),Package ID (Byte 2-3),Packet Seq (Byte 4-5),Packet Data Len (Byte 6-7),Time (Byte 8-13),Data Type (Byte 14-15)",
        );
        csv.push_str(&format!(",Particle Time {}", particle_byte_label("0-1")));
        if show_dssd {
            for layer in DSSD_TABLE_LAYERS {
                let (pos_range, x_range, y_range) = dssd_byte_ranges(layer);
                csv.push_str(&format!(
                    ",{layer:?} XY Position {},{layer:?} X Pulse Height {},{layer:?} Y Pulse Height {}",
                    particle_byte_label(pos_range),
                    particle_byte_label(x_range),
                    particle_byte_label(y_range)
                ));
            }
        } else {
            for layer in BGO_TABLE_LAYERS {
                let (h_range, l_range) = bgo_byte_ranges(layer);
                csv.push_str(&format!(
                    ",{layer:?} High-Gain Pulse Height {},{layer:?} Low-Gain Pulse Height {}",
                    particle_byte_label(h_range),
                    particle_byte_label(l_range)
                ));
            }
        }
        for field in LINE_TAIL_FIELDS {
            csv.push_str(&format!(",{}", line_extra_header(field)));
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
            csv.push_str(&format!(",{}", event.particle_time_raw));
            if show_dssd {
                for (pos, (x, y)) in event.dssd_position.into_iter().zip(event.dssd) {
                    csv.push_str(&format!(
                        ",{},{},{}",
                        pos,
                        self.format_dssd_value(x),
                        self.format_dssd_value(y)
                    ));
                }
            } else {
                for (h, l) in event.bgo {
                    csv.push_str(&format!(",{h},{l}"));
                }
            }
            for value in event.line_extra.iter() {
                csv.push_str(&format!(",{value}"));
            }
            csv.push('\n');
        }

        match std::fs::write(&path, csv) {
            Ok(()) => {
                self.status_message = format!(
                    "Exported {} event(s) to {}",
                    self.events.len(),
                    path.display()
                )
            }
            Err(e) => self.status_message = format!("Failed to export CSV: {e}"),
        }
    }

    fn histogram_plot(
        &mut self,
        ui: &mut egui::Ui,
        key: &str,
        id: &str,
        header_rect: Option<egui::Rect>,
        x_range: Option<(f64, f64)>,
        export: &mut crate::plot_export::PlotExportQueue,
    ) {
        self.histogram_plot_sized(ui, key, id, 260.0, header_rect, x_range, export);
    }

    /// Bar-chart rendering (Counting the number of events in each ADC channel)
    fn histogram_plot_sized(
        &mut self,
        ui: &mut egui::Ui,
        key: &str,
        id: &str,
        height: f32,
        header_rect: Option<egui::Rect>,
        x_range: Option<(f64, f64)>,
        export: &mut crate::plot_export::PlotExportQueue,
    ) {
        self.ensure_fits(key, x_range);
        let hist = self.histogram_data.get(key);
        let fits = self.fit_cache.get(key);
        let in_range = |x: f64| x_range.is_none_or(|(lo, hi)| x >= lo && x <= hi);
        let adaptive_threshold = if self.dssd_adaptive_threshold
            && key.starts_with("DSSD")
            && !key.contains("Strip")
        {
            self.raw_histogram_data
                .get(key)
                .and_then(|raw| adaptive_channel_threshold(raw))
        } else {
            None
        };
        let show_channel = |x: f64| adaptive_threshold.is_none_or(|threshold| x.abs() > threshold);

        let plot = Plot::new(id)
            .height(height)
            .legend(Legend::default())
            .custom_x_axes(vec![crate::plot_export::x_axis("Pulse Height (ADC Channel)")])
            .custom_y_axes(vec![crate::plot_export::y_axis("Counts")]);
        crate::plot_export::show(ui, export, id, id, height, header_rect, plot, |plot_ui| {
            if let Some(hist) = hist {
                let bars: Vec<Bar> = hist
                    .iter()
                    .enumerate()
                    .filter(|&(x, &c)| c > 0 && in_range(x as f64) && show_channel(x as f64))
                    .map(|(x, &c)| {
                        Bar::new(x as f64, c as f64)
                            .width(1.0)
                            .fill(Color32::LIGHT_BLUE)
                    })
                    .collect();
                plot_ui.bar_chart(BarChart::new(bars).name("Data").color(Color32::LIGHT_BLUE));
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
                Some(h) => {
                    format_histogram_stats(&self.math, h, fits.map(|f| f.as_slice()), x_range)
                }
                None => EMPTY_HISTOGRAM_STATS.to_string(),
            },
        );
        if (self.show_gaussian_fit || self.show_lorentzian_fit || self.show_hemg_fit)
            && fits.is_some_and(|fits| fits.is_empty())
            && hist.is_some_and(|hist| {
                hist.iter()
                    .enumerate()
                    .any(|(x, &count)| count > 0 && in_range(x as f64) && show_channel(x as f64))
            })
        {
            ui.colored_label(
                Color32::YELLOW,
                if x_range.is_some() {
                    "No valid fit in selected range; adjust Fit/View X Min or Fit/View X Max."
                } else {
                    "No valid fit in selected histogram."
                },
            );
        }
        ui.add_space(10.0);
    }

    fn strip_bar_plot(
        &mut self,
        ui: &mut egui::Ui,
        key: &str,
        id: &str,
        header_rect: Option<egui::Rect>,
        x_range: Option<(f64, f64)>,
        export: &mut crate::plot_export::PlotExportQueue,
    ) {
        self.histogram_plot_sized(ui, key, id, 180.0, header_rect, x_range, export);
    }

    fn ensure_fits(&mut self, key: &str, x_range: Option<(f64, f64)>) {
        if self.fit_cache.contains_key(key) {
            return;
        }
        let fits = self.compute_fits(key, x_range);
        self.fit_cache.insert(key.to_string(), fits);
    }

    /// `x_range`, when set, restricts *which indices* of the histogram are
    /// considered for peak detection and fitting, not just the window drawn.
    /// Observation fits receive every preprocessed point in this ROI.
    fn compute_fits(&self, key: &str, x_range: Option<(f64, f64)>) -> Vec<ObsFitCurve> {
        if !(self.show_gaussian_fit || self.show_lorentzian_fit || self.show_hemg_fit) {
            return Vec::new();
        }
        let Some(hist) = self.histogram_data.get(key) else {
            return Vec::new();
        };

        let (range_start, range_end) = histogram_bounds(hist.len(), x_range);
        if range_end <= range_start {
            return Vec::new();
        }
        let x_data: Vec<f64> = (range_start..range_end).map(|i| i as f64).collect();
        let mut y_data: Vec<f64> = hist[range_start..range_end]
            .iter()
            .map(|&c| c.max(0) as f64)
            .collect();
        preprocess_observation_histogram(
            key,
            range_start,
            &mut y_data,
            self.raw_histogram_data.get(key).map(Vec::as_slice),
            self.dssd_adaptive_threshold,
        );
        if y_data.iter().all(|&count| count <= 0.0) {
            return Vec::new();
        }

        let mut fits = Vec::new();
        if self.show_gaussian_fit {
            if let Some(fit) = fit_observation_gaussian(&x_data, &y_data) {
                fits.push(ObsFitCurve {
                    start: range_start,
                    color: Color32::from_rgb(50, 220, 50),
                    label: "Gaussian".to_string(),
                    ..fit
                });
            }
        }
        if self.show_lorentzian_fit {
            if let Some(fit) = fit_observation_lorentzian(&x_data, &y_data) {
                fits.push(ObsFitCurve {
                    start: range_start,
                    color: Color32::from_rgb(0, 220, 220),
                    label: "Lorentzian".to_string(),
                    ..fit
                });
            }
        }
        if self.show_hemg_fit {
            if let Some(fit) = fit_observation_single_left_hemg(&x_data, &y_data) {
                fits.push(ObsFitCurve {
                    start: range_start,
                    color: Color32::RED,
                    label: "HEMG".to_string(),
                    ..fit
                });
            }
        }
        fits
    }

    fn drain_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Status { run_id, text, .. } if run_id == self.run_id => {
                    self.status_message = text;
                }
                WorkerMsg::Progress {
                    run_id,
                    processed,
                    total,
                } if run_id == self.run_id => {
                    self.progress_value = if total == 0 {
                        100.0
                    } else {
                        (processed as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
                    };
                }
                WorkerMsg::Complete {
                    run_id,
                    histogram_data,
                    raw_histogram_data,
                    events,
                } if run_id == self.run_id => {
                    self.histogram_data = histogram_data;
                    self.raw_histogram_data = raw_histogram_data;
                    self.data_count_str = events.len().to_string();
                    self.events = events;
                    self.fit_cache.clear();
                    self.progress_value = 100.0;
                    self.status_message = "Processing complete!".to_string();
                    self.is_busy = false;
                }
                WorkerMsg::Error { run_id, text } if run_id == self.run_id => {
                    self.status_message = text;
                    self.is_busy = false;
                }
                _ => {}
            }
        }
    }

    fn reset(&mut self) {
        self.run_id = self.run_id.wrapping_add(1);
        self.input_files.clear();
        self.input_files_info = "No files selected".to_string();
        self.histogram_data.clear();
        self.raw_histogram_data.clear();
        self.events.clear();
        self.fit_cache.clear();
        self.data_count_str = "-".to_string();
        self.status_message = "Ready".to_string();
        self.progress_value = 0.0;
        self.is_busy = false;
    }

    fn select_files(&mut self) {
        let Some(files) = rfd::FileDialog::new()
            .add_filter("Text Files", &["txt"])
            .pick_files()
        else {
            return;
        };

        if files.is_empty() {
            return;
        }
        self.run_id = self.run_id.wrapping_add(1);
        self.histogram_data.clear();
        self.raw_histogram_data.clear();
        self.events.clear();
        self.fit_cache.clear();
        self.data_count_str = "-".to_string();
        self.progress_value = 0.0;
        let message = match files.len() {
            1 => "1 file selected.".to_string(),
            count => format!("{count} files selected."),
        };
        self.input_files = files;
        self.input_files_info = message.clone();
        self.status_message = message;
    }

    fn analyze_files(&mut self) {
        self.run_id = self.run_id.wrapping_add(1);
        self.is_busy = true;
        self.progress_value = 0.0;
        self.status_message = "Processing...".to_string();
        self.histogram_data.clear();
        self.raw_histogram_data.clear();
        self.events.clear();
        self.fit_cache.clear();
        self.data_count_str = "-".to_string();

        let files = self.input_files.clone();
        let tx = self.tx.clone();
        let run_id = self.run_id;
        std::thread::spawn(move || analyze_files_worker(files, tx, run_id));
    }
}

fn histogram_bounds(length: usize, x_range: Option<(f64, f64)>) -> (usize, usize) {
    match x_range {
        Some((lo, hi)) => (
            (lo.max(0.0) as usize).min(length),
            (hi.max(0.0) as usize).saturating_add(1).min(length),
        ),
        None => (0, length),
    }
}

fn preprocess_observation_histogram(
    key: &str,
    range_start: usize,
    y_data: &mut [f64],
    raw_histogram: Option<&[i32]>,
    adaptive_dssd: bool,
) {
    if key.starts_with("BGO") {
        let max_count = y_data.iter().copied().fold(0.0, f64::max);
        if max_count > 0.0 {
            let threshold = max_count * 0.10;
            if let Some(first) = y_data
                .iter()
                .position(|&count| count > 0.0 && count >= threshold)
            {
                y_data[..first].fill(0.0);
            }
        }
    }

    if adaptive_dssd && key.starts_with("DSSD") && !key.contains("Strip") {
        let Some(raw) = raw_histogram else {
            return;
        };
        if let Some(threshold) = adaptive_channel_threshold(raw) {
            for (index, count) in y_data.iter_mut().enumerate() {
                if ((range_start + index) as f64).abs() <= threshold {
                    *count = 0.0;
                }
            }
        }
    }
}

fn adaptive_channel_threshold(raw: &[i32]) -> Option<f64> {
    if raw.is_empty() {
        return None;
    }
    let mut total = 0.0;
    let mut weighted_sum = 0.0;
    for (channel, &count) in raw.iter().enumerate() {
        let weight = count.max(0) as f64;
        total += weight;
        weighted_sum += channel as f64 * weight;
    }
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    let mean = weighted_sum / total;
    let variance = raw
        .iter()
        .enumerate()
        .map(|(channel, &count)| {
            let channel = channel as f64;
            let diff = channel - mean;
            count.max(0) as f64 * diff * diff
        })
        .sum::<f64>()
        / total;
    let threshold = mean + variance.max(0.0).sqrt();
    if threshold.is_finite() {
        Some(threshold)
    } else {
        None
    }
}

fn weighted_moments(x_data: &[f64], y_data: &[f64]) -> Option<(f64, f64, f64)> {
    if x_data.len() != y_data.len() || x_data.is_empty() {
        return None;
    }
    let total = y_data.iter().copied().filter(|value| value.is_finite() && *value > 0.0).sum::<f64>();
    if !total.is_finite() || total <= 0.0 {
        return None;
    }
    let mean = x_data
        .iter()
        .zip(y_data)
        .map(|(&x, &y)| if y > 0.0 { x * y } else { 0.0 })
        .sum::<f64>()
        / total;
    let variance = x_data
        .iter()
        .zip(y_data)
        .map(|(&x, &y)| if y > 0.0 { (x - mean).powi(2) * y } else { 0.0 })
        .sum::<f64>()
        / total;
    let peak = y_data.iter().copied().fold(0.0, f64::max);
    if !mean.is_finite() || !variance.is_finite() || !peak.is_finite() || peak <= 0.0 {
        None
    } else {
        Some((mean, variance.sqrt(), peak))
    }
}

fn weighted_sample_std(x_data: &[f64], y_data: &[f64], mean: f64) -> f64 {
    let total = y_data.iter().copied().filter(|value| value.is_finite() && *value > 0.0).sum::<f64>();
    if total <= 1.0 {
        return 0.0;
    }
    let variance = x_data
        .iter()
        .zip(y_data)
        .map(|(&x, &y)| if y > 0.0 { (x - mean).powi(2) * y } else { 0.0 })
        .sum::<f64>()
        / (total - 1.0);
    variance.max(0.0).sqrt()
}

fn gaussian_value(x: f64, amplitude: f64, mean: f64, sigma: f64) -> f64 {
    let exponent = -(x - mean).powi(2) / (2.0 * sigma * sigma);
    if exponent < -700.0 {
        0.0
    } else {
        amplitude * exponent.exp()
    }
}

fn lorentzian_value(x: f64, amplitude: f64, mean: f64, gamma: f64) -> f64 {
    let gamma_squared = gamma * gamma;
    amplitude * gamma_squared / ((x - mean).powi(2) + gamma_squared)
}

fn erfc_fast(x: f64) -> f64 {
    if x < 0.0 {
        return 2.0 - erfc_fast(-x);
    }
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let polynomial = ((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t
        + 0.254829592)
        * t;
    polynomial * (-x * x).exp()
}

fn left_emg_value(x: f64, amplitude: f64, mean: f64, sigma: f64, tau: f64) -> f64 {
    let diff = x - mean;
    let inverse_tau = 1.0 / tau;
    let z = 0.5 * sigma * sigma * inverse_tau * inverse_tau + diff * inverse_tau;
    if z >= 700.0 {
        return 0.0;
    }
    let argument = (sigma * inverse_tau + diff / sigma) / 2.0_f64.sqrt();
    amplitude * 0.5 * inverse_tau * z.exp() * erfc_fast(argument)
}

fn single_left_hemg_value(x: f64, params: &[f64; 6]) -> f64 {
    let left = left_emg_value(x, params[0], params[1], params[2], params[3]);
    let second = left_emg_value(x, params[0], params[1], params[2], params[4]);
    params[5] * left + (1.0 - params[5]) * second
}

fn fit_quality(y_data: &[f64], curve: &[f64]) -> Option<f64> {
    if y_data.len() != curve.len() || y_data.is_empty() {
        return None;
    }
    let mean = y_data.iter().sum::<f64>() / y_data.len() as f64;
    let mut residual = 0.0;
    let mut total = 0.0;
    for (&observed, &fitted) in y_data.iter().zip(curve) {
        if !observed.is_finite() || !fitted.is_finite() {
            return None;
        }
        residual += (observed - fitted).powi(2);
        total += (observed - mean).powi(2);
    }
    if total <= f64::EPSILON {
        return None;
    }
    let r_squared = 1.0 - residual / total;
    r_squared.is_finite().then_some(r_squared)
}

fn curve_stats(x_data: &[f64], curve: &[f64], mean: f64) -> Option<(f64, f64, f64, f64)> {
    if x_data.len() != curve.len() || x_data.is_empty() {
        return None;
    }
    let (peak_index, &peak_y) = curve
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.total_cmp(right))?;
    if !peak_y.is_finite() || peak_y <= 0.0 {
        return None;
    }
    let half = peak_y / 2.0;
    let left = (0..=peak_index).rev().find(|&index| curve[index] <= half)?;
    let right = (peak_index..curve.len()).find(|&index| curve[index] <= half)?;
    if right <= left {
        return None;
    }
    let fwhm = x_data[right] - x_data[left];
    let peak_x = x_data[peak_index];
    if !fwhm.is_finite() || fwhm <= 0.0 || !peak_x.is_finite() || peak_x <= 0.0 {
        return None;
    }
    let weight = curve.iter().sum::<f64>();
    if !weight.is_finite() || weight <= 0.0 {
        return None;
    }
    let rms = (curve
        .iter()
        .zip(x_data)
        .map(|(&value, &x)| value * (x - mean).powi(2))
        .sum::<f64>()
        / weight)
        .sqrt();
    if !rms.is_finite() {
        return None;
    }
    Some((peak_y, peak_x, fwhm, rms))
}

fn measured_fwhm(x_data: &[f64], y_data: &[f64]) -> Option<f64> {
    if x_data.len() != y_data.len() || x_data.is_empty() {
        return None;
    }
    let (peak_index, &peak_y) = y_data
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.total_cmp(right))?;
    if peak_y <= 0.0 {
        return None;
    }
    let half = peak_y / 2.0;
    let left = (0..=peak_index).rev().find(|&index| y_data[index] <= half)?;
    let right = (peak_index..y_data.len()).find(|&index| y_data[index] <= half)?;
    (right > left).then_some(x_data[right] - x_data[left])
}

#[derive(Clone, Copy)]
enum ObservationPeakModel {
    Gaussian,
    Lorentzian,
}

fn refine_observation_peak(
    x_data: &[f64],
    y_data: &[f64],
    initial: [f64; 3],
    lower: [f64; 3],
    upper: [f64; 3],
    model: ObservationPeakModel,
) -> Option<[f64; 3]> {
    if x_data.len() != y_data.len() || x_data.is_empty() {
        return None;
    }
    let value = |x: f64, params: &[f64; 3]| match model {
        ObservationPeakModel::Gaussian => gaussian_value(x, params[0], params[1], params[2]),
        ObservationPeakModel::Lorentzian => lorentzian_value(x, params[0], params[1], params[2]),
    };
    let objective = |params: &[f64; 3]| {
        x_data
            .iter()
            .zip(y_data)
            .map(|(&x, &y)| (y - value(x, params)).powi(2))
            .sum::<f64>()
    };
    let mut params = initial;
    for index in 0..params.len() {
        params[index] = params[index].clamp(lower[index], upper[index]);
    }
    let mut best = objective(&params);
    if !best.is_finite() {
        return None;
    }
    let span = (upper[1] - lower[1]).max(1.0);
    let mut steps = [params[0].max(1.0) * 0.25, span * 0.1, params[2].max(0.1) * 0.25];
    for _ in 0..12 {
        for parameter in 0..params.len() {
            for direction in [-1.0, 1.0] {
                let mut candidate = params;
                candidate[parameter] =
                    (candidate[parameter] + direction * steps[parameter]).clamp(
                        lower[parameter],
                        upper[parameter],
                    );
                let score = objective(&candidate);
                if score.is_finite() && score < best {
                    params = candidate;
                    best = score;
                }
            }
        }
        for step in &mut steps {
            *step *= 0.5;
        }
    }
    Some(params)
}

fn finalize_observation_fit(
    x_data: &[f64],
    y_data: &[f64],
    mean: f64,
    width: f64,
    amplitude: f64,
    curve: Vec<f64>,
    model_fwhm: Option<f64>,
) -> Option<ObsFitCurve> {
    let roi_min = *x_data.first()?;
    let roi_max = *x_data.last()?;
    let (peak_index, _) = y_data
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.total_cmp(right))?;
    if peak_index == 0 || peak_index + 1 == y_data.len() {
        return None;
    }
    let local_pitch = match (peak_index.checked_sub(1), peak_index + 1 < x_data.len()) {
        (Some(left), true) => (x_data[peak_index] - x_data[left]).max(x_data[peak_index + 1] - x_data[peak_index]),
        (Some(left), false) => x_data[peak_index] - x_data[left],
        (None, true) => x_data[peak_index + 1] - x_data[peak_index],
        (None, false) => return None,
    };
    let r_squared = fit_quality(y_data, &curve)?;
    let (peak_y, peak_x, fwhm, rms) = curve_stats(x_data, &curve, mean)?;
    let width_valid = model_fwhm.map_or(
        fwhm.is_finite() && fwhm >= local_pitch,
        |value| value.is_finite() && value >= local_pitch,
    );
    if !amplitude.is_finite()
        || amplitude <= 0.0
        || !mean.is_finite()
        || mean < roi_min
        || mean > roi_max
        || !width.is_finite()
        || width <= 0.0
        || width > 50.0
        || !width_valid
        || !r_squared.is_finite()
        || r_squared <= 0.0
    {
        return None;
    }
    Some(ObsFitCurve {
        start: 0,
        curve,
        color: Color32::WHITE,
        label: String::new(),
        peak: peak_y,
        mu: mean,
        sigma: rms,
        fwhm,
        resolution: fwhm / peak_x * 100.0,
    })
}

fn fit_observation_gaussian(x_data: &[f64], y_data: &[f64]) -> Option<ObsFitCurve> {
    let (mean, sigma, amplitude) = weighted_moments(x_data, y_data)?;
    let params = refine_observation_peak(
        x_data,
        y_data,
        [amplitude, mean, sigma],
        [0.0, *x_data.first()?, 0.01],
        [f64::INFINITY, *x_data.last()?, 50.0],
        ObservationPeakModel::Gaussian,
    )?;
    let width = params[2];
    let curve: Vec<f64> = x_data
        .iter()
        .map(|&x| gaussian_value(x, params[0], params[1], width))
        .collect();
    finalize_observation_fit(
        x_data,
        y_data,
        params[1],
        width,
        params[0],
        curve,
        Some(2.355 * width),
    )
}

fn fit_observation_lorentzian(x_data: &[f64], y_data: &[f64]) -> Option<ObsFitCurve> {
    let (_moment_mean, moment_sigma, amplitude) = weighted_moments(x_data, y_data)?;
    let (peak_index, _) = y_data
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.total_cmp(right))?;
    let mean = x_data[peak_index];
    let width = measured_fwhm(x_data, y_data)
        .map(|fwhm| fwhm / 2.0)
        .unwrap_or(moment_sigma)
        .clamp(0.01, 50.0);
    let params = refine_observation_peak(
        x_data,
        y_data,
        [amplitude, mean, width],
        [0.0, *x_data.first()?, 0.01],
        [f64::INFINITY, *x_data.last()?, 50.0],
        ObservationPeakModel::Lorentzian,
    )?;
    let curve = x_data
        .iter()
        .map(|&x| lorentzian_value(x, params[0], params[1], params[2]))
        .collect();
    finalize_observation_fit(
        x_data,
        y_data,
        params[1],
        params[2],
        params[0],
        curve,
        Some(2.0 * params[2]),
    )
}

fn observation_x_pitch(x_data: &[f64]) -> f64 {
    let (sum, count) = x_data
        .windows(2)
        .filter_map(|window| {
            let pitch = window[1] - window[0];
            (pitch.is_finite() && pitch > 0.0).then_some(pitch)
        })
        .fold((0.0, 0usize), |(sum, count), pitch| (sum + pitch, count + 1));
    if count == 0 {
        1.0
    } else {
        sum / count as f64
    }
}

fn solve_observation_single_left_hemg(
    x_data: &[f64],
    y_data: &[f64],
) -> Option<([f64; 6], Vec<f64>)> {
    let (mean, _sigma, _peak) = weighted_moments(x_data, y_data)?;
    let amplitude = y_data
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
        .sum::<f64>()
        * observation_x_pitch(x_data);
    if !amplitude.is_finite() || amplitude <= 0.0 {
        return None;
    }
    let mut params = [
        amplitude,
        mean.clamp(*x_data.first()?, *x_data.last()?),
        weighted_sample_std(x_data, y_data, mean).clamp(0.01, 50.0),
        0.5,
        1.5,
        0.5,
    ];
    let lower = [0.0, *x_data.first()?, 0.01, 0.05, 0.05, 0.0];
    let upper = [f64::INFINITY, *x_data.last()?, 50.0, 5.0, 5.0, 1.0];
    let objective = |candidate: &[f64; 6]| {
        x_data
            .iter()
            .zip(y_data)
            .map(|(&x, &y)| (y - single_left_hemg_value(x, candidate)).powi(2))
            .sum::<f64>()
    };
    let mut best = objective(&params);
    let mut steps = [
        amplitude.max(1.0) * 0.25,
        (upper[1] - lower[1]).max(1.0) * 0.1,
        params[2].max(0.1) * 0.25,
        0.5,
        0.5,
        0.2,
    ];
    for _ in 0..10 {
        for parameter in 0..params.len() {
            for direction in [-1.0, 1.0] {
                let mut candidate = params;
                candidate[parameter] = (candidate[parameter] + direction * steps[parameter])
                    .clamp(lower[parameter], upper[parameter]);
                let score = objective(&candidate);
                if score.is_finite() && score < best {
                    params = candidate;
                    best = score;
                }
            }
        }
        for step in &mut steps {
            *step *= 0.5;
        }
    }
    let curve: Vec<f64> = x_data
        .iter()
        .map(|&x| single_left_hemg_value(x, &params))
        .collect();
    Some((params, curve))
}

fn fit_observation_single_left_hemg(x_data: &[f64], y_data: &[f64]) -> Option<ObsFitCurve> {
    let (params, curve) = solve_observation_single_left_hemg(x_data, y_data)?;
    let mut fit = finalize_observation_fit(
        x_data,
        y_data,
        params[1],
        params[2],
        params[0],
        curve,
        None,
    )?;
    fit.mu = params[1];
    Some(fit)
}

fn analyze_files_worker(files: Vec<PathBuf>, tx: Sender<WorkerMsg>, run_id: u64) {
    let mut processor = ObservationDataProcessor::new();
    let _ = tx.send(WorkerMsg::Status {
        run_id,
        text: "Processing...".to_string(),
    });
    let mut last_reported = 0u64;
    let processed = processor.process_files_with_progress(&files, |processed, total| {
        if processed == total || processed.saturating_sub(last_reported) >= 64 * 1024 {
            last_reported = processed;
            let _ = tx.send(WorkerMsg::Progress {
                run_id,
                processed,
                total,
            });
        }
    });
    match processed {
        Ok(histogram_data) => {
            if processor.valid_packet_count() == 0 {
                let _ = tx.send(WorkerMsg::Error {
                    run_id,
                    text: "No valid observation packets found.".to_string(),
                });
                return;
            }
            let raw_histogram_data = processor.raw_histogram_data();
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
                    particle_time_raw: r.particle_time_raw,
                    dssd_position: r.dssd_positions,
                    dssd: r.dssd_pulses,
                    bgo: r.bgo_pulses,
                    line_extra: r.line_tail,
                })
                .collect();
            let _ = tx.send(WorkerMsg::Complete {
                run_id,
                histogram_data,
                raw_histogram_data,
                events,
            });
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error {
                run_id,
                text: format!("Error! {e}"),
            });
        }
    }
}

//! Port of Calibration mode

use baseline_core::calibration::{
    calibration_block_fields, parse_calibration_line, CalibrationAccumulator, CalibrationLineResult,
    CALIBRATION_TAIL_FIELDS,
};
use baseline_core::io;
use baseline_core::math::MathService;
use baseline_core::models::shared::AppConstants;
use baseline_core::observation::data_processor::{split_hex_data, validate_header};
use chrono::{DateTime, Utc};
use egui::Color32;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use crate::channel::{channel_block_ui, ChannelState};
use crate::fit_overlay::{self, FitOverlayFlags};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CalibrationTab {
    ChannelView,
    DataTable,
    Json,
}

/// One decoded raw line for the Data Table tab. `blocks` holds the 44 block
/// columns (11 voltage steps x 4 layers L1/L2/L6/L7, matching
/// `calibration_block_fields`), each with its 16 decoded decimal values.
#[derive(Debug, Clone)]
struct CalibrationRow {
    packet_sync: String,
    package_id: i32,
    packet_sequence: i32,
    packet_data_length: i32,
    time: DateTime<Utc>,
    data_type: String,
    sample_index: i32,
    blocks: Vec<Vec<i32>>,
    /// Values for `CALIBRATION_TAIL_FIELDS`, in that same order.
    tail: Vec<String>,
    reserved_hex: String,
    checksum_hex: String,
}

/// Joins one block's 16 decoded values into a comma-separated cell for the
/// Data Table / CSV export.
fn join_i32_values(values: &[i32]) -> String {
    values.iter().map(i32::to_string).collect::<Vec<_>>().join(", ")
}

impl From<CalibrationLineResult> for CalibrationRow {
    fn from(r: CalibrationLineResult) -> Self {
        Self {
            packet_sync: r.packet_sync,
            package_id: r.package_id,
            packet_sequence: r.packet_sequence,
            packet_data_length: r.packet_data_length,
            time: r.time,
            data_type: r.data_type,
            sample_index: r.sample_index,
            blocks: r.blocks,
            tail: r.tail,
            reserved_hex: r.reserved_hex,
            checksum_hex: r.checksum_hex,
        }
    }
}

/// "YYYY-MM-DD HH:MM:SS.mmm", matching Observation mode's Data Table.
fn format_event_time(time: DateTime<Utc>) -> String {
    time.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

const TIME_SAMPLE_TEXT: &str = "0000-00-00 00:00:00.000";

/// Rendered pixel width of `text` in the current `Body` text style
fn text_render_width(ui: &egui::Ui, text: &str) -> f32 {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    ui.fonts(|f| f.layout_no_wrap(text.to_string(), font_id, Color32::WHITE).size().x)
}

/// Fits the Data Table's column widths to their content
fn header_column_width(ui: &egui::Ui, header: &str) -> f32 {
    const PADDING: f32 = 20.0;
    const MIN_WIDTH: f32 = 60.0;
    const LIST_MIN_WIDTH: f32 = 260.0;
    const RESERVED_MIN_WIDTH: f32 = 900.0;
    let mut text_width = text_render_width(ui, header);
    if header.starts_with("Time ") {
        text_width = text_width.max(text_render_width(ui, TIME_SAMPLE_TEXT));
    }
    let min_width = if header.contains("V L") {
        LIST_MIN_WIDTH
    } else if header.starts_with("Reserved") {
        RESERVED_MIN_WIDTH
    } else {
        MIN_WIDTH
    };
    (text_width + PADDING).max(min_width)
}

/// Header labels for the Data Table tab and its CSV export, in column order.
fn calibration_table_headers() -> Vec<String> {
    let mut headers = vec![
        "Packet Sync Code (Byte 0-1)".to_string(),
        "Package ID (Byte 2-3)".to_string(),
        "Packet Seq (Byte 4-5)".to_string(),
        "Packet Data Len (Byte 6-7)".to_string(),
        "Time (Byte 8-13)".to_string(),
        "Data Type (Byte 14-15)".to_string(),
        "Sample Index (Byte 16-17)".to_string(),
    ];
    for field in calibration_block_fields() {
        headers.push(format!("{} (Byte {}-{})", field.label, field.byte_start, field.byte_start + 31));
    }
    for field in CALIBRATION_TAIL_FIELDS {
        headers.push(format!("{} (Byte {}-{})", field.label, field.byte_start, field.byte_start + 1));
    }
    headers.push("Reserved (Byte 1446-2061)".to_string());
    headers.push("Checksum (Byte 2062-2063)".to_string());
    headers
}

/// One row's cells for the Data Table tab and its CSV export
fn calibration_row_cells(row: &CalibrationRow) -> Vec<String> {
    let mut cells = vec![
        row.packet_sync.clone(),
        row.package_id.to_string(),
        row.packet_sequence.to_string(),
        row.packet_data_length.to_string(),
        format_event_time(row.time),
        row.data_type.clone(),
        row.sample_index.to_string(),
    ];
    cells.extend(row.blocks.iter().map(|b| join_i32_values(b)));
    cells.extend(row.tail.iter().cloned());
    cells.push(row.reserved_hex.clone());
    cells.push(row.checksum_hex.clone());
    cells
}

/// Quotes a CSV field if it contains a comma, quote, or newline (the 44
/// block-column cells always do, being comma-joined lists of 16 values).
fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// Tail keys for the JSON view
const CALIBRATION_JSON_TAIL_KEYS: [&str; 10] = [
    "dssd1_temperature",
    "fee1_current",
    "fee1_temperature",
    "dssd7_temperature",
    "fee2_current",
    "fee2_temperature",
    "fee1_threshold",
    "fee2_threshold",
    "dssd1_temperature_dup",
    "dssd4_temperature",
];

/// Number of voltage steps in the calibration payload (`00v`..`10v`).
const CALIBRATION_VOLTAGE_STEPS: usize = 11;

/// One Data Table row as a JSON object
fn calibration_row_json(row: &CalibrationRow) -> serde_json::Value {
    use crate::json_view::{num_or_str, object};

    let steps = object((0..CALIBRATION_VOLTAGE_STEPS).map(|step| {
        let base = step * 4;
        let layers = object(
            ["l1", "l2", "l6", "l7"]
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let list = row
                        .blocks
                        .get(base + i)
                        .map(|b| serde_json::Value::from(b.clone()))
                        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
                    (name.to_string(), list)
                }),
        );
        (format!("{step:02}v"), layers)
    }));
    let tail = object(
        CALIBRATION_JSON_TAIL_KEYS
            .iter()
            .zip(&row.tail)
            .map(|(k, v)| (k.to_string(), num_or_str(v))),
    );

    object([
        (
            "header".to_string(),
            object([
                ("packet_sync_code".to_string(), num_or_str(&row.packet_sync)),
                ("packet_id".to_string(), row.package_id.into()),
                ("pakcet_seq".to_string(), row.packet_sequence.into()),
                ("packet_data_len".to_string(), row.packet_data_length.into()),
                ("time".to_string(), format_event_time(row.time).into()),
                ("data_type".to_string(), row.data_type.clone().into()),
                ("sample_index".to_string(), row.sample_index.into()),
            ]),
        ),
        ("calibration_voltage_step".to_string(), steps),
        ("tail".to_string(), tail),
        ("reserved".to_string(), row.reserved_hex.clone().into()),
        ("checksum".to_string(), row.checksum_hex.clone().into()),
    ])
}

fn calibration_rows_json(rows: &[CalibrationRow]) -> Vec<serde_json::Value> {
    rows.iter().map(calibration_row_json).collect()
}

enum WorkerMsg {
    Status(String, Color32),
    Busy(bool),
    HeaderCheck(String),
    Progress(f64),
    DataLoaded(CalibrationAccumulator),
    TableRows(Vec<CalibrationRow>),
    Error(String),
}

pub struct CalibrationMode {
    input_files: Vec<PathBuf>,
    input_files_info: String,
    output_file_name: String,

    selected_layer_index: usize,  // 0=L1,1=L2,2=L6,3=L7
    selected_x_axis_index: usize, // 0=ADC,1=Voltage
    x_axis_min: f64,
    x_axis_max: f64,
    delay_time: i32,
    threshold: i32,

    status_message: String,
    is_busy: bool,
    progress_value: f64,
    header_check_status: String,

    show_gaussian_fit: bool,
    show_hemg_single_fit: bool,
    show_hemg_double_fit: bool,
    show_lorentzian_fit: bool,
    math: MathService,

    accumulator: CalibrationAccumulator,
    channels: Vec<ChannelState>,

    active_tab: CalibrationTab,
    /// Data Table tab
    rows: Vec<CalibrationRow>,
    /// Pretty-printed JSON of the first rows, for the JSON tab's preview.
    json_preview: String,

    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
}

impl Default for CalibrationMode {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let channels = (0..16)
            .map(|i| {
                let name = if i < 8 {
                    format!("X{}", i + 1)
                } else {
                    format!("Z{}", i - 7)
                };
                ChannelState {
                    channel_index: i,
                    title: name,
                    stats_text: String::new(),
                    ..Default::default()
                }
            })
            .collect();

        Self {
            input_files: Vec::new(),
            input_files_info: "No files selected".to_string(),
            output_file_name: "CalibrationResult".to_string(),
            selected_layer_index: 0,
            selected_x_axis_index: 0,
            x_axis_min: 0.0,
            x_axis_max: 16384.0,
            delay_time: 50,
            threshold: 50,
            status_message: "Ready".to_string(),
            is_busy: false,
            progress_value: 0.0,
            header_check_status: String::new(),
            show_gaussian_fit: true,
            show_hemg_single_fit: false,
            show_hemg_double_fit: false,
            show_lorentzian_fit: false,
            math: MathService::new(),
            accumulator: CalibrationAccumulator::default(),
            channels,
            active_tab: CalibrationTab::ChannelView,
            rows: Vec::new(),
            json_preview: String::new(),
            tx,
            rx,
        }
    }
}

impl CalibrationMode {
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        export: &mut crate::plot_export::PlotExportQueue,
    ) {
        self.drain_messages();

        egui::TopBottomPanel::top("calibration_top").show(ctx, |ui| {
            ui.heading("Calibration Mode");
            ui.separator();
            ui.label(&self.input_files_info);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.is_busy, egui::Button::new("Select Files"))
                    .clicked()
                {
                    self.select_files();
                }
                if ui
                    .add_enabled(!self.is_busy, egui::Button::new("Check Header"))
                    .clicked()
                {
                    self.check_header();
                }
            });

            ui.horizontal(|ui| {
                ui.label("Output File Name:");
                ui.text_edit_singleline(&mut self.output_file_name);
            });

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.is_busy && !self.input_files.is_empty(),
                        egui::Button::new("Pre-Process to Excel"),
                    )
                    .clicked()
                {
                    self.process_data();
                }
                if ui
                    .add_enabled(!self.is_busy, egui::Button::new("Process Data"))
                    .clicked()
                {
                    self.read_data();
                }
                if ui.button("Reset").clicked() {
                    self.reset();
                }
            });

            ui.separator();
            egui::ComboBox::from_label("Layer")
                .selected_text(["L1", "L2", "L6", "L7"][self.selected_layer_index])
                .show_ui(ui, |ui| {
                    for (i, name) in ["L1", "L2", "L6", "L7"].iter().enumerate() {
                        if ui
                            .selectable_value(&mut self.selected_layer_index, i, *name)
                            .changed()
                        {
                            self.update_plots(true);
                        }
                    }
                });
            if egui::ComboBox::from_label("X Axis")
                .selected_text(["ADC", "Voltage"][self.selected_x_axis_index])
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.selected_x_axis_index, 0, "ADC");
                    ui.selectable_value(&mut self.selected_x_axis_index, 1, "Voltage");
                })
                .response
                .changed()
            {
                if self.selected_x_axis_index == 1 {
                    self.x_axis_min = 0.0;
                    self.x_axis_max = 5000.0;
                } else {
                    self.x_axis_min = 0.0;
                    self.x_axis_max = 16384.0;
                }
                self.update_plots(true);
            }
            ui.horizontal(|ui| {
                ui.label("X Min");
                let min_resp = ui.add(egui::DragValue::new(&mut self.x_axis_min));
                if min_resp.changed() {
                    self.update_plots(false);
                }
                if min_resp.drag_stopped() || min_resp.lost_focus() {
                    self.update_plots(true);
                }
                ui.label("X Max");
                let max_resp = ui.add(egui::DragValue::new(&mut self.x_axis_max));
                if max_resp.changed() {
                    self.update_plots(false);
                }
                if max_resp.drag_stopped() || max_resp.lost_focus() {
                    self.update_plots(true);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Delay (ms)");
                ui.add(egui::DragValue::new(&mut self.delay_time));
                ui.label("Threshold");
                ui.add(egui::DragValue::new(&mut self.threshold));
            });

            ui.horizontal_wrapped(|ui| {
                ui.label("Fits:");
                let mut fits_changed = false;
                fits_changed |= ui
                    .checkbox(&mut self.show_gaussian_fit, "Gaussian")
                    .changed();
                fits_changed |= ui
                    .checkbox(&mut self.show_hemg_single_fit, "HEMG single")
                    .changed();
                fits_changed |= ui
                    .checkbox(&mut self.show_hemg_double_fit, "HEMG double")
                    .changed();
                fits_changed |= ui
                    .checkbox(&mut self.show_lorentzian_fit, "Lorentzian")
                    .changed();
                if fits_changed {
                    self.update_plots(true);
                }
            });

            ui.separator();
            ui.colored_label(Color32::LIGHT_GRAY, &self.status_message);
            if self.is_busy {
                ui.add(
                    egui::ProgressBar::new((self.progress_value / 100.0) as f32).show_percentage(),
                );
            }
            if !self.header_check_status.is_empty() {
                ui.label(&self.header_check_status);
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active_tab, CalibrationTab::ChannelView, "Channel View");
                ui.selectable_value(&mut self.active_tab, CalibrationTab::DataTable, "Data Table");
                ui.selectable_value(&mut self.active_tab, CalibrationTab::Json, "JSON");
            });
            ui.separator();

            match self.active_tab {
                CalibrationTab::ChannelView => {
                    egui::ScrollArea::vertical().id_salt("cal_channel_scroll").show(ui, |ui| {
                        let (x_channels, z_channels) = self.channels.split_at_mut(8);
                        ui.columns(2, |columns| {
                            channel_block_ui(&mut columns[0], "X-direction (X1-X8)", x_channels, export);
                            channel_block_ui(&mut columns[1], "Z-direction (Z1-Z8)", z_channels, export);
                        });
                    });
                }
                CalibrationTab::DataTable => self.data_table_ui(ui),
                CalibrationTab::Json => self.json_tab_ui(ui),
            }
        });

        if self.is_busy {
            ctx.request_repaint();
        }
    }

    fn drain_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Status(s, _c) => self.status_message = s,
                WorkerMsg::Busy(b) => self.is_busy = b,
                WorkerMsg::HeaderCheck(s) => self.header_check_status = s,
                WorkerMsg::Progress(p) => self.progress_value = p,
                WorkerMsg::DataLoaded(acc) => {
                    self.accumulator = acc;
                    self.update_plots(true);
                }
                WorkerMsg::TableRows(rows) => {
                    self.json_preview =
                        crate::json_view::preview_string(&calibration_rows_json(&rows));
                    self.rows = rows;
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
        self.accumulator = CalibrationAccumulator::default();
        self.rows.clear();
        self.json_preview.clear();
        for ch in &mut self.channels {
            ch.counts.clear();
            ch.bin_centers.clear();
            ch.stats_text.clear();
            ch.active_fits.clear();
            ch.mu = 0.0;
            ch.sigma = 0.0;
            ch.fwhm = 0.0;
            ch.resolution = 0.0;
        }
        self.status_message = "Reset complete.".to_string();
        self.progress_value = 0.0;
        self.header_check_status.clear();
    }

    fn select_files(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .add_filter("Text files", &["txt"])
            .pick_files()
        {
            self.input_files_info = format!("{} txt file(s) selected", files.len());
            self.input_files = files;
            self.status_message = "Files selected.".to_string();
        }
    }

    fn check_header(&mut self) {
        self.is_busy = true;
        self.header_check_status = "Checking...".to_string();
        let output_name = self.output_file_name.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || header_check_worker(output_name, tx));
    }

    fn process_data(&mut self) {
        self.is_busy = true;
        self.status_message = "Processing raw data...".to_string();
        let files = self.input_files.clone();
        let output_name = self.output_file_name.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || process_data_worker(files, output_name, tx));
    }

    fn read_data(&mut self) {
        if self.input_files.is_empty() {
            self.status_message = "Select raw .txt files first.".to_string();
            return;
        }
        self.is_busy = true;
        self.progress_value = 0.0;
        self.accumulator = CalibrationAccumulator::default();
        self.rows.clear();
        self.json_preview.clear();

        let files = self.input_files.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || read_data_worker(files, tx));
    }

    /// Rebuilds histograms for the selected layer/axis
    fn update_plots(&mut self, recompute_fits: bool) {
        let source = if self.selected_x_axis_index == 1 {
            self.accumulator.voltage_columns(self.selected_layer_index)
        } else {
            self.accumulator.columns(self.selected_layer_index)
        };

        let flags = FitOverlayFlags {
            gaussian: self.show_gaussian_fit,
            lorentzian: self.show_lorentzian_fit,
            hemg_single: self.show_hemg_single_fit,
            hemg_double: self.show_hemg_double_fit,
        };

        for ch in 0..16 {
            let data_for_channel: Vec<f64> =
                source[ch].iter().copied().filter(|&d| d > 0.0).collect();
            if data_for_channel.is_empty() {
                self.channels[ch].active_fits.clear();
                continue;
            }
            let (counts, bin_centers) =
                baseline_core::baseline_processing::build_histogram_avg_centers(
                    &data_for_channel,
                    self.x_axis_min,
                    self.x_axis_max,
                    500,
                );

            if recompute_fits {
                let (stats, fits) =
                    fit_overlay::compute_fits(&self.math, &bin_centers, &counts, &flags);
                self.channels[ch].mu = stats.mu;
                self.channels[ch].sigma = stats.sigma;
                self.channels[ch].fwhm = stats.fwhm;
                self.channels[ch].resolution = stats.resolution;
                self.channels[ch].active_fits = fits;
                self.channels[ch].stats_text = format!(
                    "Counts: {}  mu={:.2} sigma={:.2} fwhm={:.2} res={:.2}%",
                    data_for_channel.len(),
                    stats.mu,
                    stats.sigma,
                    stats.fwhm,
                    stats.resolution
                );
            } else {
                // Bin centers are about to change under it; drop the stale
                // curve rather than draw it mismatched against the new bars
                // until the drag settles and fits are recomputed.
                self.channels[ch].active_fits.clear();
                self.channels[ch].stats_text = format!("Counts: {}", data_for_channel.len());
            }
            self.channels[ch].counts = counts;
            self.channels[ch].bin_centers = bin_centers;
        }
    }

    /// Data Table tab: one row per raw line, decoded per `Calibration.txt`.
    fn data_table_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.rows.is_empty(), egui::Button::new("Export Table as CSV...")).clicked() {
                self.export_table_csv();
            }
            ui.label(format!("{} row(s)", self.rows.len()));
        });
        ui.separator();

        let headers = calibration_table_headers();
        let widths: Vec<f32> = headers.iter().map(|h| header_column_width(ui, h)).collect();

        egui::ScrollArea::horizontal().id_salt("cal_data_table_hscroll").show(ui, |ui| {
            let row_height = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;

            egui::Grid::new("cal_data_table_header").show(ui, |ui| {
                for (header, &width) in headers.iter().zip(&widths) {
                    ui.add_sized([width, row_height], egui::Label::new(egui::RichText::new(header).strong()).truncate());
                }
                ui.end_row();
            });

            egui::ScrollArea::vertical().id_salt("cal_data_table_vscroll").show_rows(ui, row_height, self.rows.len(), |ui, visible_range| {
                egui::Grid::new("cal_data_table_body").striped(true).start_row(visible_range.start).show(ui, |ui| {
                    for row in &self.rows[visible_range] {
                        let cells = calibration_row_cells(row);
                        for (cell, &width) in cells.iter().zip(&widths) {
                            ui.add_sized([width, row_height], egui::Label::new(cell).truncate());
                        }
                        ui.end_row();
                    }
                });
            });
        });
    }

    /// Export the data table as CSV. Value-list cells (L1/L2/L6/L7)
    fn export_table_csv(&mut self) {
        if self.rows.is_empty() {
            self.status_message = "No rows to export - run Read Data first.".to_string();
            return;
        }
        let Some(path) = rfd::FileDialog::new().set_file_name("calibration_table.csv").add_filter("CSV", &["csv"]).save_file() else {
            return;
        };

        let mut csv = calibration_table_headers().iter().map(|h| csv_field(h)).collect::<Vec<_>>().join(",");
        csv.push('\n');
        for row in &self.rows {
            csv.push_str(&calibration_row_cells(row).iter().map(|c| csv_field(c)).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }

        match std::fs::write(&path, csv) {
            Ok(()) => self.status_message = format!("Exported {} row(s) to {}", self.rows.len(), path.display()),
            Err(e) => self.status_message = format!("Failed to export CSV: {e}"),
        }
    }

    /// JSON tab: the decoded rows as JSON with preview and downloader
    fn json_tab_ui(&mut self, ui: &mut egui::Ui) {
        if crate::json_view::json_tab_ui(ui, self.rows.len(), &self.json_preview) {
            match crate::json_view::save_json(
                "calibration_data.json",
                &calibration_rows_json(&self.rows),
            ) {
                Ok(Some(path)) => {
                    self.status_message =
                        format!("Saved {} row(s) to {}", self.rows.len(), path.display())
                }
                Ok(None) => {}
                Err(e) => self.status_message = format!("Failed to save JSON: {e}"),
            }
        }
    }
}

fn process_data_worker(files: Vec<PathBuf>, output_name: String, tx: Sender<WorkerMsg>) {
    match io::segment_filter::filter_e225_segments_from_files(
        &files,
        baseline_core::models::shared::AppConstants::SEGMENT_HEX_LENGTH,
    ) {
        Ok(segments) if !segments.is_empty() => {
            let _ = tx.send(WorkerMsg::Status(
                format!("Saving {} segments to Excel...", segments.len()),
                Color32::GRAY,
            ));
            match io::file_helper::resolve_excel_save_path(&output_name, "Calibration") {
                Ok(path) => match io::excel::save_lines_to_excel(&segments, &path) {
                    Ok(()) => {
                        let _ = tx.send(WorkerMsg::Status(
                            "Processing complete.".to_string(),
                            Color32::from_rgb(50, 200, 50),
                        ));
                    }
                    Err(e) => {
                        let _ = tx.send(WorkerMsg::Error(e));
                    }
                },
                Err(e) => {
                    let _ = tx.send(WorkerMsg::Error(e));
                }
            }
        }
        Ok(_) => {
            let _ = tx.send(WorkerMsg::Status(
                "No valid segments found.".to_string(),
                Color32::RED,
            ));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Error: {e}")));
        }
    }
    let _ = tx.send(WorkerMsg::Progress(100.0));
    let _ = tx.send(WorkerMsg::Busy(false));
}

/// Decodes the selected raw `.txt` files directly (no Excel round-trip):
/// filters E225 segments, then decodes each into the accumulator + a
/// `CalibrationRow` that feeds the Data Table and JSON tabs.
fn read_data_worker(files: Vec<PathBuf>, tx: Sender<WorkerMsg>) {
    let segments = match io::segment_filter::filter_e225_segments_from_files(
        &files,
        AppConstants::SEGMENT_HEX_LENGTH,
    ) {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Error: {e}")));
            let _ = tx.send(WorkerMsg::Busy(false));
            return;
        }
    };

    if segments.is_empty() {
        let _ = tx.send(WorkerMsg::Status(
            "No valid E225 segments found.".to_string(),
            Color32::RED,
        ));
        let _ = tx.send(WorkerMsg::Busy(false));
        return;
    }

    let total = segments.len();
    let mut accumulator = CalibrationAccumulator::default();
    accumulator.reset(total.max(1));
    let mut table_rows: Vec<CalibrationRow> = Vec::with_capacity(total);

    for (index, segment) in segments.iter().enumerate() {
        let hex_data = split_hex_data(segment);

        if index == 0 {
            let valid = validate_header(&hex_data);
            let _ = tx.send(WorkerMsg::HeaderCheck(if valid {
                "Checksum OK".to_string()
            } else {
                "Checksum Mismatch".to_string()
            }));
        }

        accumulator.process_calibration(&hex_data);
        if let Some(line) = parse_calibration_line(&hex_data) {
            table_rows.push(line.into());
        }

        if index % 1000 == 0 {
            let progress = (index as f64 / total.max(1) as f64) * 100.0;
            let _ = tx.send(WorkerMsg::Progress(progress));
            let _ = tx.send(WorkerMsg::Status(
                format!("Reading data: {index}/{total} segments"),
                Color32::GRAY,
            ));
        }
    }

    let _ = tx.send(WorkerMsg::DataLoaded(accumulator));
    let _ = tx.send(WorkerMsg::TableRows(table_rows));

    let _ = tx.send(WorkerMsg::Status(
        "Complete!".to_string(),
        Color32::from_rgb(50, 200, 50),
    ));
    let _ = tx.send(WorkerMsg::Progress(100.0));
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn header_check_worker(output_name: String, tx: Sender<WorkerMsg>) {
    let Some(file_path) = io::file_helper::find_excel_file(&output_name, "Calibration") else {
        let _ = tx.send(WorkerMsg::Error(format!(
            "File not found: {output_name}.xlsx (searched Documents/DSSD_Analysis/Calibration and legacy locations)"
        )));
        let _ = tx.send(WorkerMsg::Busy(false));
        return;
    };

    match io::excel::read_excel_column_a(&file_path) {
        Ok(rows) => {
            let mut result = "Header is correct!".to_string();
            for (i, hex) in rows.iter().enumerate() {
                if !hex.starts_with(AppConstants::HEADER_START) {
                    result = format!("Header is INCORRECT! at data row no. {}", i + 1);
                    break;
                }
            }
            let _ = tx.send(WorkerMsg::HeaderCheck(result));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Error reading file: {e}")));
        }
    }
    let _ = tx.send(WorkerMsg::Busy(false));
}

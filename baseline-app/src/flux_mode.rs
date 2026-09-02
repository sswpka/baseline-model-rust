//! Port of Flux mode

use baseline_core::flux::{parse_flux_line, FluxAccumulator, FluxLineResult, FLUX_TAIL_FIELDS, PARTICLE_COUNTS_LAYERS};
use baseline_core::io;
use baseline_core::models::shared::AppConstants;
use baseline_core::observation::data_processor::split_hex_data;
use chrono::{DateTime, Utc};
use egui::Color32;
use egui_plot::{Legend, Line, Plot, PlotPoints};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

const LAYER_COUNT: usize = 7;
/// Detector area in m^2 (32mm x 32mm).
const DETECTOR_AREA_M2: f64 = 32.0 * 32.0 * 1e-6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FluxTab {
    Plots,
    DataTable,
    Json,
}

#[derive(Debug, Clone, Default)]
struct LayerPlot {
    name: String,
    stats_text: String,
    x_data: Vec<f64>,
    y_data: Vec<f64>,
}

/// One decoded raw line for the Data Table tab
#[derive(Debug, Clone)]
struct FluxRow {
    packet_sync: String,
    package_id: i32,
    packet_sequence: i32,
    packet_data_length: i32,
    time: DateTime<Utc>,
    data_type: String,
    particle_time: i32,
    /// Particle Counts L1-L7 (Byte 18-31), in that order
    particle_counts: Vec<i32>,
    /// Particle Info (Byte 32-2031): 1000 decoded decimal values.
    particle_info: Vec<i32>,
    /// Values for `FLUX_TAIL_FIELDS`, in that same order
    tail: Vec<String>,
    reserved_hex: String,
    checksum_hex: String,
}

impl From<FluxLineResult> for FluxRow {
    fn from(r: FluxLineResult) -> Self {
        Self {
            packet_sync: r.packet_sync,
            package_id: r.package_id,
            packet_sequence: r.packet_sequence,
            packet_data_length: r.packet_data_length,
            time: r.time,
            data_type: r.data_type,
            particle_time: r.particle_time,
            particle_counts: r.particle_counts,
            particle_info: r.particle_info,
            tail: r.tail,
            reserved_hex: r.reserved_hex,
            checksum_hex: r.checksum_hex,
        }
    }
}

/// "YYYY-MM-DD HH:MM:SS.mmm", matching Baseline/Calibration/Observation
/// mode's Data Table.
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
    const INFO_MIN_WIDTH: f32 = 900.0;
    const RESERVED_MIN_WIDTH: f32 = 220.0;
    let mut text_width = text_render_width(ui, header);
    if header.starts_with("Time ") {
        text_width = text_width.max(text_render_width(ui, TIME_SAMPLE_TEXT));
    }
    let min_width = if header.starts_with("Particle Info") {
        INFO_MIN_WIDTH
    } else if header.starts_with("Reserved") {
        RESERVED_MIN_WIDTH
    } else {
        MIN_WIDTH
    };
    (text_width + PADDING).max(min_width)
}

/// Header labels for the Data Table tab and its CSV export, in column order.
fn flux_table_headers() -> Vec<String> {
    let mut headers = vec![
        "Packet Sync Code (Byte 0-1)".to_string(),
        "Package ID (Byte 2-3)".to_string(),
        "Packet Seq (Byte 4-5)".to_string(),
        "Packet Data Len (Byte 6-7)".to_string(),
        "Time (Byte 8-13)".to_string(),
        "Data Type (Byte 14-15)".to_string(),
        "Particle Time (Byte 16-17)".to_string(),
    ];
    for i in 0..PARTICLE_COUNTS_LAYERS {
        let start = 18 + i * 2;
        headers.push(format!("Particle Counts L{} (Byte {start}-{})", i + 1, start + 1));
    }
    headers.push("Particle Info (Byte 32-2031)".to_string());
    for field in FLUX_TAIL_FIELDS {
        headers.push(format!("{} (Byte {}-{})", field.label, field.byte_start, field.byte_start + 1));
    }
    headers.push("Reserved (Byte 2056-2061)".to_string());
    headers.push("Checksum (Byte 2062-2063)".to_string());
    headers
}

/// One row's cells for the Data Table tab and its CSV export
fn flux_row_cells(row: &FluxRow) -> Vec<String> {
    let mut cells = vec![
        row.packet_sync.clone(),
        row.package_id.to_string(),
        row.packet_sequence.to_string(),
        row.packet_data_length.to_string(),
        format_event_time(row.time),
        row.data_type.clone(),
        row.particle_time.to_string(),
    ];
    cells.extend(row.particle_counts.iter().map(|c| c.to_string()));
    cells.push(row.particle_info.iter().map(i32::to_string).collect::<Vec<_>>().join(", "));
    cells.extend(row.tail.iter().cloned());
    cells.push(row.reserved_hex.clone());
    cells.push(row.checksum_hex.clone());
    cells
}

/// Quotes a CSV field if it contains a comma, quote, or newline (the
/// Particle Info cell always does, being a comma-joined list).
fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// Tail keys for the JSON view
const FLUX_JSON_TAIL_KEYS: [&str; 12] = [
    "dssd_1_temperature",
    "fee1_current",
    "fee1_temperature",
    "dssd_7_temperature",
    "fee2_current",
    "fee2_temperature",
    "fee1_threshold",
    "fee2_threshold",
    "bgo1_bias_voltage",
    "bgo2_bias_voltage",
    "bgo3_bias_voltage",
    "bgo2_temperature",
];

/// One Data Table row as a JSON object
fn flux_row_json(row: &FluxRow) -> serde_json::Value {
    use crate::json_view::{num_or_str, object};

    let mut pairs: Vec<(String, serde_json::Value)> = vec![(
        "header".to_string(),
        object([
            ("packet_sync_code".to_string(), num_or_str(&row.packet_sync)),
            ("packet_id".to_string(), row.package_id.into()),
            ("pakcet_seq".to_string(), row.packet_sequence.into()),
            ("packet_data_len".to_string(), row.packet_data_length.into()),
            ("time".to_string(), format_event_time(row.time).into()),
            ("data_type".to_string(), row.data_type.clone().into()),
            ("particle_time".to_string(), row.particle_time.into()),
        ]),
    )];
    for (i, count) in row.particle_counts.iter().enumerate() {
        pairs.push((format!("particle_counts_l{}", i + 1), (*count).into()));
    }
    pairs.push((
        "particle_info".to_string(),
        serde_json::Value::from(row.particle_info.clone()),
    ));
    pairs.push((
        "tail".to_string(),
        object(
            FLUX_JSON_TAIL_KEYS
                .iter()
                .zip(&row.tail)
                .map(|(k, v)| (k.to_string(), num_or_str(v))),
        ),
    ));
    pairs.push(("reserved".to_string(), row.reserved_hex.clone().into()));
    pairs.push(("checksum".to_string(), row.checksum_hex.clone().into()));
    object(pairs)
}

fn flux_rows_json(rows: &[FluxRow]) -> Vec<serde_json::Value> {
    rows.iter().map(flux_row_json).collect()
}

enum WorkerMsg {
    Status(String, Color32),
    Busy(bool),
    Progress(f64),
    InputFilesInfo(String),
    OutputFileName(String),
    HeaderCheck(String),
    HeaderInfo(String),
    Timing {
        start: String,
        stop: String,
        duration: String,
    },
    DataCount(usize),
    LayersReady(Vec<LayerPlot>),
    TableRows(Vec<FluxRow>),
    Error(String),
}

pub struct FluxMode {
    selected_files: Vec<PathBuf>,
    input_files_info: String,
    output_file_name: String,

    status_message: String,
    is_busy: bool,
    progress_value: f64,
    header_check_status: String,
    header_info: String,
    start_time_text: String,
    stop_time_text: String,
    duration_text: String,
    data_count: usize,
    time_range_min: f64,
    time_range_max: f64,
    is_log_scale: bool,
    /// Free-text test-condition metadata (not applied to any filtering
    /// logic - the original only ever echoes these into the header info
    /// readout, see `FluxViewModel.DataProcessing.cs`'s `ProcessHeaderInternal`).
    delay_time: i32,
    threshold: i32,

    layers: Vec<LayerPlot>,
    accumulator: FluxAccumulator,

    active_tab: FluxTab,
    /// Data Table tab
    rows: Vec<FluxRow>,
    /// Pretty-printed JSON of the first rows, for the JSON tab's preview.
    json_preview: String,

    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
}

impl Default for FluxMode {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let layers = (0..LAYER_COUNT)
            .map(|i| LayerPlot {
                name: format!("L{}", i + 1),
                stats_text: "No Data".to_string(),
                ..Default::default()
            })
            .collect();

        Self {
            selected_files: Vec::new(),
            input_files_info: "No files selected".to_string(),
            output_file_name: "FluxResult".to_string(),
            status_message: "Ready".to_string(),
            is_busy: false,
            progress_value: 0.0,
            header_check_status: String::new(),
            header_info: String::new(),
            start_time_text: "-".to_string(),
            stop_time_text: "-".to_string(),
            duration_text: "-".to_string(),
            data_count: 0,
            time_range_min: 0.0,
            time_range_max: 1000.0,
            is_log_scale: false,
            delay_time: 50,
            threshold: 50,
            layers,
            accumulator: FluxAccumulator::default(),
            active_tab: FluxTab::Plots,
            rows: Vec::new(),
            json_preview: String::new(),
            tx,
            rx,
        }
    }
}

impl FluxMode {
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        export: &mut crate::plot_export::PlotExportQueue,
    ) {
        self.drain_messages();

        egui::TopBottomPanel::top("flux_top").show(ctx, |ui| {
            ui.heading("Flux Mode");
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
                    self.header_check();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Output File Name:");
                ui.text_edit_singleline(&mut self.output_file_name);
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.is_busy && !self.selected_files.is_empty(),
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
            ui.checkbox(&mut self.is_log_scale, "Log scale");
            ui.horizontal(|ui| {
                ui.label("Time min");
                ui.add(egui::DragValue::new(&mut self.time_range_min));
                ui.label("Time max");
                ui.add(egui::DragValue::new(&mut self.time_range_max));
            });

            ui.separator();
            ui.label("Test Conditions");
            ui.horizontal(|ui| {
                ui.label("Delay Time");
                ui.add(egui::DragValue::new(&mut self.delay_time));
                ui.label("Threshold");
                ui.add(egui::DragValue::new(&mut self.threshold));
            });

            ui.separator();
            ui.colored_label(Color32::LIGHT_GRAY, &self.status_message);
            if self.is_busy {
                ui.add(
                    egui::ProgressBar::new((self.progress_value / 100.0) as f32).show_percentage(),
                );
            }
            ui.label(format!("Data count: {}", self.data_count));
            ui.label(format!(
                "Start: {}  Stop: {}  Duration: {}",
                self.start_time_text, self.stop_time_text, self.duration_text
            ));
            if !self.header_check_status.is_empty() {
                ui.label(&self.header_check_status);
            }
            if !self.header_info.is_empty() {
                ui.collapsing("Header Info", |ui| ui.monospace(&self.header_info));
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active_tab, FluxTab::Plots, "Graph Visualization");
                ui.selectable_value(&mut self.active_tab, FluxTab::DataTable, "Data Table");
                ui.selectable_value(&mut self.active_tab, FluxTab::Json, "JSON");
            });
            ui.separator();

            match self.active_tab {
                FluxTab::Plots => {
                    egui::ScrollArea::vertical().id_salt("flux_plots_scroll").show(ui, |ui| {
                        for layer in &self.layers {
                            let header = ui.label(format!("{} - {}", layer.name, layer.stats_text));
                            let id_source = format!("flux_plot_{}", layer.name);
                            let plot = Plot::new(id_source.clone())
                                .height(180.0)
                                .legend(Legend::default())
                                .custom_x_axes(vec![crate::plot_export::x_axis("Time (s)")])
                                .custom_y_axes(vec![crate::plot_export::y_axis("Flux (particles/m\u{00b2}/s)")]);
                            crate::plot_export::show(
                                ui,
                                export,
                                &id_source,
                                &layer.name,
                                180.0,
                                Some(header.rect),
                                plot,
                                |plot_ui| {
                                    if !layer.x_data.is_empty() {
                                        let points: PlotPoints = layer
                                            .x_data
                                            .iter()
                                            .zip(layer.y_data.iter())
                                            .map(|(&x, &y)| {
                                                let yy = if self.is_log_scale {
                                                    if y > 0.0 {
                                                        y.log10()
                                                    } else {
                                                        -10.0
                                                    }
                                                } else {
                                                    y
                                                };
                                                [x, yy]
                                            })
                                            .collect();
                                        plot_ui.line(
                                            Line::new(points)
                                                .name(&layer.name)
                                                .color(Color32::LIGHT_BLUE),
                                        );
                                    }
                                },
                            );
                            ui.separator();
                        }
                    });
                }
                FluxTab::DataTable => self.data_table_ui(ui),
                FluxTab::Json => self.json_tab_ui(ui),
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
                WorkerMsg::Progress(p) => self.progress_value = p,
                WorkerMsg::InputFilesInfo(s) => self.input_files_info = s,
                WorkerMsg::OutputFileName(s) => self.output_file_name = s,
                WorkerMsg::HeaderCheck(s) => self.header_check_status = s,
                WorkerMsg::HeaderInfo(s) => self.header_info = s,
                WorkerMsg::Timing {
                    start,
                    stop,
                    duration,
                } => {
                    self.start_time_text = start;
                    self.stop_time_text = stop;
                    self.duration_text = duration;
                }
                WorkerMsg::DataCount(c) => self.data_count = c,
                WorkerMsg::LayersReady(layers) => self.layers = layers,
                WorkerMsg::TableRows(rows) => {
                    self.json_preview = crate::json_view::preview_string(&flux_rows_json(&rows));
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
        self.selected_files.clear();
        self.input_files_info = "No files selected".to_string();
        self.header_check_status.clear();
        self.start_time_text = "-".to_string();
        self.stop_time_text = "-".to_string();
        self.duration_text = "-".to_string();
        self.data_count = 0;
        self.header_info.clear();
        self.accumulator = FluxAccumulator::default();
        self.rows.clear();
        self.json_preview.clear();
        for (i, layer) in self.layers.iter_mut().enumerate() {
            *layer = LayerPlot {
                name: format!("L{}", i + 1),
                stats_text: "No Data".to_string(),
                ..Default::default()
            };
        }
        self.progress_value = 0.0;
        self.status_message = "Reset complete.".to_string();
    }

    fn select_files(&mut self) {
        self.reset();
        let Some(files) = rfd::FileDialog::new()
            .add_filter("Text Files", &["txt"])
            .pick_files()
        else {
            return;
        };

        if files.len() == 1 {
            self.output_file_name = files[0]
                .file_stem()
                .map(|s| format!("{}.xlsx", s.to_string_lossy()))
                .unwrap_or_else(|| "FluxResult.xlsx".to_string());
            self.input_files_info = "1 file selected.".to_string();
            self.selected_files = files;
            self.status_message = "Files loaded. Ready to process.".to_string();
        } else {
            self.is_busy = true;
            self.status_message = format!("Combining {} files...", files.len());
            let tx = self.tx.clone();
            std::thread::spawn(move || combine_files_worker(files, tx));
        }
    }

    fn process_data(&mut self) {
        self.is_busy = true;
        self.status_message = "Processing raw data...".to_string();
        let files = self.selected_files.clone();
        let output_name = self.output_file_name.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || process_data_worker(files, output_name, tx));
    }

    fn read_data(&mut self) {
        if self.selected_files.is_empty() {
            self.status_message = "Select raw .txt files first.".to_string();
            return;
        }
        self.is_busy = true;
        self.progress_value = 0.0;
        self.accumulator = FluxAccumulator::default();
        self.rows.clear();
        self.json_preview.clear();

        let files = self.selected_files.clone();
        let delay_time = self.delay_time;
        let threshold = self.threshold;
        let tx = self.tx.clone();
        std::thread::spawn(move || read_data_worker(files, delay_time, threshold, tx));
    }

    fn header_check(&mut self) {
        self.is_busy = true;
        self.header_check_status = "Checking...".to_string();
        let output_name = self.output_file_name.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || header_check_worker(output_name, tx));
    }

    /// Data Table tab: one row per raw line, decoded per `flux.md`.
    fn data_table_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.rows.is_empty(), egui::Button::new("Export Table as CSV...")).clicked() {
                self.export_table_csv();
            }
            ui.label(format!("{} row(s)", self.rows.len()));
        });
        ui.separator();

        let headers = flux_table_headers();
        let widths: Vec<f32> = headers.iter().map(|h| header_column_width(ui, h)).collect();

        egui::ScrollArea::horizontal().id_salt("flux_data_table_hscroll").show(ui, |ui| {
            let row_height = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;

            egui::Grid::new("flux_data_table_header").show(ui, |ui| {
                for (header, &width) in headers.iter().zip(&widths) {
                    ui.add_sized([width, row_height], egui::Label::new(egui::RichText::new(header).strong()).truncate());
                }
                ui.end_row();
            });

            egui::ScrollArea::vertical().id_salt("flux_data_table_vscroll").show_rows(ui, row_height, self.rows.len(), |ui, visible_range| {
                egui::Grid::new("flux_data_table_body").striped(true).start_row(visible_range.start).show(ui, |ui| {
                    for row in &self.rows[visible_range] {
                        let cells = flux_row_cells(row);
                        for (cell, &width) in cells.iter().zip(&widths) {
                            ui.add_sized([width, row_height], egui::Label::new(cell).truncate());
                        }
                        ui.end_row();
                    }
                });
            });
        });
    }

    /// Export the data table as CSV. The Particle Info cell is comma-joined
    /// (1000 values), so it's quoted per `csv_field`.
    fn export_table_csv(&mut self) {
        if self.rows.is_empty() {
            self.status_message = "No rows to export - run Read Data first.".to_string();
            return;
        }
        let Some(path) = rfd::FileDialog::new().set_file_name("flux_table.csv").add_filter("CSV", &["csv"]).save_file() else {
            return;
        };

        let mut csv = flux_table_headers().iter().map(|h| csv_field(h)).collect::<Vec<_>>().join(",");
        csv.push('\n');
        for row in &self.rows {
            csv.push_str(&flux_row_cells(row).iter().map(|c| csv_field(c)).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }

        match std::fs::write(&path, csv) {
            Ok(()) => self.status_message = format!("Exported {} row(s) to {}", self.rows.len(), path.display()),
            Err(e) => self.status_message = format!("Failed to export CSV: {e}"),
        }
    }

    /// JSON tab: the decoded rows as JSON withpreview and downloader
    fn json_tab_ui(&mut self, ui: &mut egui::Ui) {
        if crate::json_view::json_tab_ui(ui, self.rows.len(), &self.json_preview) {
            match crate::json_view::save_json("flux_data.json", &flux_rows_json(&self.rows)) {
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

fn combine_files_worker(files: Vec<PathBuf>, tx: Sender<WorkerMsg>) {
    match io::file_helper::combine_files(&files, "multiple_file_output.txt") {
        Ok(path) => {
            let _ = tx.send(WorkerMsg::InputFilesInfo(format!(
                "{} files combined.",
                files.len()
            )));
            let _ = tx.send(WorkerMsg::OutputFileName(
                "multiple_file_output.xlsx".to_string(),
            ));
            let _ = tx.send(WorkerMsg::Status(
                format!("Files combined into {}", path.display()),
                Color32::from_rgb(50, 200, 50),
            ));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(e));
        }
    }
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn process_data_worker(files: Vec<PathBuf>, output_name: String, tx: Sender<WorkerMsg>) {
    match io::segment_filter::filter_e225_segments_from_files(
        &files,
        AppConstants::SEGMENT_HEX_LENGTH,
    ) {
        Ok(segments) if !segments.is_empty() => {
            let _ = tx.send(WorkerMsg::Status(
                format!("Saving {} segments to Excel...", segments.len()),
                Color32::GRAY,
            ));
            match io::file_helper::resolve_excel_save_path(&output_name, "Flux") {
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
/// filters E225 segments, then decodes each into the accumulator + a `FluxRow`
/// that feeds the Data Table and JSON tabs.
fn read_data_worker(files: Vec<PathBuf>, delay_time: i32, threshold: i32, tx: Sender<WorkerMsg>) {
    let rows = match io::segment_filter::filter_e225_segments_from_files(
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

    if rows.is_empty() {
        let _ = tx.send(WorkerMsg::Status(
            "No valid E225 segments found.".to_string(),
            Color32::RED,
        ));
        let _ = tx.send(WorkerMsg::Busy(false));
        return;
    }

    let total_steps = rows.len();
    let _ = tx.send(WorkerMsg::DataCount(total_steps));

    let mut accumulator = FluxAccumulator::default();
    let mut start_time = None;
    let mut last_hex: Option<&String> = None;
    let mut table_rows: Vec<FluxRow> = Vec::with_capacity(total_steps);

    for (i, hex_string) in rows.iter().enumerate() {
        if i == 0 {
            let dt = baseline_core::flux::processing::get_date_time_from_hex(hex_string);
            start_time = Some(dt);
            let _ = tx.send(WorkerMsg::Timing {
                start: dt.format("%Y-%b-%d %H:%M:%S%.3f").to_string(),
                stop: "-".to_string(),
                duration: "-".to_string(),
            });
        }
        accumulator.process_flux_observation(hex_string);
        let hex_data = split_hex_data(hex_string);
        if let Some(line) = parse_flux_line(&hex_data) {
            table_rows.push(line.into());
        }
        last_hex = Some(hex_string);

        if i % 500 == 0 {
            let progress = (i as f64 / total_steps.max(1) as f64) * 100.0;
            let _ = tx.send(WorkerMsg::Progress(progress));
            let _ = tx.send(WorkerMsg::Status(
                format!("Processing... {progress:.1}% ({i}/{total_steps})"),
                Color32::GRAY,
            ));
        }
    }

    let stop_time = last_hex.map(|h| baseline_core::flux::processing::get_date_time_from_hex(h));

    if let (Some(start), Some(stop)) = (start_time, stop_time) {
        let duration = stop.signed_duration_since(start);
        let _ = tx.send(WorkerMsg::Timing {
            start: start.format("%Y-%b-%d %H:%M:%S%.3f").to_string(),
            stop: stop.format("%Y-%b-%d %H:%M:%S%.3f").to_string(),
            duration: format!("{:.3} seconds", duration.num_milliseconds() as f64 / 1000.0),
        });
    }

    if let Some(hex) = last_hex {
        let hex_data = baseline_core::observation::data_processor::split_hex_data(hex);
        if let Some(header) = baseline_core::flux::processing::process_header_internal(&hex_data) {
            let info = format!(
                "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\nTest condition:\nDelay Time: {}\nThreshold: {}",
                header.packet_sync,
                header.package_id,
                header.packet_seq,
                header.packet_data_len,
                header.timestamp,
                header.data_type,
                header.check_sum_hex,
                if header.checksum_matches {
                    "Checksum matches!"
                } else {
                    "Checksum does not match."
                },
                delay_time,
                threshold
            );
            let _ = tx.send(WorkerMsg::HeaderInfo(info));
        }
    }

    let _ = tx.send(WorkerMsg::TableRows(table_rows));

    if let Some(layers) = accumulator.calculate_flux(LAYER_COUNT, DETECTOR_AREA_M2) {
        let layer_plots: Vec<LayerPlot> = layers
            .into_iter()
            .enumerate()
            .map(|(i, l)| LayerPlot {
                name: format!("L{}", i + 1),
                stats_text: l.stats_text,
                x_data: l.x_data.unwrap_or_default(),
                y_data: l.y_data.unwrap_or_default(),
            })
            .collect();
        let _ = tx.send(WorkerMsg::LayersReady(layer_plots));
    }

    let _ = tx.send(WorkerMsg::Status(
        "Process Complete".to_string(),
        Color32::from_rgb(50, 200, 50),
    ));
    let _ = tx.send(WorkerMsg::Progress(100.0));
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn header_check_worker(output_name: String, tx: Sender<WorkerMsg>) {
    let Some(file_path) = io::file_helper::find_excel_file(&output_name, "Flux") else {
        let _ = tx.send(WorkerMsg::Error(format!(
            "File not found: {output_name}.xlsx (searched Documents/DSSD_Analysis/Flux and legacy locations)"
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

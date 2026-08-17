//! Port of Flux mode (`FluxViewModel` + `.Commands.cs` + `.FileOperations.cs`
//! + `.DataProcessing.cs` + `FluxLayerViewModel`).

use baseline_core::flux::FluxAccumulator;
use baseline_core::io;
use baseline_core::models::shared::AppConstants;
use egui::Color32;
use egui_plot::{Legend, Line, Plot, PlotPoints};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};


const LAYER_COUNT: usize = 7;
/// Detector area in m^2 (32mm x 32mm).
const DETECTOR_AREA_M2: f64 = 32.0 * 32.0 * 1e-6;

#[derive(Debug, Clone, Default)]
struct LayerPlot {
    name: String,
    stats_text: String,
    x_data: Vec<f64>,
    y_data: Vec<f64>,
}

enum WorkerMsg {
    Status(String, Color32),
    Busy(bool),
    Progress(f64),
    InputFilesInfo(String),
    OutputFileName(String),
    HeaderCheck(String),
    HeaderInfo(String),
    Timing { start: String, stop: String, duration: String },
    DataCount(usize),
    LayersReady(Vec<LayerPlot>),
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

    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
}

impl Default for FluxMode {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let layers = (0..LAYER_COUNT)
            .map(|i| LayerPlot { name: format!("L{}", i + 1), stats_text: "No Data".to_string(), ..Default::default() })
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
            tx,
            rx,
        }
    }
}

impl FluxMode {
    pub fn update(&mut self, ctx: &egui::Context, export: &mut crate::plot_export::PlotExportQueue) {
        self.drain_messages();

        egui::TopBottomPanel::top("flux_top").show(ctx, |ui| {
            ui.heading("Flux Mode");
            ui.separator();
            ui.label(&self.input_files_info);
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.is_busy, egui::Button::new("Select Files...")).clicked() {
                    self.select_files();
                }
                if ui.add_enabled(!self.is_busy, egui::Button::new("Header Check")).clicked() {
                    self.header_check();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Output name:");
                ui.text_edit_singleline(&mut self.output_file_name);
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.is_busy && !self.selected_files.is_empty(), egui::Button::new("Process to Excel")).clicked() {
                    self.process_data();
                }
                if ui.add_enabled(!self.is_busy, egui::Button::new("Read Data")).clicked() {
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
                ui.add(egui::ProgressBar::new((self.progress_value / 100.0) as f32).show_percentage());
            }
            ui.label(format!("Data count: {}", self.data_count));
            ui.label(format!("Start: {}  Stop: {}  Duration: {}", self.start_time_text, self.stop_time_text, self.duration_text));
            if !self.header_check_status.is_empty() {
                ui.label(&self.header_check_status);
            }
            if !self.header_info.is_empty() {
                ui.collapsing("Header Info", |ui| ui.monospace(&self.header_info));
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for layer in &self.layers {
                    let header = ui.label(format!("{} - {}", layer.name, layer.stats_text));
                    let id_source = format!("flux_plot_{}", layer.name);
                    let plot = Plot::new(id_source.clone())
                        .height(180.0)
                        .legend(Legend::default())
                        .custom_x_axes(vec![crate::plot_export::x_axis("Time (s)")])
                        .custom_y_axes(vec![crate::plot_export::y_axis("Flux (particles/m\u{00b2}/s)")]);
                    crate::plot_export::show(ui, export, &id_source, &layer.name, 180.0, Some(header.rect), plot, |plot_ui| {
                        if !layer.x_data.is_empty() {
                            let points: PlotPoints = layer
                                .x_data
                                .iter()
                                .zip(layer.y_data.iter())
                                .map(|(&x, &y)| {
                                    let yy = if self.is_log_scale { if y > 0.0 { y.log10() } else { -10.0 } } else { y };
                                    [x, yy]
                                })
                                .collect();
                            plot_ui.line(Line::new(points).name(&layer.name).color(Color32::LIGHT_BLUE));
                        }
                    });
                    ui.separator();
                }
            });
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
                WorkerMsg::Timing { start, stop, duration } => {
                    self.start_time_text = start;
                    self.stop_time_text = stop;
                    self.duration_text = duration;
                }
                WorkerMsg::DataCount(c) => self.data_count = c,
                WorkerMsg::LayersReady(layers) => self.layers = layers,
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
        for (i, layer) in self.layers.iter_mut().enumerate() {
            *layer = LayerPlot { name: format!("L{}", i + 1), stats_text: "No Data".to_string(), ..Default::default() };
        }
        self.progress_value = 0.0;
        self.status_message = "Reset complete.".to_string();
    }

    fn select_files(&mut self) {
        self.reset();
        let Some(files) = rfd::FileDialog::new().add_filter("Text Files", &["txt"]).pick_files() else {
            return;
        };

        if files.len() == 1 {
            self.output_file_name = files[0].file_stem().map(|s| format!("{}.xlsx", s.to_string_lossy())).unwrap_or_else(|| "FluxResult.xlsx".to_string());
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
        self.is_busy = true;
        self.progress_value = 0.0;
        self.accumulator = FluxAccumulator::default();

        let output_name = self.output_file_name.clone();
        let delay_time = self.delay_time;
        let threshold = self.threshold;
        let tx = self.tx.clone();
        std::thread::spawn(move || read_data_worker(output_name, delay_time, threshold, tx));
    }

    fn header_check(&mut self) {
        self.is_busy = true;
        self.header_check_status = "Checking...".to_string();
        let output_name = self.output_file_name.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || header_check_worker(output_name, tx));
    }
}

fn combine_files_worker(files: Vec<PathBuf>, tx: Sender<WorkerMsg>) {
    match io::file_helper::combine_files(&files, "multiple_file_output.txt") {
        Ok(path) => {
            let _ = tx.send(WorkerMsg::InputFilesInfo(format!("{} files combined.", files.len())));
            let _ = tx.send(WorkerMsg::OutputFileName("multiple_file_output.xlsx".to_string()));
            let _ = tx.send(WorkerMsg::Status(format!("Files combined into {}", path.display()), Color32::from_rgb(50, 200, 50)));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(e));
        }
    }
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn process_data_worker(files: Vec<PathBuf>, output_name: String, tx: Sender<WorkerMsg>) {
    match io::segment_filter::filter_e225_segments_from_files(&files, AppConstants::SEGMENT_HEX_LENGTH) {
        Ok(segments) if !segments.is_empty() => {
            let _ = tx.send(WorkerMsg::Status(format!("Saving {} segments to Excel...", segments.len()), Color32::GRAY));
            match io::file_helper::resolve_excel_save_path(&output_name, "") {
                Ok(path) => match io::excel::save_lines_to_excel(&segments, &path) {
                    Ok(()) => {
                        let _ = tx.send(WorkerMsg::Status("Processing complete.".to_string(), Color32::from_rgb(50, 200, 50)));
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
            let _ = tx.send(WorkerMsg::Status("No valid segments found.".to_string(), Color32::RED));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Error: {e}")));
        }
    }
    let _ = tx.send(WorkerMsg::Progress(100.0));
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn read_data_worker(output_name: String, delay_time: i32, threshold: i32, tx: Sender<WorkerMsg>) {
    let Some(file_path) = io::file_helper::find_excel_file(&output_name) else {
        let _ = tx.send(WorkerMsg::Error(format!("File not found: {output_name}.xlsx")));
        let _ = tx.send(WorkerMsg::Busy(false));
        return;
    };

    let rows = match io::excel::read_excel_column_a(&file_path) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Error: {e}")));
            let _ = tx.send(WorkerMsg::Busy(false));
            return;
        }
    };
    let total_steps = rows.len();
    let _ = tx.send(WorkerMsg::DataCount(total_steps));

    let mut accumulator = FluxAccumulator::default();
    let mut start_time = None;
    let mut last_hex: Option<&String> = None;

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
        last_hex = Some(hex_string);

        if i % 500 == 0 {
            let progress = (i as f64 / total_steps.max(1) as f64) * 100.0;
            let _ = tx.send(WorkerMsg::Progress(progress));
            let _ = tx.send(WorkerMsg::Status(format!("Processing... {progress:.1}% ({i}/{total_steps})"), Color32::GRAY));
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
                if header.checksum_matches { "Checksum matches!" } else { "Checksum does not match." },
                delay_time,
                threshold
            );
            let _ = tx.send(WorkerMsg::HeaderInfo(info));
        }
    }

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

    let _ = tx.send(WorkerMsg::Status("Process Complete".to_string(), Color32::from_rgb(50, 200, 50)));
    let _ = tx.send(WorkerMsg::Progress(100.0));
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn header_check_worker(output_name: String, tx: Sender<WorkerMsg>) {
    let Some(file_path) = io::file_helper::find_excel_file(&output_name) else {
        let _ = tx.send(WorkerMsg::Error("File not found!".to_string()));
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

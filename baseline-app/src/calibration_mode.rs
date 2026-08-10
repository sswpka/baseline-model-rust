//! Port of Calibration mode (`CalibrationViewModel` + `.Commands.cs` +
//! `.DataProcessing.cs` + `.Plotting.cs`). Deferred (see project notes):
//! the per-channel zoom window (`CalibrationDetailWindow`/`OpenZoomWindow`).

use baseline_core::calibration::CalibrationAccumulator;
use baseline_core::io;
use baseline_core::observation::data_processor::{split_hex_data, validate_header};
use egui::Color32;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use crate::channel::{channel_block_ui, ChannelState};

enum WorkerMsg {
    Status(String, Color32),
    Busy(bool),
    HeaderCheck(String),
    Progress(f64),
    DataLoaded(CalibrationAccumulator),
    Error(String),
}

pub struct CalibrationMode {
    input_files: Vec<PathBuf>,
    input_files_info: String,
    output_file_name: String,
    read_multiple_files: bool,

    selected_layer_index: usize, // 0=L1,1=L2,2=L6,3=L7
    selected_x_axis_index: usize, // 0=ADC,1=Voltage
    x_axis_min: f64,
    x_axis_max: f64,
    delay_time: i32,
    threshold: i32,

    status_message: String,
    is_busy: bool,
    progress_value: f64,
    header_check_status: String,

    accumulator: CalibrationAccumulator,
    channels: Vec<ChannelState>,

    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
}

impl Default for CalibrationMode {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let channels = (0..16)
            .map(|i| {
                let name = if i < 8 { format!("X{}", i + 1) } else { format!("Z{}", i - 7) };
                ChannelState { channel_index: i, title: name, stats_text: String::new(), ..Default::default() }
            })
            .collect();

        Self {
            input_files: Vec::new(),
            input_files_info: "No files selected".to_string(),
            output_file_name: "CalibrationResult".to_string(),
            read_multiple_files: false,
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
            accumulator: CalibrationAccumulator::default(),
            channels,
            tx,
            rx,
        }
    }
}

impl CalibrationMode {
    pub fn update(&mut self, ctx: &egui::Context, export: &mut crate::plot_export::PlotExportQueue) {
        self.drain_messages();

        egui::TopBottomPanel::top("calibration_top").show(ctx, |ui| {
            ui.heading("Calibration Mode");
            ui.separator();
            ui.label(&self.input_files_info);
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.is_busy, egui::Button::new("Select Raw Files...")).clicked() {
                    self.select_files();
                }
                if ui.add_enabled(!self.is_busy, egui::Button::new("Select Excel Files...")).clicked() {
                    self.select_excel_files();
                }
            });

            ui.horizontal(|ui| {
                ui.label("Output name:");
                ui.text_edit_singleline(&mut self.output_file_name);
            });

            ui.horizontal(|ui| {
                if ui.add_enabled(!self.is_busy && !self.input_files.is_empty(), egui::Button::new("Process to Excel")).clicked() {
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
            egui::ComboBox::from_label("Layer")
                .selected_text(["L1", "L2", "L6", "L7"][self.selected_layer_index])
                .show_ui(ui, |ui| {
                    for (i, name) in ["L1", "L2", "L6", "L7"].iter().enumerate() {
                        if ui.selectable_value(&mut self.selected_layer_index, i, *name).changed() {
                            self.update_plots();
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
                self.update_plots();
            }
            ui.horizontal(|ui| {
                ui.label("X Min");
                if ui.add(egui::DragValue::new(&mut self.x_axis_min)).changed() {
                    self.update_plots();
                }
                ui.label("X Max");
                if ui.add(egui::DragValue::new(&mut self.x_axis_max)).changed() {
                    self.update_plots();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Delay (ms)");
                ui.add(egui::DragValue::new(&mut self.delay_time));
                ui.label("Threshold");
                ui.add(egui::DragValue::new(&mut self.threshold));
            });

            ui.separator();
            ui.colored_label(Color32::LIGHT_GRAY, &self.status_message);
            if self.is_busy {
                ui.add(egui::ProgressBar::new((self.progress_value / 100.0) as f32).show_percentage());
            }
            if !self.header_check_status.is_empty() {
                ui.label(&self.header_check_status);
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let (x_channels, z_channels) = self.channels.split_at_mut(8);
                ui.columns(2, |columns| {
                    channel_block_ui(&mut columns[0], "X-direction (X1-X8)", x_channels, export);
                    channel_block_ui(&mut columns[1], "Z-direction (Z1-Z8)", z_channels, export);
                });
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
                WorkerMsg::HeaderCheck(s) => self.header_check_status = s,
                WorkerMsg::Progress(p) => self.progress_value = p,
                WorkerMsg::DataLoaded(acc) => {
                    self.accumulator = acc;
                    self.update_plots();
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
        self.read_multiple_files = false;
        self.accumulator = CalibrationAccumulator::default();
        for ch in &mut self.channels {
            ch.counts.clear();
            ch.bin_centers.clear();
            ch.stats_text.clear();
        }
        self.status_message = "Reset complete.".to_string();
        self.progress_value = 0.0;
        self.header_check_status.clear();
    }

    fn select_files(&mut self) {
        if let Some(files) = rfd::FileDialog::new().add_filter("Text files", &["txt"]).pick_files() {
            self.input_files_info = format!("{} txt file(s) selected", files.len());
            self.input_files = files;
            self.read_multiple_files = false;
            self.status_message = "Files selected.".to_string();
        }
    }

    fn select_excel_files(&mut self) {
        if let Some(files) = rfd::FileDialog::new().add_filter("Excel files", &["xlsx"]).set_title("Select Excel files to read").pick_files() {
            self.input_files_info = format!("{} Excel file(s) selected for reading", files.len());
            self.input_files = files;
            self.read_multiple_files = true;
            self.status_message = "Excel files selected.".to_string();
        }
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
        self.is_busy = true;
        self.progress_value = 0.0;
        self.accumulator = CalibrationAccumulator::default();

        let files_to_read: Vec<PathBuf> = if self.read_multiple_files && !self.input_files.is_empty() {
            self.input_files
                .iter()
                .filter(|f| f.extension().map(|e| e.eq_ignore_ascii_case("xlsx")).unwrap_or(false) && f.exists())
                .cloned()
                .collect()
        } else {
            match io::file_helper::find_excel_file(&self.output_file_name) {
                Some(f) => vec![f],
                None => {
                    self.status_message = format!("File not found: {}.xlsx", self.output_file_name);
                    self.is_busy = false;
                    return;
                }
            }
        };

        if files_to_read.is_empty() {
            self.status_message = "No valid Excel files found in selection.".to_string();
            self.is_busy = false;
            return;
        }

        let tx = self.tx.clone();
        std::thread::spawn(move || read_data_worker(files_to_read, tx));
    }

    fn update_plots(&mut self) {
        let source = if self.selected_x_axis_index == 1 {
            self.accumulator.voltage_columns(self.selected_layer_index)
        } else {
            self.accumulator.columns(self.selected_layer_index)
        };

        for ch in 0..16 {
            let data_for_channel: Vec<f64> = source[ch].iter().copied().filter(|&d| d > 0.0).collect();
            if data_for_channel.is_empty() {
                continue;
            }
            let (counts, bin_centers) =
                baseline_core::baseline_processing::build_histogram_avg_centers(&data_for_channel, self.x_axis_min, self.x_axis_max, 500);
            self.channels[ch].counts = counts;
            self.channels[ch].bin_centers = bin_centers;
            self.channels[ch].stats_text = format!("Counts: {}", data_for_channel.len());
        }
    }
}

fn process_data_worker(files: Vec<PathBuf>, output_name: String, tx: Sender<WorkerMsg>) {
    match io::segment_filter::filter_e225_segments_from_files(&files, baseline_core::models::shared::AppConstants::SEGMENT_HEX_LENGTH) {
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

fn read_data_worker(files: Vec<PathBuf>, tx: Sender<WorkerMsg>) {
    let mut accumulator = CalibrationAccumulator::default();
    accumulator.reset(1_000_000);

    let mut header_ok = true;
    let file_count = files.len();

    for (file_index, file) in files.iter().enumerate() {
        let rows = match io::excel::read_excel_column_a(file) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(WorkerMsg::Error(format!("Error: {e}")));
                let _ = tx.send(WorkerMsg::Busy(false));
                return;
            }
        };
        let total_rows = rows.len();

        for (row_index, hex_string) in rows.iter().enumerate() {
            let hex_data = split_hex_data(hex_string);

            if file_index == 0 && row_index == 0 {
                let valid = validate_header(&hex_data);
                let _ = tx.send(WorkerMsg::HeaderCheck(if valid { "Checksum OK".to_string() } else { "Checksum Mismatch".to_string() }));
                if !valid {
                    header_ok = false;
                    break;
                }
            }

            accumulator.process_calibration(&hex_data);

            if row_index % 1000 == 0 {
                let progress = (row_index as f64 / total_rows.max(1) as f64) * 100.0;
                let _ = tx.send(WorkerMsg::Progress(progress));
                let _ = tx.send(WorkerMsg::Status(
                    format!("File {}/{}: {}/{} rows", file_index + 1, file_count, row_index, total_rows),
                    Color32::GRAY,
                ));
            }
        }

        if !header_ok {
            break;
        }
    }

    if !header_ok {
        let _ = tx.send(WorkerMsg::Status("Stopped: Checksum Mismatch".to_string(), Color32::RED));
        let _ = tx.send(WorkerMsg::Busy(false));
        return;
    }

    // Hand the fully populated accumulator back to the UI thread, which
    // recomputes histograms for the currently-selected layer/axis (and again
    // on every later layer/axis change) via `update_plots`.
    let _ = tx.send(WorkerMsg::DataLoaded(accumulator));

    let _ = tx.send(WorkerMsg::Status("Complete!".to_string(), Color32::from_rgb(50, 200, 50)));
    let _ = tx.send(WorkerMsg::Progress(100.0));
    let _ = tx.send(WorkerMsg::Busy(false));
}

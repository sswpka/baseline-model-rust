//! Port of Baseline mode: `MainViewModel` (+ `.FileOperations.cs` +
//! `.Processing.cs`) and its `MainWindow`. CommunityToolkit MVVM's
//! `[ObservableProperty]`/`[RelayCommand]` plumbing is collapsed into a
//! plain state struct + methods called directly from `update()`, per the
//! project's porting approach (immediate-mode UI has no use for
//! `INotifyPropertyChanged`).
//!
//! Long-running work (file I/O, Excel, per-channel fits) runs on a
//! background thread and reports back over an `mpsc` channel, standing in
//! for the C#'s `Task.Run` + `Dispatcher.Invoke` pattern.
//!
//! Deferred (not in this pass - see project notes): manual-fit mode with
//! parameter locks and `OptimizeManual`, the data-table/paging grid, the
//! coincidence-matrix heatmap window, and `UseKalmanFilter` (present as a
//! field but not exercised by any ported pipeline - grep confirms it's not
//! read anywhere in `MainViewModel.Processing.cs`).

use baseline_core::io;
use baseline_core::math::MathService;
use baseline_core::models::baseline::BaselineData;
use egui::Color32;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

use crate::channel::{channel_block_ui, ChannelState, FitCurve};

pub enum WorkerMsg {
    Status(String, Color32),
    Progress(f64),
    Busy(bool),
    InputFilesInfo(String),
    OutputFileName(String),
    DataLoaded(Vec<BaselineData>),
    ChannelResult(usize, ChannelState),
    HeaderInfo(String),
    Timing { start: String, stop: String, duration: String },
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaselineMainMode {
    CutOffBaseline,
    BaselineSetting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaselineSubtractMode {
    Before,
    After,
    BeforeLog,
    AfterLog,
}

impl BaselineSubtractMode {
    fn should_subtract(self) -> bool {
        matches!(self, BaselineSubtractMode::After | BaselineSubtractMode::AfterLog)
    }
}

pub struct BaselineMode {
    // File selection / output
    selected_files: Vec<PathBuf>,
    input_files_info: String,
    output_directory_path: PathBuf,
    output_file_name: String,

    // Status
    status_message: String,
    status_color: Color32,
    is_busy: bool,
    progress_value: f64,

    // Controls
    selected_layer_index: usize, // 0=L1,1=L2,2=L6,3=L7
    selected_x_axis_index: usize, // 0=ADC,1=Voltage
    main_mode: BaselineMainMode,
    subtract_mode: BaselineSubtractMode,
    use_thresholding: bool,
    k_factor: f64,
    x_axis_min: f64,
    x_axis_max: f64,
    delay_time_ms: i32,

    show_gaussian_fit: bool,
    show_hemg_single_fit: bool,
    show_hemg_double_fit: bool,
    show_lorentzian_fit: bool,

    processed_data: Vec<BaselineData>,
    data_counts_str: String,
    start_time_str: String,
    stop_time_str: String,
    duration_str: String,
    header_info_text: String,
    can_save_mean: bool,

    channels: Vec<ChannelState>,

    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
}

impl Default for BaselineMode {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let output_directory_path = dirs_next_documents().join("BaselineModeOutputs_1");

        let channels = (0..16)
            .map(|i| ChannelState {
                channel_index: i,
                title: format!("Channel {}", i + 1),
                stats_text: "No Data".to_string(),
                ..Default::default()
            })
            .collect();

        Self {
            selected_files: Vec::new(),
            input_files_info: "No files selected".to_string(),
            output_directory_path,
            output_file_name: "output.txt".to_string(),
            status_message: "Ready".to_string(),
            status_color: Color32::GRAY,
            is_busy: false,
            progress_value: 0.0,
            selected_layer_index: 0,
            selected_x_axis_index: 0,
            main_mode: BaselineMainMode::CutOffBaseline,
            subtract_mode: BaselineSubtractMode::Before,
            use_thresholding: false,
            k_factor: 2.0,
            x_axis_min: 0.0,
            x_axis_max: 16383.0,
            delay_time_ms: 0,
            show_gaussian_fit: true,
            show_hemg_single_fit: false,
            show_hemg_double_fit: false,
            show_lorentzian_fit: false,
            processed_data: Vec::new(),
            data_counts_str: "-".to_string(),
            start_time_str: "-".to_string(),
            stop_time_str: "-".to_string(),
            duration_str: "-".to_string(),
            header_info_text: String::new(),
            can_save_mean: false,
            channels,
            tx,
            rx,
        }
    }
}

fn dirs_next_documents() -> PathBuf {
    // .NET's Environment.SpecialFolder.MyDocuments equivalent.
    std::env::var_os("USERPROFILE")
        .map(|p| PathBuf::from(p).join("Documents"))
        .unwrap_or_else(|| PathBuf::from("."))
}

impl BaselineMode {
    pub fn update(&mut self, ctx: &egui::Context) {
        self.drain_worker_messages();

        egui::TopBottomPanel::top("baseline_top").show(ctx, |ui| {
            ui.heading("Baseline Mode");
            ui.separator();
            ui.columns(4, |cols| {
                self.file_and_actions_ui(&mut cols[0]);
                cols[1].separator();
                self.parameters_and_fitting_ui(&mut cols[1]);
                cols[2].separator();
                self.tools_ui(&mut cols[2]);
                cols[3].separator();
                self.dssd_range_view_ui(&mut cols[3]);
            });
            ui.separator();
            self.status_ui(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let (x_channels, z_channels) = self.channels.split_at_mut(8);
                ui.columns(2, |columns| {
                    channel_block_ui(&mut columns[0], "X-direction (CH 1-8)", x_channels);
                    channel_block_ui(&mut columns[1], "Z-direction (CH 9-16)", z_channels);
                });
            });
        });

        if self.is_busy {
            ctx.request_repaint();
        }
    }

    fn drain_worker_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::Status(s, c) => {
                    self.status_message = s;
                    self.status_color = c;
                }
                WorkerMsg::Progress(p) => self.progress_value = p,
                WorkerMsg::Busy(b) => self.is_busy = b,
                WorkerMsg::InputFilesInfo(s) => self.input_files_info = s,
                WorkerMsg::OutputFileName(s) => self.output_file_name = s,
                WorkerMsg::DataLoaded(data) => {
                    self.data_counts_str = data.len().to_string();
                    self.processed_data = data;
                }
                WorkerMsg::ChannelResult(idx, state) => {
                    if idx < self.channels.len() {
                        self.channels[idx] = state;
                    }
                }
                WorkerMsg::HeaderInfo(s) => self.header_info_text = s,
                WorkerMsg::Timing { start, stop, duration } => {
                    self.start_time_str = start;
                    self.stop_time_str = stop;
                    self.duration_str = duration;
                    self.can_save_mean = true;
                }
                WorkerMsg::Error(e) => {
                    self.status_message = e;
                    self.status_color = Color32::RED;
                    self.is_busy = false;
                }
            }
        }
    }

    /// Column 1: file open/export/header-check, plus the pre-process/process/
    /// stop/reset action buttons.
    fn file_and_actions_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("File & Actions");
        ui.label(&self.input_files_info);

        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(!self.is_busy, egui::Button::new("Select Files...")).clicked() {
                self.select_files();
            }
            if ui.add_enabled(!self.is_busy, egui::Button::new("Check Header")).clicked() {
                self.check_header();
            }
        });

        ui.label("Output dir:");
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(self.output_directory_path.display().to_string()).monospace()).wrap());
            if ui.button("Browse...").clicked() {
                if let Some(dir) = rfd::FileDialog::new().set_title("Select Output Root Folder").pick_folder() {
                    self.output_directory_path = dir;
                }
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label("Output file:");
            ui.text_edit_singleline(&mut self.output_file_name);
        });

        if !self.header_info_text.is_empty() {
            ui.collapsing("Header Info", |ui| {
                ui.add(egui::Label::new(egui::RichText::new(&self.header_info_text).monospace()).wrap());
            });
        }

        ui.separator();

        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(!self.is_busy && !self.selected_files.is_empty(), egui::Button::new("Pre-Process to Excel")).clicked() {
                self.pre_process_data();
            }
            if ui.add_enabled(self.is_busy, egui::Button::new("Stop")).clicked() {
                self.status_message = "Stopping is not yet wired up for background threads in this port.".to_string();
            }
            if ui.add_enabled(!self.is_busy, egui::Button::new("Process Data")).clicked() {
                self.process_data();
            }
            if ui.button("Reset").clicked() {
                self.reset();
            }
        });
    }

    /// Column 2: processing parameters plus the fit-overlay toggles.
    fn parameters_and_fitting_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Parameters");

        egui::ComboBox::from_label("Mode")
            .selected_text(match self.main_mode {
                BaselineMainMode::CutOffBaseline => "Cut off baseline",
                BaselineMainMode::BaselineSetting => "Baseline setting",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.main_mode, BaselineMainMode::CutOffBaseline, "Cut off baseline");
                ui.selectable_value(&mut self.main_mode, BaselineMainMode::BaselineSetting, "Baseline setting");
            });

        egui::ComboBox::from_label("Baseline")
            .selected_text(match self.subtract_mode {
                BaselineSubtractMode::Before => "Before",
                BaselineSubtractMode::After => "After",
                BaselineSubtractMode::BeforeLog => "Before (log)",
                BaselineSubtractMode::AfterLog => "After (log)",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.subtract_mode, BaselineSubtractMode::Before, "Before");
                ui.selectable_value(&mut self.subtract_mode, BaselineSubtractMode::After, "After");
                ui.selectable_value(&mut self.subtract_mode, BaselineSubtractMode::BeforeLog, "Before (log)");
                ui.selectable_value(&mut self.subtract_mode, BaselineSubtractMode::AfterLog, "After (log)");
            });

        ui.checkbox(&mut self.use_thresholding, "Use Thresholding");
        ui.add(egui::Slider::new(&mut self.k_factor, 0.1..=10.0).text("K Factor"));

        ui.horizontal_wrapped(|ui| {
            ui.label("Delay (ms)");
            ui.add(egui::DragValue::new(&mut self.delay_time_ms));
        });

        ui.separator();

        ui.label("Fits:");
        let mut fits_changed = false;
        fits_changed |= ui.checkbox(&mut self.show_gaussian_fit, "Gaussian").changed();
        fits_changed |= ui.checkbox(&mut self.show_hemg_single_fit, "HEMG single").changed();
        fits_changed |= ui.checkbox(&mut self.show_hemg_double_fit, "HEMG double").changed();
        fits_changed |= ui.checkbox(&mut self.show_lorentzian_fit, "Lorentzian").changed();
        if fits_changed {
            self.recompute_fits();
        }
    }

    /// Column 3: standalone tools (coincidence-matrix heatmap, save mean).
    fn tools_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Tools");

        if ui.button("Heatmap").clicked() {
            self.status_message = "Heatmap view is not yet wired up in this port.".to_string();
        }

        if ui.add_enabled(self.can_save_mean && !self.is_busy, egui::Button::new("Save Mean")).clicked() {
            self.save_mean();
        }
    }

    /// Column 4: DSSD layer selection, X-axis range, and the X-axis view mode.
    fn dssd_range_view_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("DSSD / Range / View");

        egui::ComboBox::from_label("Layer")
            .selected_text(["L1", "L2", "L6", "L7"][self.selected_layer_index])
            .show_ui(ui, |ui| {
                for (i, name) in ["L1", "L2", "L6", "L7"].iter().enumerate() {
                    ui.selectable_value(&mut self.selected_layer_index, i, *name);
                }
            });

        ui.horizontal_wrapped(|ui| {
            ui.label("X Min");
            ui.add(egui::DragValue::new(&mut self.x_axis_min));
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("X Max");
            ui.add(egui::DragValue::new(&mut self.x_axis_max));
        });

        ui.separator();

        egui::ComboBox::from_label("X Axis")
            .selected_text(["ADC", "Voltage"][self.selected_x_axis_index])
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.selected_x_axis_index, 0, "ADC");
                ui.selectable_value(&mut self.selected_x_axis_index, 1, "Voltage");
            });
    }

    fn status_ui(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(self.status_color, &self.status_message);
        if self.is_busy {
            ui.add(egui::ProgressBar::new((self.progress_value / 100.0) as f32).show_percentage());
        }
        ui.label(format!("Data counts: {}", self.data_counts_str));
        ui.add(egui::Label::new(format!(
            "Start: {}  Stop: {}  Duration: {}",
            self.start_time_str, self.stop_time_str, self.duration_str
        )).wrap());
    }

    // ---- Commands (mirroring [RelayCommand] methods) ----

    fn reset(&mut self) {
        self.selected_files.clear();
        self.input_files_info = "No files selected".to_string();
        self.processed_data.clear();
        self.data_counts_str = "-".to_string();
        for (i, ch) in self.channels.iter_mut().enumerate() {
            *ch = ChannelState {
                channel_index: i,
                title: format!("Channel {}", i + 1),
                stats_text: "No Data".to_string(),
                ..Default::default()
            };
        }
        self.status_message = "Reset complete.".to_string();
        self.status_color = Color32::GRAY;
        self.progress_value = 0.0;
        self.can_save_mean = false;
    }

    fn select_files(&mut self) {
        self.reset();
        let Some(files) = rfd::FileDialog::new()
            .add_filter("Text Files", &["txt"])
            .add_filter("All Files", &["*"])
            .pick_files()
        else {
            return;
        };

        if files.len() == 1 {
            self.output_file_name = files[0]
                .file_stem()
                .map(|s| format!("{}.xlsx", s.to_string_lossy()))
                .unwrap_or_else(|| "output.xlsx".to_string());
            self.input_files_info = "1 file selected.".to_string();
            self.selected_files = files;
            self.status_message = "File loaded. Ready.".to_string();
        } else {
            self.input_files_info = format!("{} files selected.", files.len());
            self.is_busy = true;
            self.status_message = format!("Merging {} files...", files.len());

            let tx = self.tx.clone();
            let output_dir = self.output_directory_path.clone();
            std::thread::spawn(move || merge_files_worker(files, output_dir, tx));
        }
    }

    fn pre_process_data(&mut self) {
        if self.selected_files.is_empty() {
            self.status_message = "No files selected for processing.".to_string();
            self.status_color = Color32::RED;
            return;
        }
        if self.output_file_name.trim().is_empty() {
            self.status_message = "Please provide output filename.".to_string();
            self.status_color = Color32::RED;
            return;
        }

        self.is_busy = true;
        self.status_message = "Processing raw files to Excel...".to_string();
        self.status_color = Color32::from_rgb(255, 165, 0);

        let files = self.selected_files.clone();
        let output_dir = self.output_directory_path.clone();
        let output_file_name = self.output_file_name.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || pre_process_worker(files, output_dir, output_file_name, tx));
    }

    fn process_data(&mut self) {
        self.is_busy = true;
        self.progress_value = 0.0;
        self.status_message = "Processing...".to_string();
        self.can_save_mean = false;

        let output_dir = self.output_directory_path.clone();
        let output_file_name = self.output_file_name.clone();
        let config = ProcessConfig {
            layer_index: self.selected_layer_index,
            x_axis_is_voltage: self.selected_x_axis_index == 1,
            baseline_setting_mode: self.main_mode == BaselineMainMode::BaselineSetting,
            should_subtract: self.subtract_mode.should_subtract(),
            use_thresholding: self.use_thresholding,
            k_factor: self.k_factor,
            x_axis_min: self.x_axis_min,
            x_axis_max: self.x_axis_max,
            show_gaussian_fit: self.show_gaussian_fit,
            show_hemg_single_fit: self.show_hemg_single_fit,
            show_hemg_double_fit: self.show_hemg_double_fit,
            show_lorentzian_fit: self.show_lorentzian_fit,
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || process_data_worker(output_dir, output_file_name, config, tx));
    }

    /// Re-solves the fit curves for every channel that already has a
    /// histogram, without re-reading the Excel file or redoing the
    /// mean-subtraction/thresholding pipeline (unlike `process_data`, which
    /// this reuses `bin_centers`/`counts` in place of). Triggered whenever a
    /// fit checkbox changes, so ticking e.g. "Lorentzian" after Process Data
    /// has already run actually shows the curve instead of doing nothing
    /// until the next full reprocess.
    fn recompute_fits(&mut self) {
        // Don't let a checkbox toggle race an in-flight Process Data (or a
        // prior recompute): both send `Busy(false)` on completion, so
        // whichever finishes first would clear the busy flag while the
        // other is still overwriting channels out from under it.
        if self.is_busy {
            return;
        }
        if self.channels.iter().all(|ch| ch.bin_centers.is_empty()) {
            return;
        }

        self.is_busy = true;
        self.progress_value = 0.0;
        self.status_message = "Refitting...".to_string();
        self.status_color = Color32::from_rgb(255, 165, 0);
        let flags = FitFlags {
            gaussian: self.show_gaussian_fit,
            hemg_single: self.show_hemg_single_fit,
            hemg_double: self.show_hemg_double_fit,
            lorentzian: self.show_lorentzian_fit,
        };
        let snapshots: Vec<ChannelSnapshot> = self
            .channels
            .iter()
            .map(|ch| ChannelSnapshot {
                channel_index: ch.channel_index,
                title: ch.title.clone(),
                bin_centers: ch.bin_centers.clone(),
                counts: ch.counts.clone(),
                is_log_scale: ch.is_log_scale,
            })
            .collect();
        let tx = self.tx.clone();
        std::thread::spawn(move || recompute_fits_worker(snapshots, flags, tx));
    }

    fn save_mean(&mut self) {
        if self.processed_data.is_empty() {
            self.status_message = "No data to process.".to_string();
            return;
        }
        self.is_busy = true;
        self.status_message = "Calculating Means (Multithreaded)...".to_string();

        let data = self.processed_data.clone();
        let output_dir = self.output_directory_path.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || save_mean_worker(data, output_dir, tx));
    }

    fn check_header(&mut self) {
        let Some(file_path) = self.selected_files.first().cloned() else {
            self.status_message = "Please select files first.".to_string();
            return;
        };
        self.header_info_text = "Analyzing File Structure (Head & Tail)...".to_string();
        self.is_busy = true;
        let delay_time_ms = self.delay_time_ms;
        let k_factor = self.k_factor;
        let tx = self.tx.clone();
        std::thread::spawn(move || check_header_worker(file_path, delay_time_ms, k_factor, tx));
    }
}

// ---------------------------------------------------------------------
// Background workers
// ---------------------------------------------------------------------

struct ProcessConfig {
    layer_index: usize,
    x_axis_is_voltage: bool,
    baseline_setting_mode: bool,
    should_subtract: bool,
    use_thresholding: bool,
    k_factor: f64,
    x_axis_min: f64,
    x_axis_max: f64,
    show_gaussian_fit: bool,
    show_hemg_single_fit: bool,
    show_hemg_double_fit: bool,
    show_lorentzian_fit: bool,
}

struct FitFlags {
    gaussian: bool,
    hemg_single: bool,
    hemg_double: bool,
    lorentzian: bool,
}

struct ChannelSnapshot {
    channel_index: usize,
    title: String,
    bin_centers: Vec<f64>,
    counts: Vec<f64>,
    is_log_scale: bool,
}

fn recompute_fits_worker(snapshots: Vec<ChannelSnapshot>, flags: FitFlags, tx: Sender<WorkerMsg>) {
    use rayon::prelude::*;

    let math = MathService::new();
    let results: Vec<(usize, ChannelState)> = snapshots
        .into_par_iter()
        .map(|snap| {
            let mut state = ChannelState {
                channel_index: snap.channel_index,
                title: snap.title,
                is_log_scale: snap.is_log_scale,
                ..Default::default()
            };

            if snap.bin_centers.is_empty() || snap.counts.is_empty() {
                state.stats_text = "No Data".to_string();
                return (snap.channel_index, state);
            }

            let mut fits = Vec::new();
            if flags.gaussian {
                let res = math.gaussian_fit(&snap.bin_centers, &snap.counts);
                if res.is_valid && !res.fit_curve.is_empty() {
                    state.mu = res.mu;
                    state.sigma = res.sigma;
                    state.fwhm = res.fwhm;
                    state.resolution = res.resolution;
                    fits.push(FitCurve { curve: res.fit_curve, color: Color32::from_rgb(50, 220, 50), label: "Gaussian".to_string() });
                }
            }
            if flags.hemg_single {
                let res = math.hemg_double_sided_fit(&snap.bin_centers, &snap.counts, None, None);
                if res.is_valid && !res.fit_curve.is_empty() {
                    fits.push(FitCurve { curve: res.fit_curve, color: Color32::RED, label: "HEMG single".to_string() });
                }
            }
            if flags.hemg_double {
                let res = math.hemg_double_sided_fit(&snap.bin_centers, &snap.counts, None, None);
                if res.is_valid && !res.fit_curve.is_empty() {
                    fits.push(FitCurve { curve: res.fit_curve, color: Color32::from_rgb(220, 50, 220), label: "HEMG double".to_string() });
                }
            }
            if flags.lorentzian {
                let res = math.lorentzian_fit(&snap.bin_centers, &snap.counts);
                if res.is_valid && !res.fit_curve.is_empty() {
                    fits.push(FitCurve { curve: res.fit_curve, color: Color32::from_rgb(0, 220, 220), label: "Lorentzian".to_string() });
                }
            }

            state.stats_text = format!("mu={:.2} sigma={:.2} fwhm={:.2} res={:.2}%", state.mu, state.sigma, state.fwhm, state.resolution);
            state.bin_centers = snap.bin_centers;
            state.counts = snap.counts;
            state.active_fits = fits;
            (snap.channel_index, state)
        })
        .collect();

    for (idx, state) in results {
        let _ = tx.send(WorkerMsg::ChannelResult(idx, state));
    }
    let _ = tx.send(WorkerMsg::Status("Refit complete.".to_string(), Color32::from_rgb(50, 200, 50)));
    let _ = tx.send(WorkerMsg::Progress(100.0));
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn layer_of(data: &BaselineData, layer_index: usize) -> [f32; 16] {
    match layer_index {
        1 => data.l2,
        2 => data.l6,
        3 => data.l7,
        _ => data.l1,
    }
}

fn layer_id_of(layer_index: usize) -> u32 {
    match layer_index {
        1 => 2,
        2 => 6,
        3 => 7,
        _ => 1,
    }
}

fn merge_files_worker(files: Vec<PathBuf>, output_dir: PathBuf, tx: Sender<WorkerMsg>) {
    let today = chrono::Local::now().date_naive();
    let dir = match baseline_core::baseline_processing::get_daily_output_directory(&output_dir, today) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Merge Error: {e}")));
            return;
        }
    };
    let combined_path = dir.join("multiple_file_output.txt");

    let result: std::io::Result<()> = (|| {
        let mut out = std::fs::File::create(&combined_path)?;
        for file in &files {
            let mut input = std::fs::File::open(file)?;
            std::io::copy(&mut input, &mut out)?;
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            let _ = tx.send(WorkerMsg::InputFilesInfo(format!(
                "Merged into {}",
                combined_path.display()
            )));
            let _ = tx.send(WorkerMsg::OutputFileName("multiple_file_output.xlsx".to_string()));
            let _ = tx.send(WorkerMsg::Status("Merge Complete. Ready.".to_string(), Color32::from_rgb(50, 200, 50)));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Merge Error: {e}")));
        }
    }
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn pre_process_worker(files: Vec<PathBuf>, output_dir: PathBuf, output_file_name: String, tx: Sender<WorkerMsg>) {
    let mut all_data = Vec::new();

    for (i, file) in files.iter().enumerate() {
        let _ = tx.send(WorkerMsg::Status(format!("Processing file {}/{}...", i + 1, files.len()), Color32::from_rgb(255, 165, 0)));
        match io::process_file_stream(file, None) {
            Ok(mut data) => all_data.append(&mut data),
            Err(e) => {
                let _ = tx.send(WorkerMsg::Error(e));
                let _ = tx.send(WorkerMsg::Busy(false));
                return;
            }
        }
    }

    if all_data.is_empty() {
        let _ = tx.send(WorkerMsg::Status("No valid data found in selected files.".to_string(), Color32::RED));
        let _ = tx.send(WorkerMsg::Busy(false));
        return;
    }

    let mut file_name = output_file_name;
    if !file_name.to_ascii_lowercase().ends_with(".xlsx") {
        file_name.push_str(".xlsx");
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
    let full_path = dir.join(&file_name);

    let _ = tx.send(WorkerMsg::Status("Saving to Excel...".to_string(), Color32::from_rgb(255, 165, 0)));
    match io::excel::save_baseline_to_excel(&all_data, &full_path) {
        Ok(()) => {
            let _ = tx.send(WorkerMsg::Status(
                format!("Saved {} events to {}", all_data.len(), file_name),
                Color32::from_rgb(50, 200, 50),
            ));
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Failed to save Excel file: {e}")));
        }
    }
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn process_data_worker(output_dir: PathBuf, output_file_name: String, config: ProcessConfig, tx: Sender<WorkerMsg>) {
    let start = chrono::Local::now();

    let mut file_name = output_file_name;
    if !file_name.to_ascii_lowercase().ends_with(".xlsx") {
        file_name.push_str(".xlsx");
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
    let full_path = dir.join(&file_name);

    if !full_path.exists() {
        let _ = tx.send(WorkerMsg::Error(format!(
            "Expected input file not found: {}. Please run 'Pre-Process to Excel' first.",
            full_path.display()
        )));
        let _ = tx.send(WorkerMsg::Busy(false));
        return;
    }

    let _ = tx.send(WorkerMsg::Status("Reading Excel...".to_string(), Color32::from_rgb(255, 165, 0)));
    let data = match io::excel::read_baseline_excel(&full_path) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Read Error: {e}")));
            let _ = tx.send(WorkerMsg::Busy(false));
            return;
        }
    };

    let _ = tx.send(WorkerMsg::DataLoaded(data.clone()));
    let _ = tx.send(WorkerMsg::Progress(50.0));

    if data.is_empty() {
        let _ = tx.send(WorkerMsg::Busy(false));
        return;
    }

    let math = MathService::new();
    let output_dir_for_means = dir.clone();

    use rayon::prelude::*;
    let results: Vec<(usize, ChannelState)> = (0..16usize)
        .into_par_iter()
        .map(|ch_index| {
            let raw_data: Vec<f64> = data.iter().map(|d| layer_of(d, config.layer_index)[ch_index] as f64).collect();

            let mut working = raw_data.clone();
            if config.should_subtract {
                let mean = if !config.baseline_setting_mode {
                    let m = baseline_core::baseline_processing::load_mean_from_file(&output_dir_for_means, layer_id_of(config.layer_index), ch_index);
                    if m == 0.0 {
                        baseline_core::baseline_processing::calculate_mean(&raw_data)
                    } else {
                        m
                    }
                } else {
                    baseline_core::baseline_processing::calculate_mean(&raw_data)
                };
                for v in working.iter_mut() {
                    *v -= mean;
                }
            }

            let filtered = baseline_core::baseline_processing::apply_thresholding(&working, config.k_factor, config.use_thresholding);

            let mut state = ChannelState {
                channel_index: ch_index,
                title: format!("Channel {}", ch_index + 1),
                ..Default::default()
            };

            if filtered.is_empty() {
                state.stats_text = "No Data".to_string();
                return (ch_index, state);
            }

            let (counts, bin_centers) = baseline_core::baseline_processing::build_histogram(
                &filtered,
                config.x_axis_min,
                config.x_axis_max,
                16384,
                config.x_axis_is_voltage,
            );

            let mut fits = Vec::new();
            if filtered.len() > 5 {
                if config.show_gaussian_fit {
                    let res = math.gaussian_fit(&bin_centers, &counts);
                    if res.is_valid && !res.fit_curve.is_empty() {
                        state.mu = res.mu;
                        state.sigma = res.sigma;
                        state.fwhm = res.fwhm;
                        state.resolution = res.resolution;
                        fits.push(FitCurve { curve: res.fit_curve, color: Color32::from_rgb(50, 220, 50), label: "Gaussian".to_string() });
                    }
                }
                if config.show_hemg_single_fit {
                    let res = math.hemg_double_sided_fit(&bin_centers, &counts, Some(&filtered), None);
                    if res.is_valid && !res.fit_curve.is_empty() {
                        fits.push(FitCurve { curve: res.fit_curve, color: Color32::RED, label: "HEMG single".to_string() });
                    }
                }
                if config.show_hemg_double_fit {
                    let res = math.hemg_double_sided_fit(&bin_centers, &counts, Some(&filtered), None);
                    if res.is_valid && !res.fit_curve.is_empty() {
                        fits.push(FitCurve { curve: res.fit_curve, color: Color32::from_rgb(220, 50, 220), label: "HEMG double".to_string() });
                    }
                }
                if config.show_lorentzian_fit {
                    let res = math.lorentzian_fit(&bin_centers, &counts);
                    if res.is_valid && !res.fit_curve.is_empty() {
                        fits.push(FitCurve { curve: res.fit_curve, color: Color32::from_rgb(0, 220, 220), label: "Lorentzian".to_string() });
                    }
                }
            }

            state.stats_text = format!("mu={:.2} sigma={:.2} fwhm={:.2} res={:.2}%", state.mu, state.sigma, state.fwhm, state.resolution);
            state.counts = counts;
            state.bin_centers = bin_centers;
            state.active_fits = fits;

            (ch_index, state)
        })
        .collect();

    for (idx, state) in results {
        let _ = tx.send(WorkerMsg::ChannelResult(idx, state));
    }

    let stop = chrono::Local::now();
    let duration = stop.signed_duration_since(start);
    let _ = tx.send(WorkerMsg::Timing {
        start: start.format("%H:%M:%S").to_string(),
        stop: stop.format("%H:%M:%S").to_string(),
        duration: format!("{} ms", duration.num_milliseconds()),
    });
    let _ = tx.send(WorkerMsg::Status(format!("Processed {} events.", data.len()), Color32::from_rgb(50, 200, 50)));
    let _ = tx.send(WorkerMsg::Progress(100.0));
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn save_mean_worker(data: Vec<BaselineData>, output_dir: PathBuf, tx: Sender<WorkerMsg>) {
    let today = chrono::Local::now().date_naive();
    let dir = match baseline_core::baseline_processing::get_daily_output_directory(&output_dir, today) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(e.to_string()));
            let _ = tx.send(WorkerMsg::Busy(false));
            return;
        }
    };

    let layers: [(u32, fn(&BaselineData) -> [f32; 16]); 4] =
        [(1, |d| d.l1), (2, |d| d.l2), (6, |d| d.l6), (7, |d| d.l7)];

    for (layer_id, selector) in layers {
        let means = baseline_core::baseline_processing::calculate_mean_parallel(&data, selector);
        if let Err(e) = baseline_core::baseline_processing::write_means_to_file(&dir, layer_id, &means) {
            let _ = tx.send(WorkerMsg::Error(format!("Error saving means: {e}")));
            let _ = tx.send(WorkerMsg::Busy(false));
            return;
        }
    }

    let _ = tx.send(WorkerMsg::Status("Mean Values Saved Successfully.".to_string(), Color32::from_rgb(50, 200, 50)));
    let _ = tx.send(WorkerMsg::Busy(false));
}

fn check_header_worker(file_path: PathBuf, delay_time_ms: i32, k_factor: f64, tx: Sender<WorkerMsg>) {
    let text = check_header_summary(&file_path, delay_time_ms, k_factor).unwrap_or_else(|e| format!("Error: {e}"));
    let _ = tx.send(WorkerMsg::HeaderInfo(text));
    let _ = tx.send(WorkerMsg::Status("Header Analysis Complete.".to_string(), Color32::GRAY));
    let _ = tx.send(WorkerMsg::Busy(false));
}

/// Matches `CheckHeader` + `ReadFileBlock`/`ParseHeaderSummary`: reads the
/// first ~16KB and last ~16KB of the file as hex text and summarizes the
/// first/last packet timestamps.
fn check_header_summary(path: &Path, delay_time_ms: i32, k_factor: f64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};

    const BUFFER_SIZE: usize = 16384;
    let file_len = std::fs::metadata(path)?.len();

    let mut file = std::fs::File::open(path)?;
    let mut head_buf = vec![0u8; BUFFER_SIZE.min(file_len as usize)];
    file.read_exact(&mut head_buf)?;
    let head_hex = String::from_utf8_lossy(&head_buf).to_string();

    let seek_pos = file_len.saturating_sub(BUFFER_SIZE as u64);
    file.seek(SeekFrom::Start(seek_pos))?;
    let mut tail_buf = Vec::new();
    file.read_to_end(&mut tail_buf)?;
    let tail_hex = String::from_utf8_lossy(&tail_buf).to_string();

    let mut out = String::new();
    out.push_str(&format!("File Size: {:.2} MB\n", file_len as f64 / (1024.0 * 1024.0)));
    out.push_str("--------------------------------------------------\n");
    out.push_str("[START OF FILE]\n");
    match parse_first_packet(&head_hex) {
        Some(bytes) => out.push_str(&parse_header_summary(&bytes)),
        None => out.push_str("Error: Could not read start of file."),
    }
    out.push('\n');
    out.push_str("--------------------------------------------------\n");
    out.push_str("[END OF FILE]\n");
    match find_last_packet(&tail_hex) {
        Some(bytes) => out.push_str(&parse_header_summary(&bytes)),
        None => out.push_str("Warning: Could not identify a valid packet at the end of file.\n(File might be truncated or format is inconsistent)"),
    }
    out.push('\n');
    out.push_str("--------------------------------------------------\n");
    out.push_str(&format!("Delay Time: {delay_time_ms}\n"));
    out.push_str(&format!("Threshold: {k_factor}\n"));

    Ok(out)
}

fn clean_hex(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

fn parse_first_packet(hex_content: &str) -> Option<Vec<u8>> {
    if hex_content.is_empty() {
        return None;
    }
    let mut clean = clean_hex(hex_content);
    if clean.len() > 4128 {
        clean.truncate(4128);
    }
    if clean.len() % 2 != 0 {
        clean.pop();
    }
    hex_to_bytes(&clean)
}

fn find_last_packet(hex_content: &str) -> Option<Vec<u8>> {
    if hex_content.is_empty() {
        return None;
    }
    let clean = clean_hex(hex_content);
    let upper = clean.to_ascii_uppercase();
    let last_index = upper.rfind("AA55")?;

    let expected_hex_length = 4128;
    if last_index + expected_hex_length <= clean.len() {
        hex_to_bytes(&clean[last_index..last_index + expected_hex_length])
    } else {
        let mut partial = clean[last_index..].to_string();
        if partial.len() % 2 != 0 {
            partial.pop();
        }
        hex_to_bytes(&partial)
    }
}

fn parse_header_summary(data: &[u8]) -> String {
    if data.len() < 14 {
        return "Invalid/Incomplete Packet Data".to_string();
    }
    let seconds_part = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let milliseconds_part = ((data[13] as u16) << 8) | data[12] as u16;

    let total_ms = seconds_part as i64 * 1000 + milliseconds_part as i64;
    let dt = chrono::DateTime::from_timestamp(total_ms / 1000, ((total_ms % 1000) * 1_000_000) as u32)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).unwrap());

    format!(
        "Timestamp: {} (UTC)\nSync Code: {:02X} {:02X}\nSeq No:    {:02X} {:02X}",
        dt.format("%Y-%b-%d %H:%M:%S%.3f"),
        data[0],
        data[1],
        data[4],
        data[5]
    )
}

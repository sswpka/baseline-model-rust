//! Direct transcription of `Infrastructure/Services/Observation/DataProcessor.cs`
//! (`ObservationDataProcessor`).
//!
//! The C# stored each particle's decoded fields in a loosely-typed
//! `Dictionary<string, object>` (kept, per its own comments, "for legacy
//! UI binding"); since there is no WPF binding to replicate here, that is
//! represented as the typed [`ParticleResult`] instead. `StorageDataList`
//! and `AllResults` held the exact same objects in the same order in the
//! original, so they're collapsed into the single `results` field.

use crate::math::KalmanFilter;
use crate::models::observation::{BgoData, BgoLayer, DetectorLayer, LayerData};
use chrono::{DateTime, NaiveDate, Utc};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

const HEADER_OFFSET: usize = 16;
const PARTICLE_DATA_LENGTH: usize = 34;
const PARTICLES_PER_LINE: usize = 5;
const HISTOGRAM_SIZE: usize = 16384;

#[derive(Debug, Clone, Default)]
pub struct ParticleResult {
    pub particle_data: Vec<String>,
    pub particle_number: i32,
    pub milliseconds: i32,
    /// The event's absolute time: the containing line's timecode (decoded
    /// via `get_date_time_from_hex_data`) plus this particle's own
    /// `milliseconds` offset - every particle on the same line shares the
    /// line's timecode but gets a distinct sub-second offset from it.
    pub time: DateTime<Utc>,
    /// (X, Y) pulse heights per DSSD layer.
    pub dssd_pulses: HashMap<DetectorLayer, (i32, i32)>,
    /// (High gain, Low gain) pulse heights per BGO layer.
    pub bgo_pulses: HashMap<BgoLayer, (i32, i32)>,
}

pub struct ObservationDataProcessor {
    pub results: Vec<ParticleResult>,
    pub dssd_data: HashMap<DetectorLayer, LayerData>,
    pub bgo_data: HashMap<BgoLayer, BgoData>,

    pub kalman_l3_bgo_h: KalmanFilter,
    pub kalman_l3_bgo_l: KalmanFilter,
    pub kalman_l4_bgo_h: KalmanFilter,
    pub kalman_l4_bgo_l: KalmanFilter,
    pub kalman_l5_bgo_h: KalmanFilter,
    pub kalman_l5_bgo_l: KalmanFilter,

    pub kalman_bgo_low_gain: KalmanFilter,
    pub kalman_bgo_high_gain: KalmanFilter,
}

impl Default for ObservationDataProcessor {
    fn default() -> Self {
        let mut dssd_data = HashMap::new();
        for layer in [DetectorLayer::L1, DetectorLayer::L2, DetectorLayer::L6, DetectorLayer::L7] {
            dssd_data.insert(layer, LayerData::default());
        }
        let mut bgo_data = HashMap::new();
        for layer in [BgoLayer::L3, BgoLayer::L4, BgoLayer::L5] {
            bgo_data.insert(layer, BgoData::default());
        }

        Self {
            results: Vec::new(),
            dssd_data,
            bgo_data,
            kalman_l3_bgo_h: KalmanFilter::new(1.0, 1.0, 1.0, 10.0, 1.0, 0.0),
            kalman_l3_bgo_l: KalmanFilter::new(1.0, 1.0, 1.0, 10.0, 1.0, 0.0),
            kalman_l4_bgo_h: KalmanFilter::new(1.0, 1.0, 1.0, 10.0, 1.0, 0.0),
            kalman_l4_bgo_l: KalmanFilter::new(1.0, 1.0, 1.0, 10.0, 1.0, 0.0),
            kalman_l5_bgo_h: KalmanFilter::new(1.0, 1.0, 1.0, 10.0, 1.0, 0.0),
            kalman_l5_bgo_l: KalmanFilter::new(1.0, 1.0, 1.0, 10.0, 1.0, 0.0),
            kalman_bgo_low_gain: KalmanFilter::new(1.0, 1.0, 1.0, 1.0, 1.0, 0.0),
            kalman_bgo_high_gain: KalmanFilter::new(1.0, 1.0, 1.0, 1.0, 1.0, 0.0),
        }
    }
}

impl ObservationDataProcessor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear_data(&mut self) {
        self.results.clear();

        for layer in self.dssd_data.values_mut() {
            layer.pulse_height_x.clear();
            layer.pulse_height_y.clear();
            for strip in layer.strip_x.values_mut() {
                strip.clear();
            }
            for strip in layer.strip_y.values_mut() {
                strip.clear();
            }
        }

        for layer in self.bgo_data.values_mut() {
            layer.high_gain.clear();
            layer.low_gain.clear();
        }
        // Kalman filters are not reset here, matching the C# original.
    }

    /// Processes multiple files, returning per-layer X/Y pulse-height
    /// histograms keyed `"DSSD{layer}_X"` / `"DSSD{layer}_Y"`.
    pub fn process_files(&mut self, file_paths: &[impl AsRef<Path>]) -> Result<HashMap<String, Vec<i32>>, String> {
        self.clear_data();
        for path in file_paths {
            self.process_file(path.as_ref())?;
        }
        Ok(self.get_histogram_data())
    }

    fn process_file(&mut self, file_path: &Path) -> Result<(), String> {
        let content = fs::read_to_string(file_path).map_err(|e| e.to_string())?;
        for line in content.lines().skip(1) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let hex_data = split_hex_data(trimmed);
            if hex_data.len() >= HEADER_OFFSET + PARTICLE_DATA_LENGTH {
                // Best-effort: an undecodable line timecode falls back to the
                // Unix epoch rather than skipping the line's particles
                // entirely, matching `get_date_time_from_hex`'s epoch
                // fallback in `flux::processing`.
                let line_time = get_date_time_from_hex_data(&hex_data).unwrap_or_else(|_| DateTime::from_timestamp(0, 0).unwrap());
                self.process_particles(&hex_data, line_time);
            }
        }
        Ok(())
    }

    pub fn read_header(file_path: &Path) -> Result<String, String> {
        let content = fs::read_to_string(file_path).map_err(|e| e.to_string())?;
        let lines: Vec<&str> = content.lines().collect();
        if lines.is_empty() {
            return Ok("Empty file".to_string());
        }

        let header_line = lines[0];
        let mut out = String::new();
        out.push_str(&format!(
            "File: {}\n",
            file_path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()
        ));
        out.push_str(&format!("Header Length: {} characters\n", header_line.len()));
        out.push_str(&format!("Total Lines: {}\n", lines.len()));

        if header_line.len() >= 28 {
            let hex_data = split_hex_data(header_line);
            match get_date_time_from_hex_data(&hex_data) {
                Ok(dt) => out.push_str(&format!("Start Time: {}\n", dt.format("%Y-%m-%d %H:%M:%S"))),
                Err(_) => out.push_str("Start Time: Unable to parse\n"),
            }
        }

        out.push_str("\nRaw Header (first 100 chars):\n");
        if header_line.len() > 100 {
            out.push_str(&header_line[..100]);
            out.push_str("...");
        } else {
            out.push_str(header_line);
        }
        out.push('\n');

        Ok(out)
    }

    /// Builds fixed-size (`HISTOGRAM_SIZE`-channel) count histograms for every
    /// DSSD pulse-height/strip series and every BGO gain series, keyed so the
    /// UI can look up whichever layer/strip/mode the user has selected
    /// without having to reprocess the source files. Mirrors
    /// `RefreshDSSDPlots`/`RefreshBGOPlots` in the original WPF code-behind,
    /// minus the dynamic bin-range and curve-fitting logic (deferred - see
    /// module notes).
    ///
    /// Every particle contributes a pulse-height entry to every DSSD/BGO
    /// layer regardless of whether that layer actually fired (see
    /// `process_dssd_layer`/`process_bgo_layer`), so most particles record a
    /// `0` in the layers they didn't hit. The main X/Y and BGO histograms
    /// drop those zeros (`histogram_of_positive`), matching the original's
    /// `PlotHistogram`/`PlotBGOHistogram` (`data.Where(v => v > 0)`) -
    /// otherwise channel 0 accumulates a massive non-physical spike that
    /// wins every peak search and gets handed to the Gaussian/Lorentzian fit
    /// instead of the real photopeak. Strip histograms intentionally keep
    /// zeros, matching `PlotStripHistogram`'s `v >= xMin` filter (default
    /// `xMin = 0`).
    fn get_histogram_data(&self) -> HashMap<String, Vec<i32>> {
        let mut result = HashMap::new();

        for layer in [DetectorLayer::L1, DetectorLayer::L2, DetectorLayer::L6, DetectorLayer::L7] {
            let data = &self.dssd_data[&layer];
            let name = layer_name(layer);

            result.insert(format!("DSSD{name}_X"), histogram_of_positive(&data.pulse_height_x));
            result.insert(format!("DSSD{name}_Y"), histogram_of_positive(&data.pulse_height_y));

            for strip in 1..=8 {
                let empty = Vec::new();
                let strip_x = data.strip_x.get(&strip).unwrap_or(&empty);
                let strip_y = data.strip_y.get(&strip).unwrap_or(&empty);
                result.insert(format!("DSSD{name}_StripX{strip}"), histogram_of_i32(strip_x));
                result.insert(format!("DSSD{name}_StripY{strip}"), histogram_of_i32(strip_y));
            }
        }

        for layer in [BgoLayer::L3, BgoLayer::L4, BgoLayer::L5] {
            let data = &self.bgo_data[&layer];
            let name = bgo_layer_name(layer);
            result.insert(format!("BGO{name}_High"), histogram_of_positive(&data.high_gain));
            result.insert(format!("BGO{name}_Low"), histogram_of_positive(&data.low_gain));
        }

        result
    }

    pub fn process_particles(&mut self, hex_data: &[String], line_time: DateTime<Utc>) {
        for i in 0..PARTICLES_PER_LINE {
            let start = HEADER_OFFSET + PARTICLE_DATA_LENGTH * i;
            if start >= hex_data.len() {
                break;
            }
            let end = (start + PARTICLE_DATA_LENGTH).min(hex_data.len());
            let particle_data = &hex_data[start..end];
            if particle_data.len() < PARTICLE_DATA_LENGTH {
                break;
            }

            if let Ok(processed) = self.process_particle_data(particle_data, i, line_time) {
                self.results.push(processed);
            }
        }
    }

    pub fn process_particle_data(&mut self, particle_data: &[String], i: usize, line_time: DateTime<Utc>) -> Result<ParticleResult, String> {
        let particle_data_dec: Vec<i32> = particle_data
            .iter()
            .map(|hex| i32::from_str_radix(hex, 16).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;

        if particle_data_dec.len() < 7 {
            return Err("Insufficient particle data".to_string());
        }

        let milliseconds = (((particle_data_dec[0] << 8) + particle_data_dec[1]) / 1000) as i32;
        let time = line_time + chrono::Duration::milliseconds(milliseconds as i64);

        // --- DSSD layers ---
        // L1: pos idx 2, X-Ph idx 3-4, Y-Ph idx 5-6
        // L2: pos idx 7, X-Ph idx 8-9, Y-Ph idx 10-11
        // L6: pos idx 24, X-Ph idx 25-26, Y-Ph idx 27-28
        // L7: pos idx 29, X-Ph idx 30-31, Y-Ph idx 32-33
        let mut dssd_pulses = HashMap::new();
        self.process_dssd_layer(&particle_data_dec, DetectorLayer::L1, 2, 3, 5, &mut dssd_pulses);
        self.process_dssd_layer(&particle_data_dec, DetectorLayer::L2, 7, 8, 10, &mut dssd_pulses);
        self.process_dssd_layer(&particle_data_dec, DetectorLayer::L6, 24, 25, 27, &mut dssd_pulses);
        self.process_dssd_layer(&particle_data_dec, DetectorLayer::L7, 29, 30, 32, &mut dssd_pulses);

        // --- BGO layers ---
        // L3: H idx 12-13, L idx 14-15
        // L4: H idx 16-17, L idx 18-19
        // L5: H idx 20-21, L idx 22-23
        let mut bgo_pulses = HashMap::new();
        self.process_bgo_layer(&particle_data_dec, BgoLayer::L3, 12, 14, &mut bgo_pulses)?;
        self.process_bgo_layer(&particle_data_dec, BgoLayer::L4, 16, 18, &mut bgo_pulses)?;
        self.process_bgo_layer(&particle_data_dec, BgoLayer::L5, 20, 22, &mut bgo_pulses)?;

        Ok(ParticleResult {
            particle_data: particle_data.to_vec(),
            particle_number: i as i32 + 1,
            milliseconds,
            time,
            dssd_pulses,
            bgo_pulses,
        })
    }

    fn process_dssd_layer(
        &mut self,
        data: &[i32],
        layer: DetectorLayer,
        pos_idx: usize,
        x_ph_idx: usize,
        y_ph_idx: usize,
        out: &mut HashMap<DetectorLayer, (i32, i32)>,
    ) {
        let detect_x = (data[pos_idx] & 240) >> 4; // upper 4 bits
        let detect_y = data[pos_idx] & 15; // lower 4 bits

        let ph_x = (data[x_ph_idx] << 8) + data[x_ph_idx + 1];
        let ph_y = (data[y_ph_idx] << 8) + data[y_ph_idx + 1];
        out.insert(layer, (ph_x, ph_y));

        let layer_data = self.dssd_data.get_mut(&layer).unwrap();
        layer_data.pulse_height_x.push(ph_x as f64);
        layer_data.pulse_height_y.push(ph_y as f64);

        // Strip key mapping (see original comments): detect==8 -> strip 0,
        // otherwise detect+1 -> strip (detect+1).
        let target_strip_x = if detect_x == 8 { 0 } else { detect_x + 1 };
        let target_strip_y = if detect_y == 8 { 0 } else { detect_y + 1 };

        if let Some(v) = layer_data.strip_x.get_mut(&target_strip_x) {
            v.push(ph_x);
        }
        if let Some(v) = layer_data.strip_y.get_mut(&target_strip_y) {
            v.push(ph_y);
        }
    }

    fn process_bgo_layer(
        &mut self,
        data: &[i32],
        layer: BgoLayer,
        h_idx: usize,
        l_idx: usize,
        out: &mut HashMap<BgoLayer, (i32, i32)>,
    ) -> Result<(), String> {
        let ph_h = (data[h_idx] << 8) + data[h_idx + 1];
        let ph_l = (data[l_idx] << 8) + data[l_idx + 1];

        if ph_h < 0 || ph_l < 0 {
            return Err("Pulse heights must be non-negative.".to_string());
        }

        let bgo = self.bgo_data.get_mut(&layer).unwrap();
        bgo.high_gain.push(ph_h as f64);
        bgo.low_gain.push(ph_l as f64);

        out.insert(layer, (ph_h, ph_l));
        Ok(())
    }
}

fn layer_name(layer: DetectorLayer) -> &'static str {
    match layer {
        DetectorLayer::L1 => "L1",
        DetectorLayer::L2 => "L2",
        DetectorLayer::L6 => "L6",
        DetectorLayer::L7 => "L7",
    }
}

fn bgo_layer_name(layer: BgoLayer) -> &'static str {
    match layer {
        BgoLayer::L3 => "L3",
        BgoLayer::L4 => "L4",
        BgoLayer::L5 => "L5",
    }
}

/// Excludes non-positive values - see `get_histogram_data`'s doc comment
/// for why this matters for the main DSSD X/Y and BGO histograms.
fn histogram_of_positive(values: &[f64]) -> Vec<i32> {
    let mut hist = vec![0i32; HISTOGRAM_SIZE];
    for &value in values {
        let idx = value as i64;
        if idx > 0 && (idx as usize) < HISTOGRAM_SIZE {
            hist[idx as usize] += 1;
        }
    }
    hist
}

fn histogram_of_i32(values: &[i32]) -> Vec<i32> {
    let mut hist = vec![0i32; HISTOGRAM_SIZE];
    for &value in values {
        if value >= 0 && (value as usize) < HISTOGRAM_SIZE {
            hist[value as usize] += 1;
        }
    }
    hist
}

/// The instrument's own timecode base: the 6-byte timecode this function
/// decodes (4-byte seconds + 2-byte milliseconds) counts up from this
/// moment, not from the Unix epoch.
fn observation_epoch() -> DateTime<Utc> {
    DateTime::from_naive_utc_and_offset(NaiveDate::from_ymd_opt(2024, 10, 1).unwrap().and_hms_opt(0, 0, 0).unwrap(), Utc)
}

pub fn get_date_time_from_hex_data(hex_data: &[String]) -> Result<DateTime<Utc>, String> {
    if hex_data.len() < 14 {
        return Err("insufficient hex data for timecode".to_string());
    }
    let timecode_hex = &hex_data[8..14];
    let timecode_dec: Vec<u8> = timecode_hex
        .iter()
        .map(|h| u8::from_str_radix(h, 16).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;

    let seconds_part = u32::from_be_bytes([timecode_dec[0], timecode_dec[1], timecode_dec[2], timecode_dec[3]]);
    let milliseconds_part = u16::from_be_bytes([timecode_dec[4], timecode_dec[5]]);

    Ok(observation_epoch() + chrono::Duration::seconds(seconds_part as i64) + chrono::Duration::milliseconds(milliseconds_part as i64))
}

pub fn split_hex_data(hex_string: &str) -> Vec<String> {
    if hex_string.is_empty() {
        return Vec::new();
    }
    let bytes: Vec<char> = hex_string.chars().collect();
    let count = bytes.len() / 2;
    (0..count).map(|i| bytes[i * 2..i * 2 + 2].iter().collect()).collect()
}

pub fn validate_header(hex_data: &[String]) -> bool {
    if hex_data.len() < 2 {
        return false;
    }
    hex_data[0].eq_ignore_ascii_case("E2") && hex_data[1].eq_ignore_ascii_case("25")
}


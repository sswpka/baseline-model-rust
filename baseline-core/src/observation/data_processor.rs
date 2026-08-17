//! Data Processing for Observation Mode

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

/// Contruct a line's tail fields
pub struct LineTailField {
    pub label: &'static str,
    pub byte_start: usize,
    pub byte_len: usize,
    pub is_hex: bool,
}

const fn dec(label: &'static str, byte_start: usize, byte_len: usize) -> LineTailField {
    LineTailField { label, byte_start, byte_len, is_hex: false }
}

pub const LINE_TAIL_FIELDS: &[LineTailField] = &[
    dec("Galactic Electron Count", 186, 1),
    dec("Albedo Electron Count", 187, 1),
    dec("Galactic Ion Count", 188, 1),
    dec("Albedo Ion Count", 189, 1),
    dec("L1 Ion Threshold", 190, 2),
    dec("L1 Electron Threshold", 192, 2),
    dec("L2 Ion Particle", 194, 2),
    dec("L2 Electron Particle", 196, 2),
    dec("L3 Ion Particle", 198, 2),
    dec("L3 Electron Particle", 200, 2),
    dec("L4 Ion Particle", 202, 2),
    dec("L4 Electron Particle", 204, 2),
    dec("L5 Ion Particle", 206, 2),
    dec("L5 Electron Particle", 208, 2),
    dec("L6 Ion Particle", 210, 2),
    dec("L6 Electron Particle", 212, 2),
    dec("L7 Ion Particle", 214, 2),
    dec("L7 Electron Particle", 216, 2),
    dec("DSSD1 Temperature", 218, 2),
    dec("FEE1 Current", 220, 2),
    dec("FEE1 Temperature", 222, 2),
    dec("DSSD7 Temperature", 224, 2),
    dec("FEE2 Current", 226, 2),
    dec("FEE2 Temperature", 228, 2),
    dec("FEE1 Threshold", 230, 2),
    dec("FEE2 Threshold", 232, 2),
    dec("BGO1 Bias Voltage", 234, 2),
    dec("BGO2 Bias Voltage", 236, 2),
    dec("BGO3 Bias Voltage", 238, 2),
    dec("BGO2 Temperature", 240, 2),
    LineTailField { label: "Padding", byte_start: 242, byte_len: 12, is_hex: true },
    LineTailField { label: "Checksum", byte_start: 254, byte_len: 2, is_hex: true },
];

/// Formular decode for FEE2 Temperature and the BGO bias/temperature fields.
fn formula_decode(x: f64) -> f64 {
    65.17 * (x * 4096.0 / 3.3).powf(0.1401) - 141.6
}

/// Fields whose raw decoded value must be run through `formula_decode`.
/// BGO bias-voltage/temperature fields additionally get scaled by 0.01.
fn calibrated_value(label: &str, raw: f64) -> Option<f64> {
    match label {
        "FEE2 Temperature" => Some(formula_decode(raw)),
        "BGO1 Bias Voltage" | "BGO2 Bias Voltage" | "BGO3 Bias Voltage" | "BGO2 Temperature" => {
            Some(formula_decode(raw) * 0.01)
        }
        _ => None,
    }
}

/// Decoding LINE_TAIL_FIELDS
fn parse_line_tail_fields(hex_data: &[String]) -> Vec<String> {
    let byte = |i: usize| -> i32 { hex_data.get(i).and_then(|h| i32::from_str_radix(h, 16).ok()).unwrap_or(0) };
    LINE_TAIL_FIELDS
        .iter()
        .map(|f| {
            if f.is_hex {
                (f.byte_start..f.byte_start + f.byte_len).map(|i| hex_data.get(i).cloned().unwrap_or_else(|| "00".to_string())).collect()
            } else if f.byte_len == 1 {
                let raw = byte(f.byte_start);
                match calibrated_value(f.label, raw as f64) {
                    Some(value) => format!("{value:.4}"),
                    None => raw.to_string(),
                }
            } else {
                let raw = (byte(f.byte_start) << 8) + byte(f.byte_start + 1);
                match calibrated_value(f.label, raw as f64) {
                    Some(value) => format!("{value:.4}"),
                    None => raw.to_string(),
                }
            }
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct ParticleResult {
    pub particle_data: Vec<String>,
    pub particle_number: i32,
    /// Raw decoded decimal value of the Particle Time field (Byte 0-1).
    pub particle_time_raw: i32,
    pub milliseconds: i32,
    pub time: DateTime<Utc>,
    /// (X, Y) pulse heights per DSSD layer.
    pub dssd_pulses: HashMap<DetectorLayer, (i32, i32)>,
    /// Raw decoded decimal value of the XY Position byte per DSSD layer.
    pub dssd_positions: HashMap<DetectorLayer, i32>,
    /// (High gain, Low gain) pulse heights per BGO layer.
    pub bgo_pulses: HashMap<BgoLayer, (i32, i32)>,
    pub packet_sync: String,
    pub package_id: i32,
    pub packet_sequence: i32,
    pub packet_data_length: i32,
    pub data_type: String,
    pub line_tail: Vec<String>,
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
    }

    /// Processes multiple files, returning per-layer X/Y pulse-height
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
            // A package shorter than 256 bytes (512 hex characters) are skipped
            if hex_data.len() < 256 {
                continue;
            }
            if hex_data.len() >= HEADER_OFFSET + PARTICLE_DATA_LENGTH {
                let header = parse_line_header_fields(&hex_data).unwrap_or_default();
                let line_time = get_date_time_from_hex_data(&hex_data).unwrap_or_else(|_| DateTime::from_timestamp(0, 0).unwrap());
                self.process_particles(&hex_data, line_time, &header);
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
    /// DSSD pulse-height/strip series and every BGO gain series
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

    pub fn process_particles(&mut self, hex_data: &[String], line_time: DateTime<Utc>, header: &LineHeaderFields) {
        let line_tail = parse_line_tail_fields(hex_data);
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

            if let Ok(processed) = self.process_particle_data(particle_data, i, line_time, header, &line_tail) {
                self.results.push(processed);
            }
        }
    }

    pub fn process_particle_data(
        &mut self,
        particle_data: &[String],
        i: usize,
        line_time: DateTime<Utc>,
        header: &LineHeaderFields,
        line_tail: &[String],
    ) -> Result<ParticleResult, String> {
        let particle_data_dec: Vec<i32> = particle_data
            .iter()
            .map(|hex| i32::from_str_radix(hex, 16).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;

        if particle_data_dec.len() < 7 {
            return Err("Insufficient particle data".to_string());
        }

        let particle_time_raw = (particle_data_dec[0] << 8) + particle_data_dec[1];
        let milliseconds = particle_time_raw / 1000;
        let time = line_time + chrono::Duration::milliseconds(milliseconds as i64);

        // --- DSSD layers ---
        // L1: pos idx 2, X-Ph idx 3-4, Y-Ph idx 5-6
        // L2: pos idx 7, X-Ph idx 8-9, Y-Ph idx 10-11
        // L6: pos idx 24, X-Ph idx 25-26, Y-Ph idx 27-28
        // L7: pos idx 29, X-Ph idx 30-31, Y-Ph idx 32-33
        let mut dssd_pulses = HashMap::new();
        let mut dssd_positions = HashMap::new();
        self.process_dssd_layer(&particle_data_dec, DetectorLayer::L1, 2, 3, 5, &mut dssd_pulses, &mut dssd_positions);
        self.process_dssd_layer(&particle_data_dec, DetectorLayer::L2, 7, 8, 10, &mut dssd_pulses, &mut dssd_positions);
        self.process_dssd_layer(&particle_data_dec, DetectorLayer::L6, 24, 25, 27, &mut dssd_pulses, &mut dssd_positions);
        self.process_dssd_layer(&particle_data_dec, DetectorLayer::L7, 29, 30, 32, &mut dssd_pulses, &mut dssd_positions);

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
            particle_time_raw,
            milliseconds,
            time,
            dssd_pulses,
            dssd_positions,
            bgo_pulses,
            packet_sync: header.packet_sync.clone(),
            package_id: header.package_id,
            packet_sequence: header.packet_sequence,
            packet_data_length: header.packet_data_length,
            data_type: header.data_type.clone(),
            line_tail: line_tail.to_vec(),
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
        positions: &mut HashMap<DetectorLayer, i32>,
    ) {
        let detect_x = (data[pos_idx] & 240) >> 4;
        let detect_y = data[pos_idx] & 15;

        let ph_x = (data[x_ph_idx] << 8) + data[x_ph_idx + 1];
        let ph_y = (data[y_ph_idx] << 8) + data[y_ph_idx + 1];
        out.insert(layer, (ph_x, ph_y));
        positions.insert(layer, data[pos_idx]);

        let layer_data = self.dssd_data.get_mut(&layer).unwrap();
        layer_data.pulse_height_x.push(ph_x as f64);
        layer_data.pulse_height_y.push(ph_y as f64);

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

/// Excludes non-positive values
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

/// The line-header fields
#[derive(Debug, Clone, Default)]
pub struct LineHeaderFields {
    pub packet_sync: String,
    pub package_id: i32,
    pub packet_sequence: i32,
    pub packet_data_length: i32,
    pub data_type: String,
}

/// Decodes the line-header fields around the timecode: Packet Sync Code, Package ID, Packet Sequence,
/// Packet Data Lengtgh, and Data Type
pub fn parse_line_header_fields(hex_data: &[String]) -> Option<LineHeaderFields> {
    if hex_data.len() < 16 {
        return None;
    }
    let hex_pair = |i: usize| format!("{}{}", hex_data[i], hex_data[i + 1]);
    let byte = |i: usize| i32::from_str_radix(&hex_data[i], 16).ok();
    let dec_pair = |i: usize| Some((byte(i)? << 8) + byte(i + 1)?);
    Some(LineHeaderFields {
        packet_sync: hex_pair(0),
        package_id: dec_pair(2)?,
        packet_sequence: dec_pair(4)?,
        packet_data_length: dec_pair(6)?,
        data_type: hex_pair(14),
    })
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


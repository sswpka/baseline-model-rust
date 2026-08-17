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
use std::io::Read;
use std::path::Path;

const HEADER_OFFSET: usize = 16;
const PARTICLE_DATA_LENGTH: usize = 34;
const PARTICLES_PER_LINE: usize = 5;
const FRAME_SIZE: usize = 256;
const DSSD_HISTOGRAM_SIZE: usize = 16384;
const BGO_HISTOGRAM_SIZE: usize = 4096;
const MODERN_PACKET_LENGTH: u16 = 0x00F7;
const LEGACY_PACKET_LENGTH: u16 = 0x002D;

#[derive(Debug, Clone, Default)]
pub struct ParticleResult {
    pub particle_number: i32,
    pub milliseconds: i32,
    /// The event's absolute time: the containing line's timecode (decoded
    /// via `get_date_time_from_hex_data`) plus this particle's own
    /// `milliseconds` offset - every particle on the same line shares the
    /// line's timecode but gets a distinct sub-second offset from it.
    pub time: DateTime<Utc>,
    /// (X, Y) pulse heights in `[L1, L2, L6, L7]` order.
    pub dssd_pulses: [(i32, i32); 4],
    /// (High gain, Low gain) pulse heights in `[L3, L4, L5]` order.
    pub bgo_pulses: [(i32, i32); 3],
    /// Line header fields surrounding the byte 8-13 timecode - see
    /// `parse_line_header_fields`. Line-level, not particle-level: every
    /// particle on the same line carries the same five values, matching how
    /// they all share the line's timecode. `packet_sync`/`data_type` stay
    /// raw hex (e.g. `"E225"`) - they're markers/type codes, not
    /// measurements; `package_id`/`packet_sequence`/`packet_data_length` are
    /// decoded to decimal, like the rest of the payload.
    pub packet_sync: String,
    pub package_id: i32,
    pub packet_sequence: i32,
    pub packet_data_length: i32,
    pub data_type: String,
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
    valid_packet_count: usize,
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
            valid_packet_count: 0,
        }
    }
}

impl ObservationDataProcessor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear_data(&mut self) {
        self.results.clear();
        self.valid_packet_count = 0;

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
        self.process_files_with_progress(file_paths, |_, _| {})
    }

    pub fn process_files_with_progress<F>(
        &mut self,
        file_paths: &[impl AsRef<Path>],
        mut progress: F,
    ) -> Result<HashMap<String, Vec<i32>>, String>
    where
        F: FnMut(u64, u64),
    {
        self.clear_data();
        let total_bytes = file_paths
            .iter()
            .map(|path| fs::metadata(path.as_ref()).map(|metadata| metadata.len()).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .sum();
        let mut processed_bytes = 0u64;
        progress(0, total_bytes);

        for path in file_paths {
            let mut file = fs::File::open(path.as_ref()).map_err(|e| e.to_string())?;
            let mut content = Vec::new();
            let mut chunk = [0u8; 64 * 1024];
            loop {
                let read = file.read(&mut chunk).map_err(|e| e.to_string())?;
                if read == 0 {
                    break;
                }
                content.extend_from_slice(&chunk[..read]);
                processed_bytes += read as u64;
                progress(processed_bytes, total_bytes);
            }
            self.process_file_bytes(&content);
        }
        progress(total_bytes, total_bytes);
        Ok(self.get_histogram_data())
    }

    pub fn valid_packet_count(&self) -> usize {
        self.valid_packet_count
    }

    pub fn raw_histogram_data(&self) -> HashMap<String, Vec<i32>> {
        self.build_histogram_data(true)
    }

    fn process_file_bytes(&mut self, content: &[u8]) {
        let nibbles = normalize_hex_nibbles(content);
        let mut offset = 0usize;
        let frame_nibbles = FRAME_SIZE * 2;
        while offset + HEADER_OFFSET * 2 <= nibbles.len() {
            if !valid_header_nibbles(&nibbles, offset) {
                offset += 1;
                continue;
            }
            let candidate_end = (offset + frame_nibbles).min(nibbles.len());
            if let Some(next_header) = (offset + 1..candidate_end)
                .find(|&candidate| valid_header_nibbles(&nibbles, candidate))
            {
                offset = next_header;
                continue;
            }
            let Some(frame) = frame_from_nibbles(&nibbles, offset) else {
                offset += 1;
                continue;
            };
            let Some(header) = parse_header_bytes(&frame) else {
                offset += 1;
                continue;
            };
            let particles = (0..PARTICLES_PER_LINE)
                .map(|index| {
                    let start = HEADER_OFFSET + PARTICLE_DATA_LENGTH * index;
                    &frame[start..start + PARTICLE_DATA_LENGTH]
                })
                .collect::<Vec<_>>();
            if particles.iter().any(|particle| !valid_detector_nibbles(particle)) {
                offset += 1;
                continue;
            }

            let line_time = get_date_time_from_bytes(&frame).unwrap_or_else(|_| observation_epoch());
            let mut decoded = Vec::with_capacity(PARTICLES_PER_LINE);
            let mut valid = true;
            for (index, particle) in particles.iter().enumerate() {
                match self.process_particle_bytes(particle, index, line_time, &header) {
                    Ok(result) => decoded.push(result),
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid && decoded.len() == PARTICLES_PER_LINE {
                self.results.extend(decoded);
                self.valid_packet_count += 1;
                offset += frame_nibbles;
            } else {
                offset += 1;
            }
        }
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

    /// Builds fixed-size count histograms for every
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
    /// `0` in the layers they didn't hit. The normal main X/Y and BGO
    /// histograms drop those zeros; `raw_histogram_data` retains them for
    /// Observation-only preprocessing. Strip histograms always retain zeros.
    fn get_histogram_data(&self) -> HashMap<String, Vec<i32>> {
        self.build_histogram_data(false)
    }

    fn build_histogram_data(&self, retain_zero_main_values: bool) -> HashMap<String, Vec<i32>> {
        let mut result = HashMap::new();

        for layer in [DetectorLayer::L1, DetectorLayer::L2, DetectorLayer::L6, DetectorLayer::L7] {
            let data = &self.dssd_data[&layer];
            let name = layer_name(layer);

            result.insert(
                format!("DSSD{name}_X"),
                histogram_of_values(&data.pulse_height_x, DSSD_HISTOGRAM_SIZE, !retain_zero_main_values),
            );
            result.insert(
                format!("DSSD{name}_Y"),
                histogram_of_values(&data.pulse_height_y, DSSD_HISTOGRAM_SIZE, !retain_zero_main_values),
            );

            for strip in 1..=8 {
                let empty = Vec::new();
                let strip_x = data.strip_x.get(&strip).unwrap_or(&empty);
                let strip_y = data.strip_y.get(&strip).unwrap_or(&empty);
                result.insert(
                    format!("DSSD{name}_StripX{strip}"),
                    histogram_of_i32(strip_x, DSSD_HISTOGRAM_SIZE),
                );
                result.insert(
                    format!("DSSD{name}_StripY{strip}"),
                    histogram_of_i32(strip_y, DSSD_HISTOGRAM_SIZE),
                );
            }
        }

        for layer in [BgoLayer::L3, BgoLayer::L4, BgoLayer::L5] {
            let data = &self.bgo_data[&layer];
            let name = bgo_layer_name(layer);
            result.insert(
                format!("BGO{name}_High"),
                histogram_of_values(&data.high_gain, BGO_HISTOGRAM_SIZE, !retain_zero_main_values),
            );
            result.insert(
                format!("BGO{name}_Low"),
                histogram_of_values(&data.low_gain, BGO_HISTOGRAM_SIZE, !retain_zero_main_values),
            );
        }

        result
    }

    pub fn process_particles(&mut self, hex_data: &[String], line_time: DateTime<Utc>, header: &LineHeaderFields) {
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

            if let Ok(processed) = self.process_particle_data(particle_data, i, line_time, header) {
                self.results.push(processed);
            }
        }
    }

    pub fn process_particle_data(&mut self, particle_data: &[String], i: usize, line_time: DateTime<Utc>, header: &LineHeaderFields) -> Result<ParticleResult, String> {
        if particle_data.len() != PARTICLE_DATA_LENGTH {
            return Err("Particle data must contain exactly 34 bytes".to_string());
        }
        let mut particle_data_bytes = [0u8; PARTICLE_DATA_LENGTH];
        for (index, hex) in particle_data.iter().enumerate() {
            particle_data_bytes[index] = u8::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
        }
        self.process_particle_bytes(&particle_data_bytes, i, line_time, header)
    }

    fn process_particle_bytes(&mut self, particle_data: &[u8], i: usize, line_time: DateTime<Utc>, header: &LineHeaderFields) -> Result<ParticleResult, String> {
        if particle_data.len() != PARTICLE_DATA_LENGTH {
            return Err("Particle data must contain exactly 34 bytes".to_string());
        }
        if !valid_detector_nibbles(particle_data) {
            return Err("Invalid detector nibble".to_string());
        }
        let milliseconds = (((particle_data[0] as i32) << 8) + particle_data[1] as i32) / 1000;
        let time = line_time + chrono::Duration::milliseconds(milliseconds as i64);

        // --- DSSD layers ---
        // L1: pos idx 2, X-Ph idx 3-4, Y-Ph idx 5-6
        // L2: pos idx 7, X-Ph idx 8-9, Y-Ph idx 10-11
        // L6: pos idx 24, X-Ph idx 25-26, Y-Ph idx 27-28
        // L7: pos idx 29, X-Ph idx 30-31, Y-Ph idx 32-33
        let dssd_pulses = [
            self.process_dssd_layer(particle_data, DetectorLayer::L1, 2, 3, 5),
            self.process_dssd_layer(particle_data, DetectorLayer::L2, 7, 8, 10),
            self.process_dssd_layer(particle_data, DetectorLayer::L6, 24, 25, 27),
            self.process_dssd_layer(particle_data, DetectorLayer::L7, 29, 30, 32),
        ];

        // --- BGO layers ---
        // L3: H idx 12-13, L idx 14-15
        // L4: H idx 16-17, L idx 18-19
        // L5: H idx 20-21, L idx 22-23
        let bgo_pulses = [
            self.process_bgo_layer(particle_data, BgoLayer::L3, 12, 14)?,
            self.process_bgo_layer(particle_data, BgoLayer::L4, 16, 18)?,
            self.process_bgo_layer(particle_data, BgoLayer::L5, 20, 22)?,
        ];

        Ok(ParticleResult {
            particle_number: i as i32 + 1,
            milliseconds,
            time,
            dssd_pulses,
            bgo_pulses,
            packet_sync: header.packet_sync.clone(),
            package_id: header.package_id,
            packet_sequence: header.packet_sequence,
            packet_data_length: header.packet_data_length,
            data_type: header.data_type.clone(),
        })
    }

    fn process_dssd_layer(
        &mut self,
        data: &[u8],
        layer: DetectorLayer,
        pos_idx: usize,
        x_ph_idx: usize,
        y_ph_idx: usize,
    ) -> (i32, i32) {
        let detect_x = data[pos_idx] >> 4; // upper 4 bits
        let detect_y = data[pos_idx] & 15; // lower 4 bits

        let ph_x = ((data[x_ph_idx] as i32) << 8) + data[x_ph_idx + 1] as i32;
        let ph_y = ((data[y_ph_idx] as i32) << 8) + data[y_ph_idx + 1] as i32;

        let layer_data = self.dssd_data.get_mut(&layer).unwrap();
        layer_data.pulse_height_x.push(ph_x as f64);
        layer_data.pulse_height_y.push(ph_y as f64);

        // Strip key mapping (see original comments): detect==8 -> strip 0,
        // otherwise detect+1 -> strip (detect+1).
        let target_strip_x = if detect_x == 8 { 0 } else { (detect_x + 1) as i32 };
        let target_strip_y = if detect_y == 8 { 0 } else { (detect_y + 1) as i32 };

        if let Some(v) = layer_data.strip_x.get_mut(&target_strip_x) {
            v.push(ph_x);
        }
        if let Some(v) = layer_data.strip_y.get_mut(&target_strip_y) {
            v.push(ph_y);
        }

        (ph_x, ph_y)
    }

    fn process_bgo_layer(
        &mut self,
        data: &[u8],
        layer: BgoLayer,
        h_idx: usize,
        l_idx: usize,
    ) -> Result<(i32, i32), String> {
        let ph_h = ((data[h_idx] as i32) << 8) + data[h_idx + 1] as i32;
        let ph_l = ((data[l_idx] as i32) << 8) + data[l_idx + 1] as i32;

        if ph_h < 0 || ph_l < 0 {
            return Err("Pulse heights must be non-negative.".to_string());
        }

        let bgo = self.bgo_data.get_mut(&layer).unwrap();
        bgo.high_gain.push(ph_h as f64);
        bgo.low_gain.push(ph_l as f64);

        Ok((ph_h, ph_l))
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

fn histogram_of_values(values: &[f64], size: usize, positive_only: bool) -> Vec<i32> {
    let mut hist = vec![0i32; size];
    for &value in values {
        if !value.is_finite() || value < 0.0 {
            continue;
        }
        let idx = value as i64;
        if (!positive_only || idx > 0) && (idx as usize) < size {
            hist[idx as usize] += 1;
        }
    }
    hist
}

fn histogram_of_i32(values: &[i32], size: usize) -> Vec<i32> {
    let mut hist = vec![0i32; size];
    for &value in values {
        if value >= 0 && (value as usize) < size {
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

/// The line-header fields surrounding the byte 8-13 timecode - see
/// `parse_line_header_fields`. `packet_sync`/`data_type` stay raw hex (e.g.
/// `"E225"`) since they're a fixed marker and a type code, not measurements;
/// the rest decode to decimal like the rest of the payload.
#[derive(Debug, Clone, Default)]
pub struct LineHeaderFields {
    pub packet_sync: String,
    pub package_id: i32,
    pub packet_sequence: i32,
    pub packet_data_length: i32,
    pub data_type: String,
}

/// Decodes the line-header fields around the timecode: Packet Sync Code
/// (bytes 0-1, raw hex), Package ID (2-3, decimal), Packet Sequence (4-5,
/// decimal), Packet Data Length (6-7, decimal), the byte 8-13 timecode
/// itself (`get_date_time_from_hex_data`, decoded separately), then Data
/// Type (14-15, raw hex) - mirrors the identical 16-byte header layout
/// `flux::processing::process_header_internal` decodes for its own (much
/// larger) E225 packets.
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

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn normalize_hex_nibbles(content: &[u8]) -> Vec<Option<u8>> {
    let mut nibbles = Vec::with_capacity(content.len());
    for &byte in content {
        if byte.is_ascii_whitespace() {
            continue;
        }
        nibbles.push(hex_nibble(byte));
    }
    nibbles
}

fn frame_from_nibbles(nibbles: &[Option<u8>], offset: usize) -> Option<Vec<u8>> {
    let end = offset.checked_add(FRAME_SIZE * 2)?;
    let source = nibbles.get(offset..end)?;
    let mut frame = Vec::with_capacity(FRAME_SIZE);
    for pair in source.chunks_exact(2) {
        frame.push((pair[0]? << 4) | pair[1]?);
    }
    Some(frame)
}

fn parse_header_bytes(bytes: &[u8]) -> Option<LineHeaderFields> {
    let header = bytes.get(..HEADER_OFFSET)?;
    if !valid_header_bytes(bytes) {
        return None;
    }
    let packet_data_length = u16::from_be_bytes([header[6], header[7]]);
    Some(LineHeaderFields {
        packet_sync: "E225".to_string(),
        package_id: u16::from_be_bytes([header[2], header[3]]) as i32,
        packet_sequence: u16::from_be_bytes([header[4], header[5]]) as i32,
        packet_data_length: packet_data_length as i32,
        data_type: "00AD".to_string(),
    })
}

fn valid_header_nibbles(nibbles: &[Option<u8>], offset: usize) -> bool {
    let Some(header) = nibbles.get(offset..offset + HEADER_OFFSET * 2) else {
        return false;
    };
    if !header.iter().all(Option::is_some) {
        return false;
    }
    let packet_length_high = header[12].unwrap();
    let packet_length_high_low = header[13].unwrap();
    let packet_length_low = header[14].unwrap();
    let packet_length_low_low = header[15].unwrap();
    header[0] == Some(0xE)
        && header[1] == Some(0x2)
        && header[2] == Some(0x2)
        && header[3] == Some(0x5)
        && matches!(
            u16::from_be_bytes([
                (packet_length_high << 4) | packet_length_high_low,
                (packet_length_low << 4) | packet_length_low_low,
            ]),
            MODERN_PACKET_LENGTH | LEGACY_PACKET_LENGTH
        )
        && header[28] == Some(0x0)
        && header[29] == Some(0x0)
        && header[30] == Some(0xA)
        && header[31] == Some(0xD)
}

fn valid_header_bytes(bytes: &[u8]) -> bool {
    let Some(header) = bytes.get(..HEADER_OFFSET) else {
        return false;
    };
    header[0] == 0xE2
        && header[1] == 0x25
        && matches!(
            u16::from_be_bytes([header[6], header[7]]),
            MODERN_PACKET_LENGTH | LEGACY_PACKET_LENGTH
        )
        && u16::from_be_bytes([header[14], header[15]]) == 0x00AD
}

fn get_date_time_from_bytes(frame: &[u8]) -> Result<DateTime<Utc>, String> {
    let timecode = frame.get(8..14).ok_or_else(|| "insufficient frame timecode".to_string())?;
    let seconds = u32::from_be_bytes([timecode[0], timecode[1], timecode[2], timecode[3]]);
    let milliseconds = u16::from_be_bytes([timecode[4], timecode[5]]);
    Ok(observation_epoch()
        + chrono::Duration::seconds(seconds as i64)
        + chrono::Duration::milliseconds(milliseconds as i64))
}

fn valid_detector_nibbles(particle: &[u8]) -> bool {
    if particle.len() != PARTICLE_DATA_LENGTH {
        return false;
    }
    [2usize, 7, 24, 29].iter().all(|&index| {
        let detector = particle[index];
        detector >> 4 <= 8 && detector & 0x0F <= 8
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn set_u16(bytes: &mut [u8], index: usize, value: u16) {
        let [high, low] = value.to_be_bytes();
        bytes[index] = high;
        bytes[index + 1] = low;
    }

    fn packet_frame(
        package_id: u16,
        packet_data_length: u16,
        data_type: u16,
        invalid_particle: Option<usize>,
    ) -> Vec<u8> {
        let mut bytes = vec![0u8; FRAME_SIZE];
        bytes[0] = 0xE2;
        bytes[1] = 0x25;
        set_u16(&mut bytes, 2, package_id);
        set_u16(&mut bytes, 4, 1);
        set_u16(&mut bytes, 6, packet_data_length);
        set_u16(&mut bytes, 14, data_type);

        for particle in 0..PARTICLES_PER_LINE {
            let start = HEADER_OFFSET + particle * PARTICLE_DATA_LENGTH;
            bytes[start + 2] = 0x11;
            bytes[start + 7] = 0x11;
            bytes[start + 24] = 0x11;
            bytes[start + 29] = 0x11;
            set_u16(&mut bytes, start + 3, 100 + particle as u16);
            set_u16(&mut bytes, start + 5, 200 + particle as u16);
            set_u16(&mut bytes, start + 12, 4095);
            set_u16(&mut bytes, start + 14, 100);
            set_u16(&mut bytes, start + 16, 300);
            set_u16(&mut bytes, start + 18, 100);
            set_u16(&mut bytes, start + 20, 500);
            set_u16(&mut bytes, start + 22, 100);
        }
        if let Some(particle) = invalid_particle {
            bytes[HEADER_OFFSET + particle * PARTICLE_DATA_LENGTH + 2] = 0x99;
        }
        bytes
    }

    fn hex_text(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02X}")).collect()
    }

    fn nibble_spaced_hex_text(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|byte| format!("{:X} \n {:X} ", byte >> 4, byte & 0x0F))
            .collect()
    }

    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "baseline_observation_{label}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn process_files_keeps_first_packet_and_file_order() {
        let first_path = temp_path("order_a");
        let second_path = temp_path("order_b");
        let first = [
            packet_frame(1, MODERN_PACKET_LENGTH, 0x00AD, None),
            packet_frame(2, LEGACY_PACKET_LENGTH, 0x00AD, None),
        ]
        .concat();
        let second = packet_frame(3, MODERN_PACKET_LENGTH, 0x00AD, None);
        fs::write(&first_path, format!("{}\n", hex_text(&first))).unwrap();
        fs::write(&second_path, nibble_spaced_hex_text(&second)).unwrap();

        let mut processor = ObservationDataProcessor::new();
        let result = processor.process_files(&[&first_path, &second_path]);
        let _ = fs::remove_file(&first_path);
        let _ = fs::remove_file(&second_path);
        result.unwrap();

        assert_eq!(processor.results.len(), PARTICLES_PER_LINE * 3);
        assert_eq!(
            processor
                .results
                .iter()
                .map(|result| result.package_id)
                .collect::<Vec<_>>(),
            vec![1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3]
        );
        assert_eq!(
            processor.results[0].dssd_pulses,
            [(100, 200), (0, 0), (0, 0), (0, 0)]
        );
        assert_eq!(
            processor.results[0].bgo_pulses,
            [(4095, 100), (300, 100), (500, 100)]
        );
    }

    #[test]
    fn odd_nibble_prefix_does_not_shift_later_frame() {
        let path = temp_path("odd_nibble");
        let valid = packet_frame(9, MODERN_PACKET_LENGTH, 0x00AD, None);
        fs::write(&path, format!("A E2\nnot-hex\n{}", hex_text(&valid))).unwrap();

        let mut processor = ObservationDataProcessor::new();
        processor.process_files(&[&path]).unwrap();
        let _ = fs::remove_file(path);

        assert_eq!(processor.valid_packet_count(), 1);
        assert!(processor.results.iter().all(|result| result.package_id == 9));
    }

    #[test]
    fn malformed_and_truncated_frames_resync_without_cross_file_join() {
        let resync_path = temp_path("resync");
        let mut malformed = packet_frame(10, MODERN_PACKET_LENGTH, 0x00AD, None);
        malformed.truncate(40);
        let valid = packet_frame(11, MODERN_PACKET_LENGTH, 0x00AD, None);
        let mut content = malformed;
        content.extend_from_slice(&valid);
        fs::write(&resync_path, hex_text(&content)).unwrap();

        let truncated_path = temp_path("truncated");
        fs::write(&truncated_path, hex_text(&valid[..40])).unwrap();

        let first_half_path = temp_path("first_half");
        let second_half_path = temp_path("second_half");
        fs::write(&first_half_path, hex_text(&valid[..100])).unwrap();
        fs::write(&second_half_path, hex_text(&valid[100..])).unwrap();

        let mut processor = ObservationDataProcessor::new();
        processor
            .process_files(&[&resync_path])
            .expect("resync input should be readable");
        assert_eq!(processor.valid_packet_count(), 1);
        assert!(processor.results.iter().all(|result| result.package_id == 11));

        processor
            .process_files(&[&truncated_path])
            .expect("truncated input should be readable");
        assert!(processor.results.is_empty());

        processor
            .process_files(&[&first_half_path, &second_half_path])
            .expect("split files should be readable");
        assert!(processor.results.is_empty());

        for path in [resync_path, truncated_path, first_half_path, second_half_path] {
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn invalid_type_length_and_detector_are_rejected_without_partial_results() {
        let path = temp_path("invalid");
        let invalid_type = packet_frame(1, MODERN_PACKET_LENGTH, 0x00AE, None);
        let invalid_length = packet_frame(2, 0x1234, 0x00AD, None);
        let invalid_detector = packet_frame(3, MODERN_PACKET_LENGTH, 0x00AD, Some(2));
        let valid = packet_frame(4, LEGACY_PACKET_LENGTH, 0x00AD, None);
        let content = [invalid_type, invalid_length, invalid_detector, valid].concat();
        fs::write(&path, hex_text(&content)).unwrap();

        let mut processor = ObservationDataProcessor::new();
        processor.process_files(&[&path]).unwrap();
        let _ = fs::remove_file(path);

        assert_eq!(processor.valid_packet_count(), 1);
        assert_eq!(processor.results.len(), PARTICLES_PER_LINE);
        assert!(processor.results.iter().all(|result| result.package_id == 4));
    }

    #[test]
    fn histograms_use_dssd_and_bgo_channel_domains() {
        let path = temp_path("histogram");
        let frame = packet_frame(1, MODERN_PACKET_LENGTH, 0x00AD, None);
        fs::write(&path, hex_text(&frame)).unwrap();

        let mut processor = ObservationDataProcessor::new();
        let histograms = processor.process_files(&[&path]).unwrap();
        let raw = processor.raw_histogram_data();
        let _ = fs::remove_file(path);

        assert_eq!(histograms["DSSDL1_X"].len(), DSSD_HISTOGRAM_SIZE);
        assert_eq!(histograms["BGOL3_High"].len(), BGO_HISTOGRAM_SIZE);
        assert_eq!(
            histograms["DSSDL1_X"][100..105].iter().sum::<i32>(),
            PARTICLES_PER_LINE as i32
        );
        assert_eq!(histograms["BGOL3_High"][4095], PARTICLES_PER_LINE as i32);
        assert_eq!(raw["DSSDL1_X"][0], 0);
        assert_eq!(raw["BGOL3_High"][0], 0);
    }

    #[test]
    fn byte_progress_is_monotonic_and_reaches_total() {
        let path = temp_path("progress");
        let frame = packet_frame(1, MODERN_PACKET_LENGTH, 0x00AD, None);
        let content = frame.repeat(300);
        fs::write(&path, hex_text(&content)).unwrap();

        let mut progress_values = Vec::new();
        let mut processor = ObservationDataProcessor::new();
        processor
            .process_files_with_progress(&[&path], |processed, total| progress_values.push((processed, total)))
            .unwrap();
        let _ = fs::remove_file(path);

        assert!(progress_values.len() >= 3);
        assert!(progress_values.windows(2).all(|window| window[0].0 <= window[1].0));
        let (_, total) = *progress_values.last().unwrap();
        assert_eq!(progress_values.last().unwrap().0, total);
        assert!(progress_values.iter().any(|&(processed, total)| processed > 0 && processed < total));
    }

}

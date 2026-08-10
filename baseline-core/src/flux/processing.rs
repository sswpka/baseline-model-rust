//! Direct transcription of the pure parsing/calculation logic in
//! `Presentation/ViewModels/Flux/FluxViewModel.DataProcessing.cs`. UI plot
//! rendering (`UpdateAllPlots`) is not part of this port; that lives in the
//! egui app layer.

use crate::models::flux::FluxDataResult;
use chrono::{DateTime, Utc};

/// `seconds_part` + `milliseconds_part` behave like .NET's
/// `DateTimeOffset.FromUnixTimeSeconds(seconds).AddMilliseconds(ms)`: the
/// milliseconds are added as a precise offset (carrying into seconds as
/// needed), not truncated.
fn unix_from_seconds_and_millis(seconds_part: u32, milliseconds_part: u16) -> DateTime<Utc> {
    let total_ms = seconds_part as i64 * 1000 + milliseconds_part as i64;
    let total_secs = total_ms.div_euclid(1000);
    let remainder_ms = total_ms.rem_euclid(1000);
    DateTime::from_timestamp(total_secs, (remainder_ms * 1_000_000) as u32)
        .unwrap_or(DateTime::from_timestamp(0, 0).unwrap())
}

/// Accumulates parsed flux packets across a session, mirroring the private
/// list fields on `FluxViewModel`.
#[derive(Debug, Clone, Default)]
pub struct FluxAccumulator {
    pub seconds_part_list: Vec<f64>,
    pub particle_counting_lists: [Vec<f64>; 7],
    pub particle_layer_list: Vec<[f64; 1000]>,
    pub particle_offset_time_list: Vec<[f64; 1000]>,
    pub all_results: Vec<FluxDataResult>,
}

impl FluxAccumulator {
    pub fn reset(&mut self) {
        self.seconds_part_list.clear();
        for l in &mut self.particle_counting_lists {
            l.clear();
        }
        self.particle_layer_list.clear();
        self.particle_offset_time_list.clear();
        self.all_results.clear();
    }

    /// Parses one flux-observation hex packet and appends it, matching
    /// `ProcessFluxObservation`. Returns `false` (no-op) for undersized input.
    pub fn process_flux_observation(&mut self, hex_string: &str) -> bool {
        if hex_string.len() < 4064 {
            return false;
        }
        let hex = hex_string.as_bytes();

        let t0 = match parse_hex_byte(hex, 32) {
            Some(v) => v,
            None => return false,
        };
        let t1 = match parse_hex_byte(hex, 34) {
            Some(v) => v,
            None => return false,
        };
        let milliseconds = ((t0 as u32) << 8) + t1 as u32;
        let time_seconds = milliseconds as f64 / 1000.0;

        let mut particle_counting = [0.0f64; 7];
        for (i, slot) in particle_counting.iter_mut().enumerate() {
            let idx = 36 + i * 4;
            let p0 = match parse_hex_byte(hex, idx) {
                Some(v) => v,
                None => return false,
            };
            let p1 = match parse_hex_byte(hex, idx + 2) {
                Some(v) => v,
                None => return false,
            };
            *slot = ((p0 as u32) << 8) as f64 + p1 as f64;
        }

        let mut particle_layer = [0.0f64; 1000];
        let mut particle_offset_time = [0.0f64; 1000];
        for i in 0..1000 {
            let idx = 64 + i * 4;
            let high_byte = match parse_hex_byte(hex, idx) {
                Some(v) => v,
                None => return false,
            };
            let low_byte = match parse_hex_byte(hex, idx + 2) {
                Some(v) => v,
                None => return false,
            };
            let value = ((high_byte as u32) << 8) | low_byte as u32;
            particle_layer[i] = ((value >> 13) & 0x07) as f64;
            particle_offset_time[i] = (value & 0x0FFF) as f64;
        }

        self.seconds_part_list.push(time_seconds);
        for i in 0..7 {
            self.particle_counting_lists[i].push(particle_counting[i]);
        }
        self.particle_layer_list.push(particle_layer);
        self.particle_offset_time_list.push(particle_offset_time);
        self.all_results.push(FluxDataResult {
            time_seconds,
            particle_counting,
            particle_layer: particle_layer.to_vec(),
            particle_offset_time: particle_offset_time.to_vec(),
        });
        true
    }

    /// Per-layer (x, y, stats text) flux points, matching `CalculateAndPlotFlux`.
    /// Returns `None` if no data has been accumulated yet.
    pub fn calculate_flux(&self, layer_count: usize, detector_area_m2: f64) -> Option<Vec<FluxLayerPlot>> {
        let count = self.seconds_part_list.len();
        if count == 0 {
            return None;
        }

        let mut cumulative_time = vec![0.0; count];
        cumulative_time[0] = self.seconds_part_list[0];
        for i in 1..count {
            cumulative_time[i] = cumulative_time[i - 1] + self.seconds_part_list[i];
        }

        let mut out = Vec::with_capacity(layer_count);
        for layer in 0..layer_count {
            let particle_counting = &self.particle_counting_lists[layer];
            let mut x_points = Vec::with_capacity(count);
            let mut y_points = Vec::with_capacity(count);
            let mut max_flux = 0.0f64;

            for j in 0..count {
                let t = self.seconds_part_list[j];
                let flux = if t > 0.0 { particle_counting[j] / (t * detector_area_m2) } else { 0.0 };

                if !flux.is_nan() && !flux.is_infinite() {
                    x_points.push(cumulative_time[j]);
                    y_points.push(flux);
                    if flux > max_flux {
                        max_flux = flux;
                    }
                }
            }

            let stats_text = if !x_points.is_empty() {
                format!("Points: {} | Max: {:.2}", x_points.len(), max_flux)
            } else {
                "No valid flux data".to_string()
            };

            out.push(FluxLayerPlot {
                x_data: if x_points.is_empty() { None } else { Some(x_points) },
                y_data: if y_points.is_empty() { None } else { Some(y_points) },
                stats_text,
            });
        }

        Some(out)
    }
}

#[derive(Debug, Clone)]
pub struct FluxLayerPlot {
    pub x_data: Option<Vec<f64>>,
    pub y_data: Option<Vec<f64>>,
    pub stats_text: String,
}

/// Matches the standalone `GetDateTimeFromHexData(string)` in
/// `FluxViewModel.DataProcessing.cs` (distinct from the Observation-mode
/// version in `observation::data_processor`, which truncates milliseconds;
/// this one adds them as a precise offset via `AddMilliseconds`).
pub fn get_date_time_from_hex(hex_string: &str) -> DateTime<Utc> {
    let hex = hex_string.as_bytes();
    let epoch = || DateTime::from_timestamp(0, 0).unwrap();
    if hex.len() < 28 {
        return epoch();
    }

    let mut timecode = [0u8; 6];
    for (i, slot) in timecode.iter_mut().enumerate() {
        match parse_hex_byte(hex, 16 + i * 2) {
            Some(v) => *slot = v,
            None => return epoch(),
        }
    }

    let seconds_part = u32::from_be_bytes([timecode[0], timecode[1], timecode[2], timecode[3]]);
    let milliseconds_part = u16::from_be_bytes([timecode[4], timecode[5]]);

    unix_from_seconds_and_millis(seconds_part, milliseconds_part)
}

#[derive(Debug, Clone)]
pub struct HeaderParseResult {
    pub packet_sync: String,
    pub package_id: String,
    pub packet_seq: String,
    pub packet_data_len: String,
    pub timestamp: String,
    pub data_type: String,
    pub check_sum_hex: String,
    pub checksum_matches: bool,
}

/// Matches `ProcessHeaderInternal`. Returns `None` for undersized input
/// (matching the C# early-return that leaves `HeaderInfo` unchanged).
pub fn process_header_internal(hex_data: &[String]) -> Option<HeaderParseResult> {
    if hex_data.len() < 2064 {
        return None;
    }

    let packet_sync = format!("Packet Synchronization Code: {} {}", hex_data[0], hex_data[1]);
    let package_id = format!("Package Identification: {} {}", hex_data[2], hex_data[3]);
    let packet_seq = format!("Packet Sequence: {} {}", hex_data[4], hex_data[5]);
    let packet_data_len = format!("Packet data length: {} {}", hex_data[6], hex_data[7]);

    let timecode: Vec<u8> = hex_data[8..14]
        .iter()
        .map(|h| u8::from_str_radix(h, 16).unwrap_or(0))
        .collect();
    let sec_part = u32::from_be_bytes([timecode[0], timecode[1], timecode[2], timecode[3]]);
    let ms_part = u16::from_be_bytes([timecode[4], timecode[5]]);
    let dt = unix_from_seconds_and_millis(sec_part, ms_part);
    let timestamp = format!("Timestamp: {}", dt.format("%Y-%b-%d %H:%M:%S%.3f"));

    let data_type = format!("Data Type: {} {}", hex_data[14], hex_data[15]);
    let check_sum_hex = format!("Check Sum: {} {}", hex_data[2062], hex_data[2063]);

    let total_sum: i64 = hex_data[8..8 + 2054]
        .iter()
        .map(|h| i64::from_str_radix(h, 16).unwrap_or(0))
        .sum();
    let last_two_bytes = total_sum.rem_euclid(65536);
    let checksum_calc = format!("{:04X}", last_two_bytes);
    let checksum_from_data = format!("{}{}", hex_data[2062], hex_data[2063]);
    let checksum_matches = checksum_calc.eq_ignore_ascii_case(&checksum_from_data);

    Some(HeaderParseResult {
        packet_sync,
        package_id,
        packet_seq,
        packet_data_len,
        timestamp,
        data_type,
        check_sum_hex,
        checksum_matches,
    })
}

#[inline]
fn parse_hex_byte(hex: &[u8], char_offset: usize) -> Option<u8> {
    if char_offset + 2 > hex.len() {
        return None;
    }
    let s = std::str::from_utf8(&hex[char_offset..char_offset + 2]).ok()?;
    u8::from_str_radix(s, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_flux_observation_rejects_undersized_input() {
        let mut acc = FluxAccumulator::default();
        assert!(!acc.process_flux_observation("00"));
        assert!(acc.all_results.is_empty());
    }

    #[test]
    fn process_flux_observation_parses_minimal_valid_packet() {
        let mut acc = FluxAccumulator::default();
        let hex = "0".repeat(4064);
        assert!(acc.process_flux_observation(&hex));
        assert_eq!(acc.all_results.len(), 1);
        assert_eq!(acc.all_results[0].time_seconds, 0.0);
    }

    #[test]
    fn process_header_internal_rejects_undersized_input() {
        let hex_data = vec!["00".to_string(); 10];
        assert!(process_header_internal(&hex_data).is_none());
    }
}

//! Data Table decoding for Flux mode, per `flux.md`.

use crate::observation::data_processor::{calibrated_value, get_date_time_from_hex_data, parse_line_header_fields};
use chrono::{DateTime, Utc};

/// One decoded tail field (Byte 2032-2055), 2 bytes each.
pub struct FluxTailField {
    pub label: &'static str,
    pub byte_start: usize,
}

const fn tail(label: &'static str, byte_start: usize) -> FluxTailField {
    FluxTailField { label, byte_start }
}

/// Byte 2032-2055: 12 fields, 2 bytes each.
pub const FLUX_TAIL_FIELDS: &[FluxTailField] = &[
    tail("DSSD 1 Temperature", 2032),
    tail("FEE1 Current", 2034),
    tail("FEE1 Temperature", 2036),
    tail("DSSD 7 Temperature", 2038),
    tail("FEE2 Current", 2040),
    tail("FEE2 Temperature", 2042),
    tail("FEE1 Threshold", 2044),
    tail("FEE2 Threshold", 2046),
    tail("BGO1 Bias Voltage", 2048),
    tail("BGO2 Bias Voltage", 2050),
    tail("BGO3 Bias Voltage", 2052),
    tail("BGO2 Temperature", 2054),
];

const PARTICLE_TIME_START: usize = 16;
const PARTICLE_COUNTS_START: usize = 18;
/// Particle Counts L1-L7 (Byte 18-31), one per layer.
pub const PARTICLE_COUNTS_LAYERS: usize = 7;
const PARTICLE_INFO_START: usize = 32;
/// Particle Info (Byte 32-2031): 1000 decoded decimal values, 2 bytes each.
pub const PARTICLE_INFO_COUNT: usize = 1000;

const RESERVED_START: usize = 2056;
const RESERVED_LEN: usize = 6; // Byte 2056-2061
const CHECKSUM_START: usize = 2062;
const CHECKSUM_LEN: usize = 2; // Byte 2062-2063

/// One decoded raw line for the Flux mode Data Table tab.
#[derive(Debug, Clone, Default)]
pub struct FluxLineResult {
    pub packet_sync: String,
    pub package_id: i32,
    pub packet_sequence: i32,
    pub packet_data_length: i32,
    pub time: DateTime<Utc>,
    pub data_type: String,
    pub particle_time: i32,
    /// Particle Counts L1-L7 (Byte 18-31), in that order.
    pub particle_counts: Vec<i32>,
    /// Particle Info (Byte 32-2031): `PARTICLE_INFO_COUNT` decoded decimal values.
    pub particle_info: Vec<String>,
    /// Decoded values for `FLUX_TAIL_FIELDS`, in that same order.
    pub tail: Vec<String>,
    pub reserved_hex: String,
    pub checksum_hex: String,
}

fn byte(hex_data: &[String], i: usize) -> i32 {
    hex_data.get(i).and_then(|h| i32::from_str_radix(h, 16).ok()).unwrap_or(0)
}

fn dec_pair(hex_data: &[String], i: usize) -> i32 {
    (byte(hex_data, i) << 8) + byte(hex_data, i + 1)
}

fn hex_span(hex_data: &[String], start: usize, len: usize) -> String {
    (start..start + len).map(|i| hex_data.get(i).cloned().unwrap_or_else(|| "00".to_string())).collect()
}

/// Decodes one raw flux line into a Data Table row, per `flux.md`.
pub fn parse_flux_line(hex_data: &[String]) -> Option<FluxLineResult> {
    if hex_data.len() < CHECKSUM_START + CHECKSUM_LEN {
        return None;
    }
    let header = parse_line_header_fields(hex_data)?;
    let time = get_date_time_from_hex_data(hex_data).ok()?;
    let particle_time = dec_pair(hex_data, PARTICLE_TIME_START);
    let particle_counts = (0..PARTICLE_COUNTS_LAYERS)
        .map(|i| dec_pair(hex_data, PARTICLE_COUNTS_START + i * 2))
        .collect();
    let particle_info = (0..PARTICLE_INFO_COUNT)
        .map(|i| dec_pair(hex_data, PARTICLE_INFO_START + i * 2).to_string())
        .collect();

    let tail = FLUX_TAIL_FIELDS
        .iter()
        .map(|f| {
            let raw = dec_pair(hex_data, f.byte_start);
            match calibrated_value(f.label, raw as f64) {
                Some(value) => format!("{value:.4}"),
                None => raw.to_string(),
            }
        })
        .collect();

    Some(FluxLineResult {
        packet_sync: header.packet_sync,
        package_id: header.package_id,
        packet_sequence: header.packet_sequence,
        packet_data_length: header.packet_data_length,
        time,
        data_type: header.data_type,
        particle_time,
        particle_counts,
        particle_info,
        tail,
        reserved_hex: hex_span(hex_data, RESERVED_START, RESERVED_LEN),
        checksum_hex: hex_span(hex_data, CHECKSUM_START, CHECKSUM_LEN),
    })
}

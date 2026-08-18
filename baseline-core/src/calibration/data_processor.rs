//! Data Table decoding for Calibration mode

use crate::observation::data_processor::{formula_decode, get_date_time_from_hex_data, parse_line_header_fields};
use chrono::{DateTime, Utc};

/// One decoded tail field (Byte 1426-1445), 2 bytes each.
pub struct CalibrationTailField {
    pub label: &'static str,
    pub byte_start: usize,
}

const fn tail(label: &'static str, byte_start: usize) -> CalibrationTailField {
    CalibrationTailField { label, byte_start }
}

/// Byte 1426-1445: 10 fields, 2 bytes each.
pub const CALIBRATION_TAIL_FIELDS: &[CalibrationTailField] = &[
    tail("DSSD 1 Temperature", 1426),
    tail("FEE1 Current", 1428),
    tail("FEE1 Temperature", 1430),
    tail("DSSD 7 Temperature", 1432),
    tail("FEE2 Current", 1434),
    tail("FEE2 Temperature", 1436),
    tail("FEE1 Threshold", 1438),
    tail("FEE2 Threshold", 1440),
    tail("DSSD 1 Temperature (dup)", 1442),
    tail("DSSD 4 Temperature", 1444),
];

const VOLTAGE_STEPS: usize = 11;
const BLOCK_START: usize = 18;
/// Each voltage step's 128 bytes hold L1, L2, L6, L7 back-to-back (32B each) -
const STEP_STRIDE: usize = 128;
const BLOCK_LEN: usize = 32;
const L1_LAYER_OFFSET: usize = 0;
const L2_LAYER_OFFSET: usize = 32;
const L6_LAYER_OFFSET: usize = 64;
const L7_LAYER_OFFSET: usize = 96;

const RESERVED_START: usize = 1446;
const RESERVED_LEN: usize = 616; // Byte 1446-2061
const CHECKSUM_START: usize = 2062;
const CHECKSUM_LEN: usize = 2; // Byte 2062-2063

/// One decoded raw line for the Calibration mode Data Table tab.
#[derive(Debug, Clone, Default)]
pub struct CalibrationLineResult {
    pub packet_sync: String,
    pub package_id: i32,
    pub packet_sequence: i32,
    pub packet_data_length: i32,
    pub time: DateTime<Utc>,
    pub data_type: String,
    pub sample_index: i32,
    pub l1_values: Vec<String>,
    pub l2_values: Vec<String>,
    pub l6_values: Vec<String>,
    pub l7_values: Vec<String>,
    /// Decoded values for `CALIBRATION_TAIL_FIELDS`, in that same order.
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

/// 11 voltage steps' raw 32-byte blocks for one layer, reading step-major:
/// within each step's 128-byte block, `layer_offset` selects that layer's
/// 32-byte sub-block (`L1_LAYER_OFFSET`/`L2_LAYER_OFFSET`/`L6_LAYER_OFFSET`/`L7_LAYER_OFFSET`).
fn layer_values(hex_data: &[String], layer_offset: usize) -> Vec<String> {
    (0..VOLTAGE_STEPS)
        .map(|step| {
            let offset = BLOCK_START + STEP_STRIDE * step + layer_offset;
            hex_span(hex_data, offset, BLOCK_LEN)
        })
        .collect()
}

/// Decodes one raw calibration line into a Data Table row.
pub fn parse_calibration_line(hex_data: &[String]) -> Option<CalibrationLineResult> {
    if hex_data.len() < CHECKSUM_START + CHECKSUM_LEN {
        return None;
    }
    let header = parse_line_header_fields(hex_data)?;
    let time = get_date_time_from_hex_data(hex_data).ok()?;
    let sample_index = dec_pair(hex_data, 16);

    let tail = CALIBRATION_TAIL_FIELDS
        .iter()
        .map(|f| {
            let raw = dec_pair(hex_data, f.byte_start);
            if f.label == "FEE2 Temperature" {
                format!("{:.4}", formula_decode(raw as f64))
            } else {
                raw.to_string()
            }
        })
        .collect();

    Some(CalibrationLineResult {
        packet_sync: header.packet_sync,
        package_id: header.package_id,
        packet_sequence: header.packet_sequence,
        packet_data_length: header.packet_data_length,
        time,
        data_type: header.data_type,
        sample_index,
        l1_values: layer_values(hex_data, L1_LAYER_OFFSET),
        l2_values: layer_values(hex_data, L2_LAYER_OFFSET),
        l6_values: layer_values(hex_data, L6_LAYER_OFFSET),
        l7_values: layer_values(hex_data, L7_LAYER_OFFSET),
        tail,
        reserved_hex: hex_span(hex_data, RESERVED_START, RESERVED_LEN),
        checksum_hex: hex_span(hex_data, CHECKSUM_START, CHECKSUM_LEN),
    })
}

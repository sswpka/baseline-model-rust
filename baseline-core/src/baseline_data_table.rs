//! Data Table decoding for Baseline mode

use crate::observation::data_processor::{formula_decode, get_date_time_from_hex_data, parse_line_header_fields};
use chrono::{DateTime, Utc};

/// One decoded tail field (Byte 1938-1957), 2 bytes each.
pub struct BaselineTailField {
    pub label: &'static str,
    pub byte_start: usize,
}

const fn tail(label: &'static str, byte_start: usize) -> BaselineTailField {
    BaselineTailField { label, byte_start }
}

/// Byte 1938-1957: 10 fields, 2 bytes each.
pub const BASELINE_TAIL_FIELDS: &[BaselineTailField] = &[
    tail("DSSD 1 Temperature", 1938),
    tail("FEE1 Current", 1940),
    tail("FEE1 Temperature", 1942),
    tail("DSSD 7 Temperature", 1944),
    tail("FEE2 Current", 1946),
    tail("FEE2 Temperature", 1948),
    tail("FEE1 Threshold", 1950),
    tail("FEE2 Threshold", 1952),
    tail("DSSD 1 Temperature (dup)", 1954),
    tail("DSSD 4 Temperature", 1956),
];

const L1L2_START: usize = 18;
const L6L7_START: usize = 978;
/// One layer's channel block within a sample, in bytes (16 channels x 2 bytes).
pub const BLOCK_LEN: usize = 32;
/// Decoded values per block (one per channel).
const VALUES_PER_BLOCK: usize = BLOCK_LEN / 2;
/// Byte stride between consecutive samples: one block per layer, two layers
/// per section (e.g. L1 then L2).
const SAMPLE_STRIDE: usize = BLOCK_LEN * 2;
pub const BLOCKS_PER_LAYER: usize = 15;

pub const L1_OFFSET: usize = L1L2_START;
pub const L2_OFFSET: usize = L1L2_START + BLOCK_LEN;
pub const L6_OFFSET: usize = L6L7_START;
pub const L7_OFFSET: usize = L6L7_START + BLOCK_LEN;

/// Byte offset of the `index`'th (0-based) sample's block for one layer,
/// given that layer's `L1_OFFSET`/`L2_OFFSET`/`L6_OFFSET`/`L7_OFFSET`.
pub const fn sample_block_offset(layer_base: usize, index: usize) -> usize {
    layer_base + SAMPLE_STRIDE * index
}

const RESERVED_START: usize = 1958;
const RESERVED_LEN: usize = 104; // Byte 1958-2061
const CHECKSUM_START: usize = 2062;
const CHECKSUM_LEN: usize = 2; // Byte 2062-2063

/// One decoded raw line for the Baseline mode Data Table tab.
#[derive(Debug, Clone, Default)]
pub struct BaselineLineResult {
    pub packet_sync: String,
    pub package_id: i32,
    pub packet_sequence: i32,
    pub packet_data_length: i32,
    pub time: DateTime<Utc>,
    pub data_type: String,
    pub sample_index: i32,
    /// Blocks of 16 decoded per-channel values, one `Vec` per sample block,
    /// for each layer.
    pub l1_blocks: Vec<Vec<i32>>,
    pub l2_blocks: Vec<Vec<i32>>,
    pub l6_blocks: Vec<Vec<i32>>,
    pub l7_blocks: Vec<Vec<i32>>,
    /// Decoded values for `BASELINE_TAIL_FIELDS`, in that same order.
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

/// Decodes one 32-byte channel block into its `VALUES_PER_BLOCK` (16)
/// per-channel values.
fn block_values(hex_data: &[String], start: usize) -> Vec<i32> {
    (0..VALUES_PER_BLOCK)
        .map(|i| dec_pair(hex_data, start + i * 2))
        .collect()
}

/// `BLOCKS_PER_LAYER` sample blocks for one layer, per `sample_block_offset`.
fn layer_blocks(hex_data: &[String], layer_base: usize) -> Vec<Vec<i32>> {
    (0..BLOCKS_PER_LAYER)
        .map(|index| block_values(hex_data, sample_block_offset(layer_base, index)))
        .collect()
}

/// Decodes one raw baseline line into a Data Table row, per `Baseline.txt`.
pub fn parse_baseline_line(hex_data: &[String]) -> Option<BaselineLineResult> {
    if hex_data.len() < CHECKSUM_START + CHECKSUM_LEN {
        return None;
    }
    let header = parse_line_header_fields(hex_data)?;
    let time = get_date_time_from_hex_data(hex_data).ok()?;
    let sample_index = dec_pair(hex_data, 16);

    let tail = BASELINE_TAIL_FIELDS
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

    Some(BaselineLineResult {
        packet_sync: header.packet_sync,
        package_id: header.package_id,
        packet_sequence: header.packet_sequence,
        packet_data_length: header.packet_data_length,
        time,
        data_type: header.data_type,
        sample_index,
        l1_blocks: layer_blocks(hex_data, L1_OFFSET),
        l2_blocks: layer_blocks(hex_data, L2_OFFSET),
        l6_blocks: layer_blocks(hex_data, L6_OFFSET),
        l7_blocks: layer_blocks(hex_data, L7_OFFSET),
        tail,
        reserved_hex: hex_span(hex_data, RESERVED_START, RESERVED_LEN),
        checksum_hex: hex_span(hex_data, CHECKSUM_START, CHECKSUM_LEN),
    })
}

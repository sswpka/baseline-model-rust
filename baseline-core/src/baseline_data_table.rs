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
    /// Blocks of 16 per-channel values, comma-joined, for each layer.
    pub l1_blocks: Vec<String>,
    pub l2_blocks: Vec<String>,
    pub l6_blocks: Vec<String>,
    pub l7_blocks: Vec<String>,
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

/// Decodes one 32-byte channel block into its comma-joined list of
/// `VALUES_PER_BLOCK` (16) per-channel values.
fn block_values(hex_data: &[String], start: usize) -> String {
    (0..VALUES_PER_BLOCK)
        .map(|i| dec_pair(hex_data, start + i * 2).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `BLOCKS_PER_LAYER` sample cells for one layer, per `sample_block_offset`.
fn layer_blocks(hex_data: &[String], layer_base: usize) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::baseline_file_service::process_file_stream;
    use crate::observation::data_processor::split_hex_data;
    use std::fs;

    /// Builds a 2064-byte synthetic packet with distinguishable per-channel
    /// values, matching `E225`+15-sample layout.
    fn synthetic_packet() -> Vec<u8> {
        let mut bytes = vec![0u8; 2064];
        bytes[0] = 0xE2;
        bytes[1] = 0x25;

        for sample in 0..15usize {
            let l1l2 = 18 + 64 * sample;
            let l6l7 = 978 + 64 * sample;
            for ch in 0..16usize {
                // Distinct, decodable values per (section, layer, sample, channel).
                let l1_val = (100 + sample * 100 + ch) as u16;
                let l2_val = (200 + sample * 100 + ch) as u16;
                let l6_val = (300 + sample * 100 + ch) as u16;
                let l7_val = (400 + sample * 100 + ch) as u16;
                bytes[l1l2 + ch * 2..l1l2 + ch * 2 + 2].copy_from_slice(&l1_val.to_be_bytes());
                bytes[l1l2 + 32 + ch * 2..l1l2 + 32 + ch * 2 + 2].copy_from_slice(&l2_val.to_be_bytes());
                bytes[l6l7 + ch * 2..l6l7 + ch * 2 + 2].copy_from_slice(&l6_val.to_be_bytes());
                bytes[l6l7 + 32 + ch * 2..l6l7 + 32 + ch * 2 + 2].copy_from_slice(&l7_val.to_be_bytes());
            }
        }
        bytes
    }

    fn hex_text(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02X}")).collect()
    }

    #[test]
    fn matches_shipped_baseline_file_service_decode() {
        let packet = synthetic_packet();
        let hex_data = split_hex_data(&hex_text(&packet));
        let row = parse_baseline_line(&hex_data).expect("valid packet decodes");

        let path = std::env::temp_dir().join(format!(
            "baseline_data_table_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::write(&path, hex_text(&packet)).unwrap();
        let shipped = process_file_stream(&path, None).unwrap();
        let _ = fs::remove_file(&path);

        assert_eq!(shipped.len(), 15);
        for (sample, data) in shipped.iter().enumerate() {
            let expected_l1: Vec<String> = data.l1.iter().map(|v| (*v as i32).to_string()).collect();
            let expected_l2: Vec<String> = data.l2.iter().map(|v| (*v as i32).to_string()).collect();
            let expected_l6: Vec<String> = data.l6.iter().map(|v| (*v as i32).to_string()).collect();
            let expected_l7: Vec<String> = data.l7.iter().map(|v| (*v as i32).to_string()).collect();

            assert_eq!(row.l1_blocks[sample], expected_l1.join(", "), "L1 sample {sample}");
            assert_eq!(row.l2_blocks[sample], expected_l2.join(", "), "L2 sample {sample}");
            assert_eq!(row.l6_blocks[sample], expected_l6.join(", "), "L6 sample {sample}");
            assert_eq!(row.l7_blocks[sample], expected_l7.join(", "), "L7 sample {sample}");
        }
    }
}

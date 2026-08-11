//! Direct transcription of `Infrastructure/Services/Baseline/BaselineFileService.cs`'s
//! streaming hex-segment parser (`ProcessFileStreamAsync`). Excel export/import
//! for this service live in `baseline_excel.rs`.

use crate::models::baseline::BaselineData;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

const VOLTAGE_FACTOR: f32 = (5.0 / 16383.0) * 1000.0;
const CHUNK_SIZE: usize = 4128;
const SAMPLES_PER_SEGMENT: usize = 15;
const BUFFER_SIZE: usize = 64; // size for l1l2Dec and l6l7Dec

/// Streams `file_path`, extracting every complete `E225`-headed 4128-hex-char
/// segment and decoding its 15 samples. `on_progress`, if given, is called
/// with a 0..=100 percentage as bytes are consumed (mirrors the C# `IProgress<double>`).
pub fn process_file_stream(
    file_path: impl AsRef<Path>,
    mut on_progress: Option<&mut dyn FnMut(f64)>,
) -> Result<Vec<BaselineData>, String> {
    let file_path = file_path.as_ref();
    if !file_path.exists() {
        return Err(format!("File not found: {}", file_path.display()));
    }

    let file = File::open(file_path).map_err(|e| format!("Processing failed: {e}"))?;
    let total_bytes = file.metadata().map(|m| m.len()).unwrap_or(0).max(1);
    let mut reader = BufReader::with_capacity(131072, file);

    let mut results = Vec::new();
    let mut hex_accumulator = String::with_capacity(CHUNK_SIZE * 4);
    let mut l1l2_dec = [0i32; BUFFER_SIZE];
    let mut l6l7_dec = [0i32; BUFFER_SIZE];

    let mut file_buffer = [0u8; 131072];
    let mut processed_bytes: u64 = 0;

    loop {
        let bytes_read = reader
            .read(&mut file_buffer)
            .map_err(|e| format!("Processing failed: {e}"))?;
        if bytes_read == 0 {
            break;
        }
        processed_bytes += bytes_read as u64;

        for &b in &file_buffer[..bytes_read] {
            let c = b as char;
            if is_hex_char(c) {
                hex_accumulator.push(c);
            }
        }

        process_accumulated_hex(&mut hex_accumulator, &mut results, &mut l1l2_dec, &mut l6l7_dec, false);

        if let Some(cb) = on_progress.as_deref_mut() {
            if results.len() % 1000 == 0 {
                cb(processed_bytes as f64 / total_bytes as f64 * 100.0);
            }
        }
    }

    process_accumulated_hex(&mut hex_accumulator, &mut results, &mut l1l2_dec, &mut l6l7_dec, true);

    Ok(results)
}

fn process_accumulated_hex(
    sb: &mut String,
    results: &mut Vec<BaselineData>,
    l1l2_dec: &mut [i32; BUFFER_SIZE],
    l6l7_dec: &mut [i32; BUFFER_SIZE],
    force: bool,
) {
    let buffer: Vec<char> = sb.chars().collect();
    let mut search_index = 0usize;

    loop {
        let header_index = find_header_case_insensitive(&buffer, search_index);

        let header_index = match header_index {
            Some(idx) => idx,
            None => {
                if force {
                    sb.clear();
                } else {
                    *sb = buffer[search_index..].iter().collect();
                }
                return;
            }
        };

        if header_index + CHUNK_SIZE <= buffer.len() {
            let segment = &buffer[header_index..header_index + CHUNK_SIZE];
            process_single_segment(segment, results, l1l2_dec, l6l7_dec);
            search_index = header_index + CHUNK_SIZE;
        } else {
            *sb = buffer[search_index..].iter().collect();
            return;
        }
    }
}

fn find_header_case_insensitive(buffer: &[char], from: usize) -> Option<usize> {
    const HEADER: [char; 4] = ['E', '2', '2', '5'];
    if from + 4 > buffer.len() {
        return None;
    }
    'outer: for i in from..=buffer.len() - 4 {
        for (k, &hc) in HEADER.iter().enumerate() {
            if buffer[i + k].to_ascii_uppercase() != hc {
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

fn process_single_segment(
    segment: &[char],
    results: &mut Vec<BaselineData>,
    l1l2_dec: &mut [i32; BUFFER_SIZE],
    l6l7_dec: &mut [i32; BUFFER_SIZE],
) {
    let sampling_packet = extract_sampling_packet(segment);

    for i in 0..SAMPLES_PER_SEGMENT {
        let mut data = BaselineData {
            
            sampling_packet_no: sampling_packet,
            sampling_no: (i + 1) as i32,
            ..Default::default()
        };

        let l1l2_offset = 36 + 64 * i * 2;
        let l6l7_offset = 1956 + 64 * i * 2;

        if !parse_hex_to_span(segment, l1l2_offset, BUFFER_SIZE, l1l2_dec)
            || !parse_hex_to_span(segment, l6l7_offset, BUFFER_SIZE, l6l7_dec)
        {
            continue;
        }

        process_channels(&mut data, l1l2_dec, l6l7_dec);
        results.push(data);
    }
}

#[inline]
fn is_hex_char(c: char) -> bool {
    c.is_ascii_hexdigit()
}

#[inline]
fn extract_sampling_packet(hex: &[char]) -> i32 {
    let byte1 = hex_char_to_int(hex[32]) * 16 + hex_char_to_int(hex[33]);
    let byte2 = hex_char_to_int(hex[34]) * 16 + hex_char_to_int(hex[35]);
    (byte1 << 8) | byte2
}

#[inline]
fn parse_hex_to_span(hex: &[char], start_offset: usize, byte_count: usize, output: &mut [i32]) -> bool {
    if start_offset + byte_count * 2 > hex.len() {
        return false;
    }
    for i in 0..byte_count {
        let pos = start_offset + i * 2;
        output[i] = hex_char_to_int(hex[pos]) * 16 + hex_char_to_int(hex[pos + 1]);
    }
    true
}

#[inline]
fn hex_char_to_int(c: char) -> i32 {
    match c {
        '0'..='9' => c as i32 - '0' as i32,
        'A'..='F' => c as i32 - 'A' as i32 + 10,
        'a'..='f' => c as i32 - 'a' as i32 + 10,
        _ => 0,
    }
}

#[inline]
fn process_channels(data: &mut BaselineData, l1l2_dec: &[i32; BUFFER_SIZE], l6l7_dec: &[i32; BUFFER_SIZE]) {
    for j in 0..16usize {
        let j2 = j * 2;
        let j2_32 = j2 + 32;

        let l1_val = (l1l2_dec[j2] << 8) | l1l2_dec[j2 + 1];
        data.l1[j] = l1_val as f32;
        data.l1_voltage[j] = l1_val as f32 * VOLTAGE_FACTOR;

        let l2_val = (l1l2_dec[j2_32] << 8) | l1l2_dec[j2_32 + 1];
        data.l2[j] = l2_val as f32;
        data.l2_voltage[j] = l2_val as f32 * VOLTAGE_FACTOR;

        let l6_val = (l6l7_dec[j2] << 8) | l6l7_dec[j2 + 1];
        data.l6[j] = l6_val as f32;
        data.l6_voltage[j] = l6_val as f32 * VOLTAGE_FACTOR;

        let l7_val = (l6l7_dec[j2_32] << 8) | l6l7_dec[j2_32 + 1];
        data.l7[j] = l7_val as f32;
        data.l7_voltage[j] = l7_val as f32 * VOLTAGE_FACTOR;
    }
}


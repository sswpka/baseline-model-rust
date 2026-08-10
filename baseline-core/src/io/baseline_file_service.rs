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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push("BaselineModeFileTestsRust");
        std::fs::create_dir_all(&dir).unwrap();
        dir.push(name);
        dir
    }

    fn write_all_text(path: &Path, content: &str) {
        let mut f = File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    fn create_valid_segment(packet_no: u32) -> String {
        let header = "E22508B0";
        let sequence = format!("{:04X}", packet_no);
        let remaining_length = CHUNK_SIZE - header.len() - sequence.len();
        format!("{header}{sequence}{}", "0".repeat(remaining_length))
    }

    #[test]
    fn empty_file_returns_empty_list() {
        let path = temp_path("empty.txt");
        write_all_text(&path, "");
        let result = process_file_stream(&path, None).unwrap();
        assert!(result.is_empty());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn no_e225_header_returns_empty_list() {
        let path = temp_path("no_header.txt");
        write_all_text(&path, &"A".repeat(10000));
        let result = process_file_stream(&path, None).unwrap();
        assert!(result.is_empty());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn single_valid_segment_returns_15_items() {
        let path = temp_path("single_segment.txt");
        write_all_text(&path, &create_valid_segment(1));
        let result = process_file_stream(&path, None).unwrap();
        assert_eq!(result.len(), SAMPLES_PER_SEGMENT);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn multiple_segments_returns_correct_count() {
        let path = temp_path("multi_segment.txt");
        let content = format!("{}{}{}", create_valid_segment(1), create_valid_segment(2), create_valid_segment(3));
        write_all_text(&path, &content);
        let result = process_file_stream(&path, None).unwrap();
        assert_eq!(result.len(), 3 * SAMPLES_PER_SEGMENT);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn segments_with_newlines_parses_correctly() {
        let path = temp_path("newlines.txt");
        let content = format!(
            "{}\n{}\r\n{}",
            create_valid_segment(1),
            create_valid_segment(2),
            create_valid_segment(3)
        );
        write_all_text(&path, &content);
        let result = process_file_stream(&path, None).unwrap();
        assert_eq!(result.len(), 3 * SAMPLES_PER_SEGMENT);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn segments_with_spaces_parses_correctly() {
        let path = temp_path("spaces.txt");
        let seg1 = create_valid_segment(1);
        let seg2 = create_valid_segment(2);
        let content = format!("{} {}  {}", &seg1[..100], &seg1[100..], seg2);
        write_all_text(&path, &content);
        let result = process_file_stream(&path, None).unwrap();
        assert_eq!(result.len(), 2 * SAMPLES_PER_SEGMENT);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn incomplete_segment_at_end_ignores_incomplete() {
        let path = temp_path("incomplete.txt");
        let complete = create_valid_segment(1);
        let incomplete = format!("E225{}", "0".repeat(100));
        write_all_text(&path, &format!("{complete}{incomplete}"));
        let result = process_file_stream(&path, None).unwrap();
        assert_eq!(result.len(), SAMPLES_PER_SEGMENT);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn garbage_before_header_finds_header() {
        let path = temp_path("garbage_before.txt");
        let garbage = "GARBAGE123456789";
        let segment = create_valid_segment(1);
        write_all_text(&path, &format!("{garbage}{segment}"));
        let result = process_file_stream(&path, None).unwrap();
        assert_eq!(result.len(), SAMPLES_PER_SEGMENT);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn case_insensitive_header_finds_lowercase() {
        let path = temp_path("lowercase.txt");
        let segment = format!("e225{}", "0".repeat(CHUNK_SIZE - 4));
        write_all_text(&path, &segment);
        let result = process_file_stream(&path, None).unwrap();
        assert_eq!(result.len(), SAMPLES_PER_SEGMENT);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn valid_segment_populates_baseline_data() {
        let path = temp_path("data_check.txt");
        write_all_text(&path, &create_valid_segment(42));
        let result = process_file_stream(&path, None).unwrap();
        assert_eq!(result.len(), SAMPLES_PER_SEGMENT);
        let data = &result[0];
        assert_eq!(data.l1.len(), 16);
        assert_eq!(data.l2.len(), 16);
        assert_eq!(data.l6.len(), 16);
        assert_eq!(data.l7.len(), 16);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn large_file_processes_without_memory_issue() {
        let path = temp_path("large.txt");
        let mut content = String::with_capacity(500 * CHUNK_SIZE);
        for i in 0..500u32 {
            content.push_str(&create_valid_segment(i));
        }
        write_all_text(&path, &content);
        let result = process_file_stream(&path, None).unwrap();
        assert_eq!(result.len(), 500 * SAMPLES_PER_SEGMENT);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn truncated_header_tail_does_not_panic() {
        let path = temp_path("truncated_tail.txt");
        // A full valid segment followed by a truncated "E22" header fragment
        // at EOF (the real-world case flagged by HeaderVerificationTests.cs
        // for partial/truncated raw-data files).
        let content = format!("{}E22", create_valid_segment(1));
        write_all_text(&path, &content);
        let result = process_file_stream(&path, None).unwrap();
        assert_eq!(result.len(), SAMPLES_PER_SEGMENT);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn bare_e2_tail_does_not_panic() {
        let path = temp_path("bare_e2.txt");
        write_all_text(&path, "E2");
        let result = process_file_stream(&path, None).unwrap();
        assert!(result.is_empty());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn uses_correct_offsets_not_legacy_ones() {
        // Correct offsets: L1/L2 = 36 + 64*0*2 = 36, L6/L7 = 1956.
        // Incorrect (legacy) offsets: L1/L2 = 18, L6/L7 = 978.
        let mut content = String::new();
        content.push_str("E225"); // 4 chars
        content.push_str(&"0".repeat(18 - 4));
        content.push_str("AAAA"); // at incorrect-offset location (index 18)
        content.push_str(&"0".repeat(36 - 22));
        content.push_str("BBBB"); // at correct-offset location (index 36)
        content.push_str(&"0".repeat(CHUNK_SIZE - 40));

        let path = temp_path("offset_check.txt");
        write_all_text(&path, &content);
        let result = process_file_stream(&path, None).unwrap();
        assert!(!result.is_empty());

        let expected_value = 0xBBBBu32 as f32;
        let actual_value = result[0].l1[0];
        assert_eq!(actual_value, expected_value);
        assert_ne!(actual_value, 0xAAAAu32 as f32);
        std::fs::remove_file(&path).unwrap();
    }
}

//! Shared "strip whitespace, find E225 runs, chunk into fixed-size hex
//! segments" logic used identically by `CalibrationViewModel.ProcessData`,
//! `FluxViewModel.ProcessData`, and `ObservationViewModel.FilterSegmentsAsync`
//! (the last uses a different chunk size: `PacketHexLength` (512) vs the
//! other two's `SegmentHexLength` (4128) - `chunk_hex_len` parameterizes that).

use super::regex_patterns::E225_HEADER;
use std::fs;
use std::path::Path;

pub fn filter_e225_segments_from_files(files: &[impl AsRef<Path>], chunk_hex_len: usize) -> Result<Vec<String>, String> {
    let mut filtered_segments = Vec::new();

    for file in files {
        let content = fs::read_to_string(file.as_ref()).map_err(|e| e.to_string())?;
        let cleaned: String = content.chars().filter(|c| !c.is_whitespace()).collect();

        for m in E225_HEADER.find_iter(&cleaned) {
            let segment = m.as_str();
            let segment_len = segment.len();
            let mut i = 0;
            while i < segment_len {
                let len = chunk_hex_len.min(segment_len - i);
                filtered_segments.push(segment[i..i + len].to_string());
                i += chunk_hex_len;
            }
        }
    }

    if let Some(last) = filtered_segments.last() {
        if last.len() < chunk_hex_len {
            filtered_segments.pop();
        }
    }

    Ok(filtered_segments)
}


//! Sementics for filtering segments from files based on regex patterns
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

    // Drop any incomplete trailing chunk
    filtered_segments.retain(|segment| segment.len() == chunk_hex_len);

    Ok(filtered_segments)
}

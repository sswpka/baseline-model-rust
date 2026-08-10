use crate::models::shared::{AppConstants, HeaderValidationResult};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub fn validate_file<P: AsRef<Path>>(file_path: P) -> HeaderValidationResult {
    let file_path = file_path.as_ref();

    if !file_path.exists() {
        return HeaderValidationResult {
            is_valid: false,
            error_message: Some("File not found.".to_string()),
            ..Default::default()
        };
    }

    let file = match File::open(file_path) {
        Ok(f) => f,
        Err(e) => {
            return HeaderValidationResult {
                is_valid: false,
                filtered_file_path: Some(file_path.to_string_lossy().to_string()),
                error_message: Some(format!("Error reading file: {e}")),
                ..Default::default()
            }
        }
    };

    let reader = BufReader::new(file);
    let mut line_number = 0i32;
    let mut first_header: Option<String> = None;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                return HeaderValidationResult {
                    is_valid: false,
                    filtered_file_path: Some(file_path.to_string_lossy().to_string()),
                    error_message: Some(format!("Error reading file: {e}")),
                    ..Default::default()
                }
            }
        };
        line_number += 1;

        if line.is_empty() {
            continue;
        }

        // Strict check: no trim, just check if it starts with E225 for every row.
        if !line.starts_with(AppConstants::HEADER_START) {
            return HeaderValidationResult {
                is_valid: false,
                error_line: line_number,
                error_content: Some(line.clone()),
                filtered_file_path: Some(file_path.to_string_lossy().to_string()),
                error_message: Some(format!("Header INCORRECT at line {line_number}")),
                ..Default::default()
            };
        }

        if first_header.is_none() {
            first_header = Some(line);
        }
    }

    if line_number == 0 {
        return HeaderValidationResult {
            is_valid: false,
            error_message: Some("File is empty.".to_string()),
            ..Default::default()
        };
    }

    HeaderValidationResult {
        is_valid: true,
        first_header_content: first_header,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push("BaselineModeRustTests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.push(name);
        dir
    }

    fn write_all_text(path: &Path, content: &str) {
        let mut f = File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn file_not_found_returns_invalid() {
        let path = temp_path("non_existent_file.txt");
        let _ = std::fs::remove_file(&path);
        let result = validate_file(&path);
        assert!(!result.is_valid);
        assert_eq!(result.error_message.as_deref(), Some("File not found."));
    }

    #[test]
    fn empty_file_returns_invalid() {
        let path = temp_path("empty_file.txt");
        write_all_text(&path, "");
        let result = validate_file(&path);
        assert!(!result.is_valid);
        assert_eq!(result.error_message.as_deref(), Some("File is empty."));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn only_whitespace_lines_returns_invalid() {
        let path = temp_path("whitespace_file.txt");
        write_all_text(&path, "\n\n   \n\t\n");
        let result = validate_file(&path);
        assert!(!result.is_valid);
        assert!(result.error_message.unwrap().contains("line"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn single_valid_line_returns_valid() {
        let path = temp_path("valid_single.txt");
        let valid_hex = format!(
            "E22508B0D7D10807025738E800BF00AE17D2{}",
            "0".repeat(4128 - 38)
        );
        write_all_text(&path, &valid_hex);
        let result = validate_file(&path);
        assert!(result.is_valid);
        assert!(result.first_header_content.is_some());
        assert!(result.first_header_content.unwrap().starts_with("E225"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn multiple_valid_lines_returns_valid() {
        let path = temp_path("valid_multiple.txt");
        let line1 = format!("E22508B0D7D10807025738E800BF00AE17D2{}", "A".repeat(100));
        let line2 = format!("E22508B0D7D20807025738E8019400AE17D3{}", "B".repeat(100));
        let line3 = format!("E22508B0D7D30807025738E8027400AE17D4{}", "C".repeat(100));
        write_all_text(&path, &format!("{line1}\n{line2}\n{line3}"));
        let result = validate_file(&path);
        assert!(result.is_valid);
        assert_eq!(result.first_header_content.as_deref(), Some(line1.as_str()));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn valid_lines_with_empty_lines_between_returns_valid() {
        let path = temp_path("valid_with_empty.txt");
        write_all_text(&path, "E225AAAA\n\nE225BBBB\n\nE225CCCC");
        let result = validate_file(&path);
        assert!(result.is_valid);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn first_line_invalid_returns_invalid_at_line1() {
        let path = temp_path("invalid_first.txt");
        write_all_text(&path, "XXXX0000\nE2250000");
        let result = validate_file(&path);
        assert!(!result.is_valid);
        assert_eq!(result.error_line, 1);
        assert!(result.error_message.unwrap().contains("line 1"));
        assert_eq!(result.error_content.as_deref(), Some("XXXX0000"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn second_line_invalid_returns_invalid_at_line2() {
        let path = temp_path("invalid_second.txt");
        write_all_text(&path, "E2250000\nINVALID!");
        let result = validate_file(&path);
        assert!(!result.is_valid);
        assert_eq!(result.error_line, 2);
        assert!(result.error_message.unwrap().contains("line 2"));
        assert_eq!(result.error_content.as_deref(), Some("INVALID!"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn middle_line_invalid_returns_correct_line_number() {
        let path = temp_path("invalid_middle.txt");
        write_all_text(&path, "E225AAAA\nE225BBBB\nE225CCCC\nBADLINE!\nE225DDDD");
        let result = validate_file(&path);
        assert!(!result.is_valid);
        assert_eq!(result.error_line, 4);
        assert_eq!(result.error_content.as_deref(), Some("BADLINE!"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn line_starts_with_e224_not_e225_returns_invalid() {
        let path = temp_path("invalid_partial.txt");
        write_all_text(&path, "E224AAAA");
        let result = validate_file(&path);
        assert!(!result.is_valid);
        assert_eq!(result.error_content.as_deref(), Some("E224AAAA"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn lowercase_e225_returns_invalid() {
        let path = temp_path("invalid_lowercase.txt");
        write_all_text(&path, "e225AAAA");
        let result = validate_file(&path);
        assert!(!result.is_valid);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn line_with_leading_space_returns_invalid() {
        let path = temp_path("invalid_space.txt");
        write_all_text(&path, " E225AAAA");
        let result = validate_file(&path);
        assert!(!result.is_valid);
        assert_eq!(result.error_content.as_deref(), Some(" E225AAAA"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn very_long_valid_line_returns_valid() {
        let path = temp_path("valid_long.txt");
        let long_line = format!("E225{}", "F".repeat(10000));
        write_all_text(&path, &long_line);
        let result = validate_file(&path);
        assert!(result.is_valid);
        assert_eq!(result.first_header_content.as_deref(), Some(long_line.as_str()));
        std::fs::remove_file(&path).unwrap();
    }
}

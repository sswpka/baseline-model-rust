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


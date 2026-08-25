//! FileHelper

use std::fs;
use std::path::{Path, PathBuf};

const APP_FOLDER_NAME: &str = "DSSD_Analysis";
const SOURCE_FOLDER_NAME: &str = "Source";

pub fn get_documents_folder() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(|p| PathBuf::from(p).join("Documents"))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn get_output_folder(sub_folder: &str) -> std::io::Result<PathBuf> {
    let mut path = get_documents_folder().join(APP_FOLDER_NAME);
    if !sub_folder.is_empty() {
        path = path.join(sub_folder);
    }
    if !path.exists() {
        fs::create_dir_all(&path)?;
    }
    Ok(path)
}

pub fn combine_files(file_paths: &[PathBuf], output_file_name: &str) -> Result<PathBuf, String> {
    if file_paths.is_empty() {
        return Err("No files selected for combination.".to_string());
    }
    let output_folder = get_output_folder("CombinedData").map_err(|e| e.to_string())?;
    let output_path = output_folder.join(output_file_name);

    (|| -> std::io::Result<()> {
        let mut out = fs::File::create(&output_path)?;
        for file_path in file_paths {
            let mut input = fs::File::open(file_path)?;
            std::io::copy(&mut input, &mut out)?;
        }
        Ok(())
    })()
    .map_err(|e| format!("Failed to combine files: {e}"))?;

    Ok(output_path)
}

pub fn resolve_excel_save_path(file_name: &str, sub_folder: &str) -> Result<PathBuf, String> {
    let path = Path::new(file_name);
    if path.is_absolute() {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() && !dir.exists() {
                fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
        }
        return Ok(path.to_path_buf());
    }

    let mut name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    if name.trim().is_empty() {
        return Err("Invalid file name. Please use a valid name.".to_string());
    }
    if !name.to_ascii_lowercase().ends_with(".xlsx") {
        name.push_str(".xlsx");
    }

    let folder_name = if sub_folder.trim().is_empty() { SOURCE_FOLDER_NAME } else { sub_folder };
    let save_dir = get_output_folder(folder_name).map_err(|e| e.to_string())?;
    Ok(save_dir.join(name))
}

pub fn get_possible_file_paths(output_name: &str, sub_folder: &str) -> Vec<PathBuf> {
    let documents_base = get_documents_folder().join(APP_FOLDER_NAME);
    let stem = if output_name.to_ascii_lowercase().ends_with(".xlsx") {
        &output_name[..output_name.len() - ".xlsx".len()]
    } else {
        output_name
    };
    let folder_name = if sub_folder.trim().is_empty() { SOURCE_FOLDER_NAME } else { sub_folder };
    let mut candidates = vec![documents_base.join(folder_name).join(format!("{stem}.xlsx"))];
    if folder_name != SOURCE_FOLDER_NAME {
        candidates.push(documents_base.join(SOURCE_FOLDER_NAME).join(format!("{stem}.xlsx")));
    }
    candidates.push(documents_base.join(stem).join(format!("{stem}_ParticleData.xlsx")));
    candidates.push(documents_base.join(format!("{stem}.xlsx")));
    candidates
}

pub fn find_excel_file(output_name: &str, sub_folder: &str) -> Option<PathBuf> {
    get_possible_file_paths(output_name, sub_folder).into_iter().find(|p| p.exists())
}


//! Build Histogram and compute mean for one detector layer (16 channels), 
//! UI /plot rendering and Dispatcher-marshaled progress reporting are not part of

use crate::models::baseline::BaselineData;
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};

pub fn calculate_mean(data: &[f64]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    data.iter().sum::<f64>() / data.len() as f64
}

/// ApplyThresholding: keeps only samples whose deviation from the
/// mean exceeds `k_factor * sigma`
pub fn apply_thresholding(centered_data: &[f64], k_factor: f64, use_thresholding: bool) -> Vec<f64> {
    if !use_thresholding {
        return centered_data.to_vec();
    }
    let length = centered_data.len();
    if length == 0 {
        return Vec::new();
    }

    let mean = centered_data.iter().sum::<f64>() / length as f64;
    let sum_squares: f64 = centered_data.iter().map(|&v| (v - mean).powi(2)).sum();
    let sigma = (sum_squares / length as f64).sqrt();
    let threshold = k_factor * sigma;

    centered_data.iter().copied().filter(|&v| (v - mean).abs() > threshold).collect()
}

/// ProcessData, build Histogram, and compute mean for one detector layer (16 channels)
pub fn build_histogram(
    filtered_data: &[f64],
    h_min: f64,
    h_max: f64,
    bin_count: usize,
    x_axis_is_voltage: bool,
) -> (Vec<f64>, Vec<f64>) {
    let (counts, bin_edges) = histogram_common(filtered_data, h_min, h_max, bin_count);

    let mut bin_centers = vec![0.0; bin_edges.len() - 1];
    for k in 0..bin_centers.len() {
        let center = bin_edges[k] + 0.5;
        bin_centers[k] = if x_axis_is_voltage { (center / 16384.0) * 5.0 * 1000.0 } else { center };
    }

    (counts, bin_centers)
}

/// CalibrationViewModel.UpdatePlots`'s histogram (true average bin
/// centers, unlike [`build_histogram`]'s `edge + 0.5` approximation
pub fn build_histogram_avg_centers(filtered_data: &[f64], min: f64, max: f64, bin_count: usize) -> (Vec<f64>, Vec<f64>) {
    let (counts, bin_edges) = histogram_common(filtered_data, min, max, bin_count);
    let mut bin_centers = vec![0.0; bin_edges.len() - 1];
    for k in 0..bin_centers.len() {
        bin_centers[k] = (bin_edges[k] + bin_edges[k + 1]) / 2.0;
    }
    (counts, bin_centers)
}

/// HistogramCommon: fixed-width bins spanning `[min, max]`; values outside the range are not counted
fn histogram_common(data: &[f64], min: f64, max: f64, bin_count: usize) -> (Vec<f64>, Vec<f64>) {
    let mut counts = vec![0.0; bin_count];
    let mut edges = vec![0.0; bin_count + 1];
    let bin_width = (max - min) / bin_count as f64;
    for i in 0..=bin_count {
        edges[i] = min + i as f64 * bin_width;
    }

    if bin_width > 0.0 {
        for &v in data {
            if v < min || v > max {
                continue;
            }
            let mut idx = ((v - min) / bin_width) as i64;
            if idx >= bin_count as i64 {
                idx = bin_count as i64 - 1;
            }
            if idx < 0 {
                idx = 0;
            }
            counts[idx as usize] += 1.0;
        }
    }

    (counts, edges)
}

/// CalculateCoincidenceMatrix: rows/cols indexed \[Z\]\[X\]
pub fn calculate_coincidence_matrix(data: &[BaselineData], layer_selector: impl Fn(&BaselineData) -> [f32; 16]) -> [[f64; 8]; 8] {
    let mut matrix = [[0.0f64; 8]; 8];

    for item in data {
        let values = layer_selector(item);

        let mut max_x = 0usize;
        let mut max_val_x = values[0];
        for x in 1..8 {
            if values[x] > max_val_x {
                max_val_x = values[x];
                max_x = x;
            }
        }

        let mut max_z = 0usize;
        let mut max_val_z = values[8];
        for z in 1..8 {
            if values[z + 8] > max_val_z {
                max_val_z = values[z + 8];
                max_z = z;
            }
        }

        matrix[max_z][max_x] += 1.0;
    }

    matrix
}

/// CalculateMeanParallel: per-channel (16) mean across all events for one detector layer.
pub fn calculate_mean_parallel(data: &[BaselineData], layer_selector: impl Fn(&BaselineData) -> [f32; 16] + Sync) -> [f64; 16] {
    let count = data.len();
    if count == 0 {
        return [0.0; 16];
    }

    let sums = data
        .par_iter()
        .map(|item| {
            let values = layer_selector(item);
            let mut local = [0.0f64; 16];
            for ch in 0..16 {
                local[ch] = values[ch] as f64;
            }
            local
        })
        .reduce(
            || [0.0f64; 16],
            |mut a, b| {
                for ch in 0..16 {
                    a[ch] += b[ch];
                }
                a
            },
        );

    let mut means = sums;
    for ch in 0..16 {
        means[ch] /= count as f64;
    }
    means
}

/// GetDailyOutputDirectory: `{base}/{yyyy-MM-dd}`, created folder if missing.
pub fn get_daily_output_directory(base: &Path, today: chrono::NaiveDate) -> std::io::Result<PathBuf> {
    let full_path = base.join(today.format("%Y-%m-%d").to_string());
    if !full_path.exists() {
        fs::create_dir_all(&full_path)?;
    }
    Ok(full_path)
}

/// WriteMeansToFile: one `"F2"`-formatted value per line.
pub fn write_means_to_file(dir: &Path, layer_id: u32, means: &[f64; 16]) -> std::io::Result<()> {
    let lines: Vec<String> = means.iter().map(|m| format!("{m:.2}")).collect();
    let path = dir.join(format!("MeanValues{layer_id}.txt"));
    fs::write(path, lines.join("\n"))
}

/// LoadMeanFromFile: reads a mean value from a file.
pub fn load_mean_from_file(dir: &Path, layer_id: u32, channel_index: usize) -> f64 {
    let path = dir.join(format!("MeanValues{layer_id}.txt"));
    let Ok(content) = fs::read_to_string(path) else {
        return 0.0;
    };
    content
        .lines()
        .nth(channel_index)
        .and_then(|l| l.trim().parse::<f64>().ok())
        .unwrap_or(0.0)
}


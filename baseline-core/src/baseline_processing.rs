//! Pure business logic extracted from the Baseline mode ViewModels
//! (`MainViewModel.cs` + `.Processing.cs` + `.FileOperations.cs`). UI/plot
//! rendering and Dispatcher-marshaled progress reporting are not part of
//! this port; that lives in the egui app layer, which calls these functions
//! from worker threads.
//!
//! `MainViewModel.cs`'s own `BuildHistogram` helper (using
//! `(edges[i]+edges[i+1])/2` for bin centers) is dead code - grep confirms
//! no call site - so it is *not* replicated. The actually-exercised path is
//! the inline histogram logic in `ProcessData`/`ProcessCalibration`'s ROI
//! sibling, which computes bin centers as `edge + 0.5` (an approximation
//! that only equals the true center when bin width is ~1, true for the
//! default ADC config of 16384 bins over a ~16384-wide range). That exact
//! formula is what's transcribed in [`build_histogram`] below.

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

/// Matches `ApplyThresholding`: keeps only samples whose deviation from the
/// mean exceeds `k_factor * sigma`. No-op (returns input unchanged) when
/// `use_thresholding` is false.
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

/// Matches the inline histogram + bin-center logic in `ProcessData`
/// (see module docs for why `edge + 0.5`, not the true bin center, is used).
/// `x_axis_is_voltage` mirrors `SelectedXAxisIndex == 1`.
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

/// Matches `CalibrationViewModel.UpdatePlots`'s histogram (true average bin
/// centers, unlike [`build_histogram`]'s `edge + 0.5` approximation - the two
/// modes' C# actually use different formulas, transcribed as-is).
pub fn build_histogram_avg_centers(filtered_data: &[f64], min: f64, max: f64, bin_count: usize) -> (Vec<f64>, Vec<f64>) {
    let (counts, bin_edges) = histogram_common(filtered_data, min, max, bin_count);
    let mut bin_centers = vec![0.0; bin_edges.len() - 1];
    for k in 0..bin_centers.len() {
        bin_centers[k] = (bin_edges[k] + bin_edges[k + 1]) / 2.0;
    }
    (counts, bin_centers)
}

/// Best-effort transcription of `ScottPlot.Statistics.Common.Histogram`:
/// fixed-width bins spanning `[min, max]`; values outside the range are not
/// counted (ScottPlot's exact out-of-range behavior could not be inspected
/// from source in this port and should be re-verified against real data).
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

/// Matches `CalculateCoincidenceMatrix`: rows/cols indexed \[Z\]\[X\], each 0-7.
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

/// Matches `CalculateMeanParallel`: per-channel (16) mean across all events
/// for one detector layer.
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

/// Matches `GetDailyOutputDirectory`: `{base}/{yyyy-MM-dd}`, created if missing.
pub fn get_daily_output_directory(base: &Path, today: chrono::NaiveDate) -> std::io::Result<PathBuf> {
    let full_path = base.join(today.format("%Y-%m-%d").to_string());
    if !full_path.exists() {
        fs::create_dir_all(&full_path)?;
    }
    Ok(full_path)
}

/// Matches `WriteMeansToFile`: one `"F2"`-formatted value per line.
pub fn write_means_to_file(dir: &Path, layer_id: u32, means: &[f64; 16]) -> std::io::Result<()> {
    let lines: Vec<String> = means.iter().map(|m| format!("{m:.2}")).collect();
    let path = dir.join(format!("MeanValues{layer_id}.txt"));
    fs::write(path, lines.join("\n"))
}

/// Matches `LoadMeanFromFile`. `layer_id` is 1/2/6/7 (matches file naming);
/// returns 0.0 on any failure, mirroring the original's swallowed `catch {}`.
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


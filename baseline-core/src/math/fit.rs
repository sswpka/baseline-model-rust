//! Direct transcription of `Infrastructure/Services/MathService.cs`.
//! `GaussianFit`/`LorentzianFit` used MathNet.Numerics' `LevenbergMarquardtMinimizer`

use super::hemg;
use crate::models::baseline::{FittingResult, HemgFitConfig};

const MIN_VALUE: f64 = 1e-9;
const MAX_EXP_ARG: f64 = 700.0;

#[derive(Debug, Clone, Copy, Default)]
pub struct MathService;

impl MathService {
    pub fn new() -> Self {
        Self
    }

    pub fn calculate_moments(&self, x_data: &[f64], y_data: &[f64]) -> (f64, f64, f64) {
        if x_data.len() != y_data.len() {
            return (0.0, 0.0, 0.0);
        }
        let length = x_data.len();
        if length == 0 {
            return (0.0, 0.0, 0.0);
        }

        let mut peak = f64::MIN;
        let mut total_weight = 0.0;
        let mut sum_weighted_x = 0.0;

        for i in 0..length {
            let y = y_data[i];
            if y > peak {
                peak = y;
            }
            total_weight += y;
            sum_weighted_x += x_data[i] * y;
        }

        if total_weight < MIN_VALUE {
            return (0.0, 0.0, peak);
        }

        let inv_total_weight = 1.0 / total_weight;
        let mean = sum_weighted_x * inv_total_weight;

        let mut sum_weighted_sq_diff = 0.0;
        for i in 0..length {
            let diff = x_data[i] - mean;
            sum_weighted_sq_diff += diff * diff * y_data[i];
        }

        let variance = sum_weighted_sq_diff * inv_total_weight;
        let sigma = variance.max(0.0).sqrt();

        (mean, sigma, peak)
    }

    pub fn calculate_rms(&self, x_data: &[f64], y_data: &[f64], mean: f64) -> f64 {
        let mut sum_sq = 0.0;
        let mut total_w = 0.0;
        for i in 0..x_data.len() {
            total_w += y_data[i];
            let diff = x_data[i] - mean;
            sum_sq += diff * diff * y_data[i];
        }
        if total_w < MIN_VALUE {
            0.0
        } else {
            (sum_sq / total_w).sqrt()
        }
    }

    /// Full-width-at-half-maximum of the tallest peak in `y_data`, in
    /// `x_data` units - found by scanning outward from the peak bin for the
    /// first half-max crossing on each side, the same raw-data technique
    /// `perform_simple_fit` uses for its own initial-guess FWHM (see the ROI
    /// comment there). Unlike a fitted FWHM (`FittingResult::fwhm`, derived
    /// from a solved sigma/gamma), this needs no curve fit at all, so it's
    /// usable as a quick descriptive stat even when no fit overlay is
    /// enabled. Returns `0.0` for empty/all-non-positive input.
    pub fn calculate_fwhm(&self, x_data: &[f64], y_data: &[f64]) -> f64 {
        if x_data.len() != y_data.len() || x_data.is_empty() {
            return 0.0;
        }

        let mut max_index = 0;
        let mut max_y = y_data[0];
        for (i, &y) in y_data.iter().enumerate().skip(1) {
            if y > max_y {
                max_y = y;
                max_index = i;
            }
        }
        if max_y <= MIN_VALUE {
            return 0.0;
        }

        let half_max = max_y / 2.0;
        let mut left_idx = max_index;
        for i in (0..=max_index).rev() {
            if y_data[i] <= half_max {
                left_idx = i;
                break;
            }
        }
        let mut right_idx = max_index;
        for i in max_index..y_data.len() {
            if y_data[i] <= half_max {
                right_idx = i;
                break;
            }
        }

        (x_data[right_idx] - x_data[left_idx]).abs()
    }

    pub fn gaussian_fit(&self, x_data: &[f64], y_data: &[f64]) -> FittingResult {
        self.perform_simple_fit(x_data, y_data, true)
    }

    pub fn lorentzian_fit(&self, x_data: &[f64], y_data: &[f64]) -> FittingResult {
        self.perform_simple_fit(x_data, y_data, false)
    }

    fn perform_simple_fit(&self, x_data: &[f64], y_data: &[f64], is_gaussian: bool) -> FittingResult {
        if x_data.len() != y_data.len() || x_data.len() < 3 {
            return FittingResult::empty(0);
        }

        let len = y_data.len();
        let mut max_index = 0;
        let mut max_y = y_data[0];
        for i in 1..len {
            if y_data[i] > max_y {
                max_y = y_data[i];
                max_index = i;
            }
        }
        if max_y <= MIN_VALUE {
            return FittingResult::empty(x_data.len());
        }

        let scale = 1.0 / max_y;
        let y_norm: Vec<f64> = y_data.iter().map(|&y| y * scale).collect();
        let mu_guess = x_data[max_index];

        // Simple FWHM estimate
        let half_max = max_y / 2.0;
        let mut left_idx = max_index;
        for i in (0..=max_index).rev() {
            if y_data[i] <= half_max {
                left_idx = i;
                break;
            }
        }
        let mut right_idx = max_index;
        for i in max_index..y_data.len() {
            if y_data[i] <= half_max {
                right_idx = i;
                break;
            }
        }

        let mut fwhm_guess = x_data[right_idx] - x_data[left_idx];
        if fwhm_guess <= 0.0 {
            fwhm_guess = (x_data[x_data.len() - 1] - x_data[0]) * 0.1;
        }

        let sigma_guess = if is_gaussian { fwhm_guess / 2.355 } else { fwhm_guess / 2.0 };
        let initial_guess = [1.0, mu_guess, sigma_guess];

        // Crop to a region-of-interest around the peak before solving,
        // mirroring the HEMG solver's own `roi_sigma_multiplier` ROI
        // extraction (see `hemg::hemg_double_sided_fit`). Unlike the
        // original C# (`MathService.PerformSimpleFit`), which hands the
        // *full* un-cropped histogram to MathNet.Numerics' production-grade
        // `LevenbergMarquardtMinimizer`, this fits with a small hand-rolled
        // solver (no equivalent crate was pulled in - see module docs).
        // Handing that solver a mostly-empty 16384-point domain (only a
        // narrow window around the peak is ever non-zero) leaves it free to
        // walk `mu` toward whatever it likes across thousands of
        // uninformative zero bins - in practice it was converging to (or
        // never leaving) the low end of the domain, i.e. always "peaking at
        // the first value" regardless of where the visible data's peak
        // actually was. Restricting the domain to mu_guess +/- 8*sigma_guess
        // (same 8x multiplier HEMG uses) keeps every point in the fit
        // informative about the actual peak.
        const ROI_SIGMA_MULTIPLIER: f64 = 8.0;
        let fit_range = sigma_guess.abs().max(1.0) * ROI_SIGMA_MULTIPLIER;
        let min_x = mu_guess - fit_range;
        let max_x = mu_guess + fit_range;
        let mut roi_start = 0usize;
        let mut roi_end = x_data.len() - 1;
        for (i, &v) in x_data.iter().enumerate() {
            if v >= min_x {
                roi_start = i;
                break;
            }
        }
        for i in (0..x_data.len()).rev() {
            if x_data[i] <= max_x {
                roi_end = i;
                break;
            }
        }
        let (x_roi, y_roi): (&[f64], &[f64]) = if roi_end > roi_start && roi_end - roi_start >= 4 {
            (&x_data[roi_start..=roi_end], &y_norm[roi_start..=roi_end])
        } else {
            (x_data, &y_norm)
        };

        let model = |p: &[f64], xi: f64| -> f64 {
            let a = p[0];
            let mu = p[1];
            let width = p[2].max(MIN_VALUE);
            if is_gaussian {
                let denom = 2.0 * width * width;
                let exponent = -(xi - mu).powi(2) / denom;
                if exponent < -MAX_EXP_ARG {
                    0.0
                } else {
                    a * exponent.exp()
                }
            } else {
                let w_sq = width * width;
                (a * w_sq) / ((xi - mu).powi(2) + w_sq)
            }
        };

        let p_final = levenberg_marquardt(&initial_guess, x_roi, y_roi, model);

        if p_final.iter().any(|v| v.is_nan() || v.is_infinite()) {
            return FittingResult::empty(x_data.len());
        }

        let final_amp_real = p_final[0] * max_y;
        let final_mean = p_final[1];
        let final_sigma = p_final[2];
        let mut fit_curve = vec![0.0; x_data.len()];

        if is_gaussian {
            let safe_sigma = final_sigma.abs().max(MIN_VALUE);
            let denom = 2.0 * safe_sigma * safe_sigma;
            for i in 0..x_data.len() {
                let exponent = -(x_data[i] - final_mean).powi(2) / denom;
                fit_curve[i] = if exponent < -MAX_EXP_ARG {
                    0.0
                } else {
                    final_amp_real * exponent.exp()
                };
            }
        } else {
            for i in 0..x_data.len() {
                fit_curve[i] = calculate_lorentzian_value(x_data[i], final_amp_real, final_mean, final_sigma);
            }
        }

        let final_fwhm = if is_gaussian {
            2.355 * final_sigma.abs()
        } else {
            2.0 * final_sigma.abs()
        };
        let final_res = if final_mean.abs() > MIN_VALUE {
            final_fwhm / final_mean * 100.0
        } else {
            0.0
        };

        let mut result = FittingResult::simple(fit_curve.clone(), final_mean, final_sigma, final_amp_real, 0.0);
        result.fwhm = final_fwhm;
        result.resolution = final_res;
        calculate_fit_stats(&mut result, x_data, y_data, &fit_curve);

        let roi_min = x_roi.iter().copied().fold(f64::INFINITY, f64::min);
        let roi_max = x_roi.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let roi_span = roi_max - roi_min;
        let left_pitch = max_index
            .checked_sub(1)
            .map(|i| x_data[max_index] - x_data[i])
            .filter(|pitch| pitch.is_finite() && *pitch > 0.0);
        let right_pitch = x_data
            .get(max_index + 1)
            .map(|&x| x - x_data[max_index])
            .filter(|pitch| pitch.is_finite() && *pitch > 0.0);
        let local_pitch = match (left_pitch, right_pitch) {
            (Some(left), Some(right)) => left.max(right),
            (Some(pitch), None) | (None, Some(pitch)) => pitch,
            (None, None) => return FittingResult::empty(x_data.len()),
        };
        if !final_amp_real.is_finite()
            || final_amp_real <= 0.0
            || !final_sigma.is_finite()
            || final_sigma <= 0.0
            || !final_mean.is_finite()
            || !roi_span.is_finite()
            || final_sigma > roi_span
            || final_mean < roi_min
            || final_mean > roi_max
            || !result.r_squared.is_finite()
            || result.r_squared <= 0.0
            || !local_pitch.is_finite()
            || local_pitch <= 0.0
            || !final_fwhm.is_finite()
            || final_fwhm < local_pitch
        {
            return FittingResult::empty(x_data.len());
        }
        result
    }

    pub fn hyper_emg_fit(&self, x_data: &[f64], y_data: &[f64]) -> FittingResult {
        self.hemg_double_sided_fit(x_data, y_data, None, None)
    }

    pub fn hemg_single_sided_fit(&self, x_data: &[f64], y_data: &[f64]) -> FittingResult {
        self.hemg_double_sided_fit(x_data, y_data, None, None)
    }

    pub fn hyper_emg_double_sided_fit(&self, x_data: &[f64], y_data: &[f64]) -> FittingResult {
        self.hemg_double_sided_fit(x_data, y_data, None, None)
    }

    pub fn hemg_double_sided_fit(
        &self,
        x_data: &[f64],
        y_data: &[f64],
        _raw_data: Option<&[f64]>,
        config: Option<&HemgFitConfig>,
    ) -> FittingResult {
        if x_data.len() < 10 || x_data.len() != y_data.len() {
            return FittingResult::empty(x_data.len());
        }
        let owned_config;
        let config = match config {
            Some(c) => c,
            None => {
                owned_config = HemgFitConfig::default();
                &owned_config
            }
        };

        let (fit_curve, parameters) = hemg::hemg_double_sided_fit(x_data, y_data, None, None, config);

        if fit_curve.is_empty() || parameters.len() < 7 {
            return FittingResult::empty(x_data.len());
        }

        let real_peak_height = hemg::calculate_max_height(&parameters);

        let mut result = FittingResult {
            is_valid: true,
            fit_curve: fit_curve.clone(),
            a: parameters[0],
            peak: real_peak_height,
            mu: parameters[1],
            sigma: parameters[2],
            tau_l1: parameters[3],
            tau_r1: parameters[4],
            eta_l1: parameters[5],
            eta_r1: parameters[6],
            ..Default::default()
        };
        calculate_fit_stats(&mut result, x_data, y_data, &fit_curve);
        result
    }

    pub fn hemg_double_sided_fit_manual(
        &self,
        x_data: &[f64],
        y_data: &[f64],
        initial_guess: Option<&[f64]>,
        fixed_params: Option<&[bool]>,
    ) -> FittingResult {
        if x_data.len() < 10 || x_data.len() != y_data.len() {
            return FittingResult::empty(x_data.len());
        }
        let config = HemgFitConfig::default();
        let (fit_curve, parameters) =
            hemg::hemg_double_sided_fit(x_data, y_data, initial_guess, fixed_params, &config);

        if fit_curve.is_empty() || parameters.len() < 7 {
            return FittingResult::empty(x_data.len());
        }

        let mut result = FittingResult {
            is_valid: true,
            fit_curve: fit_curve.clone(),
            a: parameters[0],
            peak: parameters[0],
            mu: parameters[1],
            sigma: parameters[2],
            tau_l1: parameters[3],
            tau_r1: parameters[4],
            eta_l1: parameters[5],
            eta_r1: parameters[6],
            ..Default::default()
        };
        calculate_fit_stats(&mut result, x_data, y_data, &fit_curve);
        result
    }

    pub fn generate_hemg_curve(&self, x: &[f64], parameters: &[f64]) -> Vec<f64> {
        hemg::generate_hemg_curve(x, parameters)
    }
}

pub fn calculate_lorentzian_value(x: f64, a: f64, x0: f64, gamma: f64) -> f64 {
    if gamma.abs() < MIN_VALUE {
        return if (x - x0).abs() < MIN_VALUE { a } else { 0.0 };
    }
    let gamma_sq = gamma * gamma;
    (a * gamma_sq) / ((x - x0).powi(2) + gamma_sq)
}

/// Evaluates a Gaussian with the same amplitude/mean/sigma convention as
/// `perform_simple_fit`'s own curve reconstruction (see its `is_gaussian`
/// branch) - exposed so callers can reconstruct a fitted curve over an x
/// range other than the one actually handed to `gaussian_fit` (e.g. a full
/// histogram domain after fitting only a cropped peak window).
pub fn calculate_gaussian_value(x: f64, a: f64, mu: f64, sigma: f64) -> f64 {
    let safe_sigma = sigma.abs().max(MIN_VALUE);
    let exponent = -(x - mu).powi(2) / (2.0 * safe_sigma * safe_sigma);
    if exponent < -MAX_EXP_ARG {
        0.0
    } else {
        a * exponent.exp()
    }
}

fn calculate_fit_stats(result: &mut FittingResult, x_data: &[f64], y_data: &[f64], fit_curve: &[f64]) {
    let n = y_data.len();
    if n == 0 {
        return;
    }

    let y_mean = y_data.iter().sum::<f64>() / n as f64;
    let mut ssr = 0.0;
    let mut ss_tot = 0.0;

    for i in 0..n {
        let diff = y_data[i] - fit_curve[i];
        ssr += diff * diff;
        ss_tot += (y_data[i] - y_mean).powi(2);
    }

    result.rms = (ssr / n as f64).sqrt();
    result.r_squared = if ss_tot > MIN_VALUE { 1.0 - (ssr / ss_tot) } else { 0.0 };

    if result.fwhm.abs() < MIN_VALUE {
        result.fwhm = result.sigma * 2.355;
    }
    if result.mu.abs() > MIN_VALUE && result.resolution.abs() < MIN_VALUE {
        result.resolution = (result.fwhm / result.mu) * 100.0;
    }
    let _ = x_data;
}

/// Small Levenberg-Marquardt solver with a forward-difference Jacobian and
/// Gaussian-elimination normal-equations solve, used for the 3-parameter
/// Gaussian/Lorentzian fits (standing in for MathNet's `LevenbergMarquardtMinimizer`).
fn levenberg_marquardt(p0: &[f64], x: &[f64], y: &[f64], model: impl Fn(&[f64], f64) -> f64) -> Vec<f64> {
    let n = p0.len();
    let m = x.len();
    let mut p = p0.to_vec();
    let eps = 1e-6;
    let mut lambda = 1e-3;
    const MAX_ITER: usize = 200;
    const TOLERANCE: f64 = 1e-12;

    let residual_sq_sum = |p: &[f64]| -> f64 {
        (0..m).map(|i| { let d = y[i] - model(p, x[i]); d * d }).sum()
    };

    let mut current_error = residual_sq_sum(&p);

    for _ in 0..MAX_ITER {
        let mut residuals = vec![0.0; m];
        let mut j_flat = vec![0.0; m * n];
        for i in 0..m {
            let base = model(&p, x[i]);
            residuals[i] = y[i] - base;
            for k in 0..n {
                let mut pp = p.clone();
                pp[k] += eps;
                let perturbed = model(&pp, x[i]);
                j_flat[i * n + k] = (perturbed - base) / eps;
            }
        }

        let mut jt_j = vec![0.0; n * n];
        let mut jt_res = vec![0.0; n];
        for i in 0..m {
            let res = residuals[i];
            for j in 0..n {
                jt_res[j] += j_flat[i * n + j] * res;
                for k in 0..n {
                    jt_j[j * n + k] += j_flat[i * n + j] * j_flat[i * n + k];
                }
            }
        }

        for d in 0..n {
            jt_j[d * n + d] += lambda * (jt_j[d * n + d] + 1e-10);
        }

        let mut delta = vec![0.0; n];
        if solve_gaussian(&jt_j, &jt_res, &mut delta, n) {
            let p_new: Vec<f64> = (0..n).map(|i| p[i] + delta[i]).collect();
            let new_error = residual_sq_sum(&p_new);

            if new_error < current_error {
                lambda /= 10.0;
                let converged = (current_error - new_error).abs() < TOLERANCE * current_error.max(1e-300);
                current_error = new_error;
                p = p_new;
                if converged {
                    break;
                }
            } else {
                lambda *= 10.0;
            }
        } else {
            lambda *= 10.0;
        }

        if lambda > 1e10 {
            break;
        }
    }

    p
}

fn solve_gaussian(a: &[f64], b: &[f64], x: &mut [f64], n: usize) -> bool {
    let mut m = a.to_vec();
    x[..n].copy_from_slice(&b[..n]);

    for k in 0..n {
        let mut pivot = k;
        let mut max_val = m[k * n + k].abs();
        for i in (k + 1)..n {
            if m[i * n + k].abs() > max_val {
                max_val = m[i * n + k].abs();
                pivot = i;
            }
        }
        if pivot != k {
            for j in k..n {
                m.swap(k * n + j, pivot * n + j);
            }
            x.swap(k, pivot);
        }
        if m[k * n + k].abs() < 1e-20 {
            return false;
        }
        for i in (k + 1)..n {
            let factor = m[i * n + k] / m[k * n + k];
            x[i] -= factor * x[k];
            for j in k..n {
                m[i * n + j] -= factor * m[k * n + j];
            }
        }
    }

    for i in (0..n).rev() {
        let mut sum = 0.0;
        for j in (i + 1)..n {
            sum += m[i * n + j] * x[j];
        }
        x[i] = (x[i] - sum) / m[i * n + i];
    }
    true
}

//! Direct transcription of `Infrastructure/Services/HemgFittingService.cs`.
//!
//! The Levenberg-Marquardt optimizer, its Gaussian-elimination linear solver,
//! and the Abramowitz-Stegun `erfc` approximation are all hand-rolled in the
//! original C# (not backed by a math library), so they are transcribed here
//! verbatim rather than replaced by a crate, to preserve exact numeric
//! behavior (damping schedule, convergence tolerance, parameter clamping).

use crate::models::baseline::HemgFitConfig;
use rayon::prelude::*;

const MAX_EXP_ARG: f64 = 700.0;
const SQRT_2PI: f64 = 2.506628274631000;
const INV_SQRT_2: f64 = 0.7071067811865475;

/// p = [A, mu, sigma, tauL, tauR, etaL, etaR]
pub fn calculate_max_height(p: &[f64]) -> f64 {
    if p.len() < 7 {
        return 0.0;
    }
    hyper_emg_kernel(p[1], p[0], p[1], p[2], p[3], p[4], p[5], p[6])
}

pub fn generate_hemg_curve(x: &[f64], p: &[f64]) -> Vec<f64> {
    if p.len() < 7 {
        return Vec::new();
    }
    x.par_iter()
        .map(|&xi| hyper_emg_kernel(xi, p[0], p[1], p[2], p[3], p[4], p[5], p[6]))
        .collect()
}

pub fn hemg_double_sided_fit_raw(
    thresholded_data: &[f64],
    config: &HemgFitConfig,
) -> (Vec<f64>, Vec<f64>) {
    let (_, centers, counts) = create_histogram(thresholded_data, config.histogram_bin_count);
    hemg_double_sided_fit(&centers, &counts, None, None, config)
}

pub fn hemg_double_sided_fit(
    bin_centers: &[f64],
    counts: &[f64],
    initial_guess: Option<&[f64]>,
    fixed_params: Option<&[bool]>,
    config: &HemgFitConfig,
) -> (Vec<f64>, Vec<f64>) {
    // C# validates only `binCenters.Length < 10` and relies on a caller-side
    // try/catch to turn any subsequent out-of-range access into an empty
    // result; Rust has no equivalent safety net, so the `counts` length is
    // checked explicitly here to avoid a panic on malformed/mismatched input.
    if bin_centers.len() < 10 || counts.len() < bin_centers.len() {
        return (vec![0.0; bin_centers.len()], vec![0.0; 7]);
    }

    let mut p0 = match initial_guess {
        Some(g) if g.len() >= 7 => g.to_vec(),
        _ => {
            // 1. Peak Detection
            let (mu0, sigma0, height0) = find_peak_parameters(bin_centers, counts);

            // 2. Initial Guess
            let tau_l0 = sigma0 * 0.5;
            let tau_r0 = sigma0 * 0.5;
            let model_at_peak = hyper_emg_kernel(
                mu0,
                1.0,
                mu0,
                sigma0,
                tau_l0,
                tau_r0,
                config.initial_eta,
                config.initial_eta,
            );
            let a0 = if model_at_peak > 1e-15 {
                height0 / model_at_peak
            } else {
                height0
            };
            vec![a0, mu0, sigma0, tau_l0, tau_r0, config.initial_eta, config.initial_eta]
        }
    };
    if p0.len() < 7 {
        p0.resize(7, 0.0);
    }

    // 3. ROI Extraction
    let mut start_bin = 0usize;
    let mut end_bin = bin_centers.len() - 1;
    let fit_range = p0[2] * config.roi_sigma_multiplier;
    let min_x = p0[1] - fit_range;
    let max_x = p0[1] + fit_range;

    for (i, &v) in bin_centers.iter().enumerate() {
        if v >= min_x {
            start_bin = i;
            break;
        }
    }
    for i in (0..bin_centers.len()).rev() {
        if bin_centers[i] <= max_x {
            end_bin = i;
            break;
        }
    }

    if end_bin < start_bin {
        let fit_curve = generate_hemg_curve(bin_centers, &p0);
        return (fit_curve, p0);
    }
    let roi_len = end_bin - start_bin + 1;
    if roi_len < 5 {
        let fit_curve = generate_hemg_curve(bin_centers, &p0);
        return (fit_curve, p0);
    }

    let x_roi = &bin_centers[start_bin..=end_bin];
    let y_roi = &counts[start_bin..=end_bin];

    // 4. Optimization (Levenberg-Marquardt)
    let p_fit = fit_levenberg_marquardt(&p0, x_roi, y_roi, fixed_params, config);

    // 5. Generate Curve
    let fit_curve = generate_hemg_curve(bin_centers, &p_fit);

    (fit_curve, p_fit)
}

fn fit_levenberg_marquardt(
    p0: &[f64],
    x: &[f64],
    y: &[f64],
    fixed_params: Option<&[bool]>,
    config: &HemgFitConfig,
) -> Vec<f64> {
    let n = p0.len(); // 7 parameters
    let m = x.len();
    let mut p = p0.to_vec();
    let default_locks = vec![false; n];
    let locks: &[bool] = fixed_params.unwrap_or(&default_locks);

    let mut j_flat = vec![0.0; m * n];
    let mut residuals = vec![0.0; m];
    let mut p_new = vec![0.0; n];
    let mut jt_j = vec![0.0; n * n];
    let mut jt_res = vec![0.0; n];
    let mut delta = vec![0.0; n];

    let mut lambda = config.lambda;
    let max_iter = config.max_iterations;

    let mut current_error = calculate_error_parallel(&p, x, y, Some(&mut residuals));

    for _iter in 0..max_iter {
        // 1. Calculate Jacobian (parallel) with locks
        calculate_jacobian_parallel(&p, x, &residuals, y, &mut j_flat, locks, config);

        // 2. Compute JtJ and JtRes
        jt_j.iter_mut().for_each(|v| *v = 0.0);
        jt_res.iter_mut().for_each(|v| *v = 0.0);

        for i in 0..m {
            let res = residuals[i];
            let row_offset = i * n;
            for j in 0..n {
                if locks[j] {
                    continue;
                }
                let val_j = j_flat[row_offset + j];
                jt_res[j] += val_j * res;
                for k in j..n {
                    if locks[k] {
                        continue;
                    }
                    jt_j[j * n + k] += val_j * j_flat[row_offset + k];
                }
            }
        }

        for j in 0..n {
            for k in 0..j {
                jt_j[j * n + k] = jt_j[k * n + j];
            }
        }

        let mut diag_backup = vec![0.0; n];
        for i in 0..n {
            if locks[i] {
                jt_j[i * n + i] = 1.0;
                jt_res[i] = 0.0;
            } else {
                diag_backup[i] = jt_j[i * n + i];
                jt_j[i * n + i] += lambda * (diag_backup[i] + 1e-5);
            }
        }

        if solve_linear_system_gaussian(&jt_j, &jt_res, &mut delta, n) {
            for i in 0..n {
                p_new[i] = if !locks[i] { p[i] + delta[i] } else { p[i] };
            }
            enforce_constraints(&mut p_new, config);
            for i in 0..n {
                if locks[i] {
                    p_new[i] = p[i];
                }
            }

            let new_error = calculate_error_parallel(&p_new, x, y, None);

            if new_error < current_error {
                lambda /= 10.0;
                // NB: matches the C# original exactly, including its quirk of
                // assigning `current_error = new_error` *before* comparing the
                // two, which makes the diff always 0 and the loop break right
                // after the first improving step (see HemgFittingService.cs,
                // FitLevenbergMarquardtParallel). Not "fixed" here on purpose
                // to preserve identical fit output.
                current_error = new_error;
                p.copy_from_slice(&p_new);
                if (current_error - new_error).abs() < config.convergence_tolerance * current_error {
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

fn calculate_jacobian_parallel(
    p: &[f64],
    x: &[f64],
    pre_calc_residuals: &[f64],
    y: &[f64],
    j_flat: &mut [f64],
    locks: &[bool],
    config: &HemgFitConfig,
) {
    let n = p.len();
    let eps = config.jacobian_epsilon;

    j_flat
        .par_chunks_mut(n)
        .enumerate()
        .for_each(|(i, row)| {
            let xi = x[i];
            let model_base = y[i] - pre_calc_residuals[i];

            for k in 0..7 {
                if locks[k] {
                    row[k] = 0.0;
                    continue;
                }
                let mut pp = [p[0], p[1], p[2], p[3], p[4], p[5], p[6]];
                pp[k] += eps;
                let perturbed = hyper_emg_kernel(xi, pp[0], pp[1], pp[2], pp[3], pp[4], pp[5], pp[6]);
                row[k] = (perturbed - model_base) / eps;
            }
        });
}

fn calculate_error_parallel(p: &[f64], x: &[f64], y: &[f64], mut residuals_out: Option<&mut [f64]>) -> f64 {
    match residuals_out.as_deref_mut() {
        Some(residuals) => x
            .par_iter()
            .zip(y.par_iter())
            .zip(residuals.par_iter_mut())
            .map(|((&xi, &yi), res_slot)| {
                let model = hyper_emg_kernel(xi, p[0], p[1], p[2], p[3], p[4], p[5], p[6]);
                let diff = yi - model;
                *res_slot = diff;
                diff * diff
            })
            .sum(),
        None => x
            .par_iter()
            .zip(y.par_iter())
            .map(|(&xi, &yi)| {
                let model = hyper_emg_kernel(xi, p[0], p[1], p[2], p[3], p[4], p[5], p[6]);
                let diff = yi - model;
                diff * diff
            })
            .sum(),
    }
}

/// Optimized Gaussian elimination with partial pivoting (no allocation beyond
/// the working copy), matching the C# flat-array implementation.
fn solve_linear_system_gaussian(a: &[f64], b: &[f64], x: &mut [f64], n: usize) -> bool {
    let mut m = a.to_vec(); // n*n working copy, since elimination is destructive
    x[..n].copy_from_slice(&b[..n]);

    for k in 0..n {
        // Pivot
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
            return false; // Singular
        }

        for i in (k + 1)..n {
            let factor = m[i * n + k] / m[k * n + k];
            x[i] -= factor * x[k];
            for j in k..n {
                m[i * n + j] -= factor * m[k * n + j];
            }
        }
    }

    // Back substitution
    for i in (0..n).rev() {
        let mut sum = 0.0;
        for j in (i + 1)..n {
            sum += m[i * n + j] * x[j];
        }
        x[i] = (x[i] - sum) / m[i * n + i];
    }
    true
}

/// The critical hot path: hardcoded for 1 left, 1 right tail.
#[inline]
fn hyper_emg_kernel(
    x: f64,
    a: f64,
    mu: f64,
    sigma: f64,
    tau_l: f64,
    tau_r: f64,
    eta_l: f64,
    eta_r: f64,
) -> f64 {
    let mut total_y = 0.0;
    let mut eta_sum = 0.0;
    let diff = x - mu;
    let sigma_sq = sigma * sigma;

    // Left tail
    if tau_l > 1e-9 {
        eta_sum += eta_l;
        let inv_tau = 1.0 / tau_l;
        let z = (sigma_sq * 0.5 * inv_tau * inv_tau) + (diff * inv_tau);
        if z < MAX_EXP_ARG {
            let arg = (sigma * inv_tau + diff / sigma) * INV_SQRT_2;
            total_y += eta_l * (0.5 * inv_tau) * z.exp() * erfc_fast(arg);
        }
    }

    // Right tail
    if tau_r > 1e-9 {
        eta_sum += eta_r;
        let inv_tau = 1.0 / tau_r;
        let z = (sigma_sq * 0.5 * inv_tau * inv_tau) - (diff * inv_tau);
        if z < MAX_EXP_ARG {
            let arg = (sigma * inv_tau - diff / sigma) * INV_SQRT_2;
            total_y += eta_r * (0.5 * inv_tau) * z.exp() * erfc_fast(arg);
        }
    }

    // Gaussian core
    let eta_g = 1.0 - eta_sum;
    if eta_g > 0.0 {
        let r = diff / sigma;
        total_y += eta_g * (-0.5 * r * r).exp() / (sigma * SQRT_2PI);
    }

    total_y * a
}

/// Complementary error function, Abramowitz & Stegun 7.1.26 approximation
/// (|error| <= 1.5e-7) - transcribed exactly as in the C# source.
#[inline]
fn erfc_fast(x: f64) -> f64 {
    if x < 0.0 {
        return 2.0 - erfc_fast(-x);
    }
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let poly = ((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t
        + 0.254829592)
        * t;
    poly * (-x * x).exp()
}

fn enforce_constraints(p: &mut [f64], config: &HemgFitConfig) {
    let min_val = config.min_parameter_value;
    let max_total_eta = config.max_total_eta;

    if p[0] < 0.0 {
        p[0] = min_val;
    }
    if p[2] < min_val {
        p[2] = min_val;
    }
    if p[3] < min_val {
        p[3] = min_val;
    }
    if p[4] < min_val {
        p[4] = min_val;
    }
    p[5] = p[5].clamp(1e-6, max_total_eta - 0.04);
    let remaining = max_total_eta - p[5];
    p[6] = p[6].clamp(1e-6, remaining);
}

fn find_peak_parameters(x: &[f64], y: &[f64]) -> (f64, f64, f64) {
    let mut max_height = -1.0;
    let mut max_idx = 0usize;
    for (i, &v) in y.iter().enumerate() {
        if v > max_height {
            max_height = v;
            max_idx = i;
        }
    }
    let mu = x[max_idx];
    let half_max = max_height * 0.5;
    let mut left_idx = max_idx;
    while left_idx > 0 && y[left_idx] > half_max {
        left_idx -= 1;
    }
    let mut right_idx = max_idx;
    while right_idx < y.len() - 1 && y[right_idx] > half_max {
        right_idx += 1;
    }
    let mut fwhm = x[right_idx] - x[left_idx];
    if fwhm <= 0.0 {
        let x_max = x.iter().cloned().fold(f64::MIN, f64::max);
        let x_min = x.iter().cloned().fold(f64::MAX, f64::min);
        fwhm = (x_max - x_min) / 100.0;
    }
    (mu, fwhm / 2.355, max_height)
}

fn create_histogram(data: &[f64], num_bins: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut edges = vec![0.0; num_bins + 1];
    let mut centers = vec![0.0; num_bins];
    let mut counts = vec![0.0; num_bins];
    for i in 0..=num_bins {
        edges[i] = i as f64;
    }
    for i in 0..num_bins {
        centers[i] = edges[i] + 0.5;
    }

    let max_bin_idx = num_bins as i64 - 1;
    for &val in data {
        let mut bin = val as i64; // truncation toward zero, matches C# (int) cast
        if bin >= num_bins as i64 {
            bin = max_bin_idx;
        }
        if bin < 0 {
            bin = 0;
        }
        counts[bin as usize] += 1.0;
    }
    (edges, centers, counts)
}

//! Shared Gaussian/Lorentzian/HEMG fit-overlay computation for the
//! per-channel histograms in Baseline and Calibration mode.
//!
//! Baseline and Calibration mode previously handed their solvers a
//! histogram spanning the *entire* configured axis range (16384 ADC bins
//! for Baseline, 500 bins for Calibration) and relied entirely on each fit's
//! own internal region-of-interest narrowing (`ROI_SIGMA_MULTIPLIER` in
//! `baseline_core::math::fit`) to find the real peak - which only works once
//! an initial peak/FWHM guess has already been taken over the *full* domain.
//! Observation mode instead crops to a window around the histogram's peak
//! *before* calling the fit at all (see `observation_mode.rs`'s
//! `compute_fits`/`FIT_WINDOW` doc comment); this module applies that same
//! crop-before-fit approach here, which:
//! - keeps the initial peak/FWHM guess (and the `rms`/`r_squared` goodness-
//!   of-fit stats) confined to the peak's own neighborhood, instead of being
//!   diluted by thousands of unrelated near-empty bins elsewhere in the
//!   histogram - relevant for e.g. Baseline's "After" (mean-subtracted) mode
//!   histograms, which can have other structure far from the pedestal peak;
//! - bounds the solver's per-iteration cost to a small fixed window rather
//!   than the full axis range.
//!
//! This is *not* a fix for a spurious bin outright outweighing the real
//! peak (e.g. ADC rail/clipping artifacts) - that's what Observation mode's
//! `histogram_of_positive` guards against for particle pulse-height data,
//! but it isn't safe to reuse here: Baseline's "After"/"After (log)" modes
//! legitimately produce negative, mean-subtracted values, so filtering
//! non-positive samples would silently discard real data in those modes.
//!
//! Unlike Observation's `ObsFitCurve` (which only plots the cropped window,
//! offset-aligned), the fitted parameters here are reconstructed into a
//! curve spanning the *full* `bin_centers` domain, so `FitCurve`/
//! `ChannelState` (shared with `channel.rs`'s rendering, which expects
//! `curve.len() == bin_centers.len()`) needs no changes.

use baseline_core::math::fit::{calculate_gaussian_value, calculate_lorentzian_value};
use baseline_core::math::MathService;
use baseline_core::models::baseline::FittingResult;
use egui::Color32;

use crate::channel::FitCurve;

/// Mirrors Observation mode's `FIT_WINDOW`: the number of bins on either
/// side of the histogram's peak bin included in the fit.
const FIT_WINDOW: usize = 100;

#[derive(Debug, Clone, Copy, Default)]
pub struct FitOverlayFlags {
    pub gaussian: bool,
    pub lorentzian: bool,
    pub hemg_single: bool,
    pub hemg_double: bool,
}

impl FitOverlayFlags {
    pub fn any(&self) -> bool {
        self.gaussian || self.lorentzian || self.hemg_single || self.hemg_double
    }
}

/// Primary-fit stats mirrored into `ChannelState`'s `mu`/`sigma`/`fwhm`/
/// `resolution` fields - taken from the Gaussian fit when enabled, matching
/// the pre-existing Baseline mode behavior of using Gaussian as the
/// "headline" fit.
#[derive(Debug, Clone, Copy, Default)]
pub struct FitOverlayStats {
    pub mu: f64,
    pub sigma: f64,
    pub fwhm: f64,
    pub resolution: f64,
}

/// Finds the histogram's peak bin and returns the `[x, y]` slices cropped to
/// `peak +/- FIT_WINDOW`. Returns `None` if the histogram is empty/all-zero
/// or the resulting window is too small to fit (mirrors Observation mode's
/// `compute_fits` bail-out conditions).
fn crop_around_peak<'a>(bin_centers: &'a [f64], counts: &'a [f64]) -> Option<(&'a [f64], &'a [f64])> {
    let peak_idx = counts
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .filter(|&(_, &c)| c > 0.0)
        .map(|(i, _)| i)?;

    let start = peak_idx.saturating_sub(FIT_WINDOW);
    let end = (peak_idx + FIT_WINDOW + 1).min(counts.len());
    if end - start < 5 {
        return None;
    }
    Some((&bin_centers[start..end], &counts[start..end]))
}

/// Fits a single left-tailed EMG by seeding from an ordinary double-sided
/// fit and re-solving with the right tail's `tau`/`eta` locked at zero
/// (`hemg_double_sided_fit_manual`'s `fixed_params`); the double-sided fit's
/// own `eta_r`, left unlocked, would otherwise always converge close to the
/// same curve, making a "single" and "double" checkbox indistinguishable.
fn fit_hemg_single_sided(math: &MathService, x: &[f64], y: &[f64]) -> Option<FittingResult> {
    let seed = math.hemg_double_sided_fit(x, y, None, None);
    if !seed.is_valid {
        return None;
    }
    let initial = [seed.a, seed.mu, seed.sigma, seed.tau_l1, seed.tau_r1, seed.eta_l1, 0.0];
    let locks = [false, false, false, false, true, false, true];
    let res = math.hemg_double_sided_fit_manual(x, y, Some(&initial), Some(&locks));
    res.is_valid.then_some(res)
}

/// Computes (and returns) the requested fit-overlay curves for one channel's
/// histogram, plus the Gaussian-derived primary stats. `bin_centers`/`counts`
/// must be the same length; returns `(FitOverlayStats::default(), vec![])`
/// if no flags are set, the histogram is empty, or the peak window is too
/// small to fit.
pub fn compute_fits(math: &MathService, bin_centers: &[f64], counts: &[f64], flags: &FitOverlayFlags) -> (FitOverlayStats, Vec<FitCurve>) {
    let mut stats = FitOverlayStats::default();
    let mut fits = Vec::new();

    if !flags.any() {
        return (stats, fits);
    }
    let Some((x_win, y_win)) = crop_around_peak(bin_centers, counts) else {
        return (stats, fits);
    };

    if flags.gaussian {
        let res = math.gaussian_fit(x_win, y_win);
        if res.is_valid && !res.fit_curve.is_empty() {
            stats = FitOverlayStats { mu: res.mu, sigma: res.sigma, fwhm: res.fwhm, resolution: res.resolution };
            let curve = bin_centers.iter().map(|&x| calculate_gaussian_value(x, res.peak, res.mu, res.sigma)).collect();
            fits.push(FitCurve { curve, color: Color32::from_rgb(50, 220, 50), label: "Gaussian".to_string() });
        }
    }
    if flags.hemg_single {
        if let Some(res) = fit_hemg_single_sided(math, x_win, y_win) {
            let curve = math.generate_hemg_curve(bin_centers, &[res.a, res.mu, res.sigma, res.tau_l1, res.tau_r1, res.eta_l1, res.eta_r1]);
            fits.push(FitCurve { curve, color: Color32::RED, label: "HEMG single".to_string() });
        }
    }
    if flags.hemg_double {
        let res = math.hemg_double_sided_fit(x_win, y_win, None, None);
        if res.is_valid && !res.fit_curve.is_empty() {
            let curve = math.generate_hemg_curve(bin_centers, &[res.a, res.mu, res.sigma, res.tau_l1, res.tau_r1, res.eta_l1, res.eta_r1]);
            fits.push(FitCurve { curve, color: Color32::from_rgb(220, 50, 220), label: "HEMG double".to_string() });
        }
    }
    if flags.lorentzian {
        let res = math.lorentzian_fit(x_win, y_win);
        if res.is_valid && !res.fit_curve.is_empty() {
            let curve = bin_centers.iter().map(|&x| calculate_lorentzian_value(x, res.peak, res.mu, res.sigma)).collect();
            fits.push(FitCurve { curve, color: Color32::from_rgb(0, 220, 220), label: "Lorentzian".to_string() });
        }
    }

    (stats, fits)
}


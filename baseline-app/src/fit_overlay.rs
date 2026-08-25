use baseline_core::math::fit::{calculate_gaussian_value, calculate_lorentzian_value};
use baseline_core::math::MathService;
use baseline_core::models::baseline::FittingResult;
use egui::Color32;

use crate::channel::FitCurve;

/// Fit Window: The number of bins on either side of the histogram's peak bin included in the fit.
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

/// Primary-fit stats `mu`/`sigma`/`fwhm`/`resolution` are derived from the Gaussian fit, if requested and valid; otherwise they remain zero.
#[derive(Debug, Clone, Copy, Default)]
pub struct FitOverlayStats {
    pub mu: f64,
    pub sigma: f64,
    pub fwhm: f64,
    pub resolution: f64,
}

/// Finds the histogram's peak bin and returns the `[x, y]` slices cropped to
/// `peak +/- FIT_WINDOW`. Returns `None` if the histogram is empty/all-zero
/// or the resulting window is too small to fit
fn crop_around_peak<'a>(
    bin_centers: &'a [f64],
    counts: &'a [f64],
) -> Option<(&'a [f64], &'a [f64])> {
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
fn fit_hemg_single_sided(math: &MathService, x: &[f64], y: &[f64]) -> Option<FittingResult> {
    let seed = math.hemg_double_sided_fit(x, y, None, None);
    if !seed.is_valid {
        return None;
    }
    let initial = [
        seed.a,
        seed.mu,
        seed.sigma,
        seed.tau_l1,
        seed.tau_r1,
        seed.eta_l1,
        0.0,
    ];
    let locks = [false, false, false, false, true, false, true];
    let res = math.hemg_double_sided_fit_manual(x, y, Some(&initial), Some(&locks));
    res.is_valid.then_some(res)
}

/// Computes (and returns) the requested fit-overlay curves for one channel's
/// histogram, plus the Gaussian-derived primary stats. `bin_centers`/`counts`
/// must be the same length; returns `(FitOverlayStats::default(), vec![])`
/// if no flags are set, the histogram is empty, or the peak window is too
/// small to fit.
pub fn compute_fits(
    math: &MathService,
    bin_centers: &[f64],
    counts: &[f64],
    flags: &FitOverlayFlags,
) -> (FitOverlayStats, Vec<FitCurve>) {
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
            stats = FitOverlayStats {
                mu: res.mu,
                sigma: res.sigma,
                fwhm: res.fwhm,
                resolution: res.resolution,
            };
            let curve = bin_centers
                .iter()
                .map(|&x| calculate_gaussian_value(x, res.peak, res.mu, res.sigma))
                .collect();
            fits.push(FitCurve {
                curve,
                color: Color32::from_rgb(50, 220, 50),
                label: "Gaussian".to_string(),
            });
        }
    }
    if flags.hemg_single {
        if let Some(res) = fit_hemg_single_sided(math, x_win, y_win) {
            let curve = math.generate_hemg_curve(
                bin_centers,
                &[
                    res.a, res.mu, res.sigma, res.tau_l1, res.tau_r1, res.eta_l1, res.eta_r1,
                ],
            );
            fits.push(FitCurve {
                curve,
                color: Color32::RED,
                label: "HEMG single".to_string(),
            });
        }
    }
    if flags.hemg_double {
        let res = math.hemg_double_sided_fit(x_win, y_win, None, None);
        if res.is_valid && !res.fit_curve.is_empty() {
            let curve = math.generate_hemg_curve(
                bin_centers,
                &[
                    res.a, res.mu, res.sigma, res.tau_l1, res.tau_r1, res.eta_l1, res.eta_r1,
                ],
            );
            fits.push(FitCurve {
                curve,
                color: Color32::from_rgb(220, 50, 220),
                label: "HEMG double".to_string(),
            });
        }
    }
    if flags.lorentzian {
        let res = math.lorentzian_fit(x_win, y_win);
        if res.is_valid && !res.fit_curve.is_empty() {
            let curve = bin_centers
                .iter()
                .map(|&x| calculate_lorentzian_value(x, res.peak, res.mu, res.sigma))
                .collect();
            fits.push(FitCurve {
                curve,
                color: Color32::from_rgb(0, 220, 220),
                label: "Lorentzian".to_string(),
            });
        }
    }

    (stats, fits)
}

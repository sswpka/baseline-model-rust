#[derive(Debug, Clone, Default)]
pub struct BaselineData {
    pub sampling_packet_no: i32,
    pub sampling_no: i32,
    pub l1: [f32; 16],
    pub l2: [f32; 16],
    pub l6: [f32; 16],
    pub l7: [f32; 16],
    pub l1_voltage: [f32; 16],
    pub l2_voltage: [f32; 16],
    pub l6_voltage: [f32; 16],
    pub l7_voltage: [f32; 16],
}

#[derive(Debug, Clone)]
pub struct AnalysisAxisConfig {
    pub x_min: f64,
    pub x_max: f64,
    pub bin_count: i32,
    /// 0=ADC, 1=Voltage, 2=Energy
    pub axis_index: i32,
    pub slope: f64,
    pub offset: f64,
    pub voltage_max: f64,
    pub adc_resolution: f64,
    pub bar_width_multiplier: f64,
}

impl Default for AnalysisAxisConfig {
    fn default() -> Self {
        Self {
            x_min: 0.0,
            x_max: 0.0,
            bin_count: 4096,
            axis_index: 0,
            slope: 0.000427,
            offset: 0.0,
            voltage_max: 5000.0,
            adc_resolution: 16383.0,
            bar_width_multiplier: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FittingAlgorithm {
    #[default]
    LevenbergMarquardt = 0,
    NelderMead = 1,
    GradientDescentLegacy = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HemgMode {
    Gaussian,
    Lorentzian,
    LeftTailed,
    RightTailed,
    #[default]
    DoubleSided,
}

#[derive(Debug, Clone)]
pub struct HemgFitConfig {
    // Fitting Settings
    pub max_iterations: i32,
    /// Initial Damping
    pub lambda: f64,
    pub mode: HemgMode,

    // Data/Hardware Settings
    pub adc_resolution: i32,
    /// mV
    pub voltage_range: f64,
    pub histogram_bin_count: usize,

    // ROI Settings
    pub roi_sigma_multiplier: f64,
    pub default_roi_window: i32,

    // Optimization Settings
    pub initial_eta: f64,
    pub jacobian_epsilon: f64,
    pub convergence_tolerance: f64,
    pub min_parameter_value: f64,
    pub max_total_eta: f64,
}

impl Default for HemgFitConfig {
    fn default() -> Self {
        Self {
            max_iterations: 30,
            lambda: 0.01,
            mode: HemgMode::DoubleSided,
            adc_resolution: 16384,
            voltage_range: 5000.0,
            histogram_bin_count: 16384,
            roi_sigma_multiplier: 8.0,
            default_roi_window: 100,
            initial_eta: 0.05,
            jacobian_epsilon: 1e-5,
            convergence_tolerance: 1e-4,
            min_parameter_value: 1e-3,
            max_total_eta: 0.99,
        }
    }
}

/// Mirrors C# `FittingResult`: `True` when fit succeeded and result is usable;
/// `false` for `empty()` or a failed fit.
#[derive(Debug, Clone, Default)]
pub struct FittingResult {
    pub is_valid: bool,
    pub fit_curve: Vec<f64>,
    pub mu: f64,
    pub sigma: f64,
    pub peak: f64,
    pub rms: f64,

    // HEMG-specific parameters
    pub a: f64,
    pub tau_l1: f64,
    pub tau_l2: f64,
    pub tau_r1: f64,
    pub eta_l1: f64,
    pub eta_l2: f64,
    pub eta_r1: f64,

    // Statistics
    pub fwhm: f64,
    pub resolution: f64,
    pub r_squared: f64,
}

impl FittingResult {
    pub fn simple(fit_curve: Vec<f64>, mu: f64, sigma: f64, peak: f64, rms: f64) -> Self {
        Self {
            is_valid: true,
            fit_curve,
            mu,
            sigma,
            peak,
            rms,
            ..Default::default()
        }
    }

    pub fn hemg(
        fit_curve: Vec<f64>,
        a: f64,
        mu: f64,
        sigma: f64,
        tau_l1: f64,
        tau_r1: f64,
        eta_l1: f64,
        eta_r1: f64,
    ) -> Self {
        Self {
            is_valid: true,
            fit_curve,
            a,
            mu,
            sigma,
            tau_l1,
            tau_r1,
            eta_l1,
            eta_r1,
            peak: a,
            rms: 0.0,
            ..Default::default()
        }
    }

    pub fn empty(length: usize) -> Self {
        Self {
            is_valid: false,
            fit_curve: vec![0.0; length],
            ..Default::default()
        }
    }
}

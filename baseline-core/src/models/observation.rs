use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DetectorLayer {
    L1,
    L2,
    L6,
    L7,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BgoLayer {
    L3,
    L4,
    L5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FitMethod {
    Gaussian,
    Lorentzian,
}

#[derive(Debug, Clone, Default)]
pub struct BgoData {
    pub high_gain: Vec<f64>,
    pub low_gain: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct LayerData {
    pub pulse_height_x: Vec<f64>,
    pub pulse_height_y: Vec<f64>,
    /// Strip data: key is strip index (0-8)
    pub strip_x: HashMap<i32, Vec<i32>>,
    pub strip_y: HashMap<i32, Vec<i32>>,
}

impl Default for LayerData {
    fn default() -> Self {
        let mut strip_x = HashMap::new();
        let mut strip_y = HashMap::new();
        for i in 0..=8 {
            strip_x.insert(i, Vec::new());
            strip_y.insert(i, Vec::new());
        }
        Self {
            pulse_height_x: Vec::new(),
            pulse_height_y: Vec::new(),
            strip_x,
            strip_y,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ObservationProcessReport {
    pub current_step: i32,
    pub total_steps: i32,
    pub message: String,
    pub current_time: Option<chrono::NaiveDateTime>,
    pub is_complete: bool,
    pub last_hex_data: Option<Vec<String>>,
}

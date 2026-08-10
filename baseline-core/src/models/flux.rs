/// Holds processed flux observation data for a single packet.
#[derive(Debug, Clone)]
pub struct FluxDataResult {
    /// Time offset in seconds from the timecode field.
    pub time_seconds: f64,
    /// Particle counting for 7 layers (L1-L7).
    pub particle_counting: [f64; 7],
    /// Layer index for each of the 1000 particle slots.
    pub particle_layer: Vec<f64>,
    /// Offset time detected for each of the 1000 particle slots.
    pub particle_offset_time: Vec<f64>,
}

impl Default for FluxDataResult {
    fn default() -> Self {
        Self {
            time_seconds: 0.0,
            particle_counting: [0.0; 7],
            particle_layer: vec![0.0; 1000],
            particle_offset_time: vec![0.0; 1000],
        }
    }
}

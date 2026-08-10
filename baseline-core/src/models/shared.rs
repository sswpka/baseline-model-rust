pub struct AppConstants;

impl AppConstants {
    pub const SOURCE_FOLDER_NAME: &'static str = "Source";
    pub const DEFAULT_OUTPUT_FILE_NAME: &'static str = "Output";

    pub const HEADER_START: &'static str = "E225";
    /// Total hex bytes per line (Header+Data+Checksum)
    pub const PACKET_LENGTH: usize = 256;
    /// Total hex characters
    pub const PACKET_HEX_LENGTH: usize = Self::PACKET_LENGTH * 2;
    /// E225 segment size: 2064 bytes = 4128 hex chars (used in Flux/Calibration raw processing).
    pub const SEGMENT_HEX_LENGTH: usize = 4128;
    /// bytes to skip before particle data
    pub const HEADER_OFFSET: usize = 16;
    /// number of particles per data packet
    pub const PARTICLES_PER_LINE: usize = 5;

    /// length of one particle's data in bytes
    pub const PARTICLE_DATA_LENGTH: usize = 34;
    /// Number of channels per detector layer
    pub const CHANNELS_PER_LAYER: usize = 16;

    pub const CHART_X_MIN: i32 = 0;
    pub const CHART_X_MAX_DSSD: i32 = 16384;
    pub const CHART_X_MAX_BGO: i32 = 4095;
    pub const DATE_TIME_FORMAT: &'static str = "%Y-%m-%d %H:%M:%S";
}

#[derive(Debug, Clone, Default)]
pub struct HeaderValidationResult {
    pub is_valid: bool,
    pub error_message: Option<String>,
    pub error_line: i32,
    pub error_content: Option<String>,
    /// Captures the valid header for parsing.
    pub first_header_content: Option<String>,
    pub filtered_file_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PlotUpdateEvent {
    pub data: Vec<super::baseline::BaselineData>,
}

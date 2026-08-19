pub mod data_processor;
pub mod processing;

pub use data_processor::{
    parse_flux_line, FluxLineResult, FluxTailField, FLUX_TAIL_FIELDS, PARTICLE_COUNTS_LAYERS, PARTICLE_INFO_COUNT,
};
pub use processing::{FluxAccumulator, HeaderParseResult};

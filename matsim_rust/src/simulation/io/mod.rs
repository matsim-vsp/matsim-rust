use crate::simulation::config::CompressionType;
use std::path::{Path, PathBuf};

pub(crate) mod batch;
pub mod proto;
pub mod xml;

/// The files that the controller writes to the output directory, and with plans and experienced plans also to the
/// iteration directories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFile {
    Config,
    ScoringParameters,
    Network,
    TransitSchedule,
    TransitVehicles,
    Plans,
    ExperiencedPlans,
    Ids,
}

impl OutputFile {
    /// Returns the file name. Files with a fixed format, the config and the scoring parameters as YAML and the IDs as
    /// protobuf, ignore `compression`.
    pub fn file_name(self, compression: CompressionType) -> String {
        match self {
            Self::Config => "output_config.yml".to_string(),
            Self::ScoringParameters => "output_scoring_parameters.yml".to_string(),
            Self::Ids => "output_ids.binpb".to_string(),
            Self::Network => compression.with_extension("output_network"),
            Self::TransitSchedule => compression.with_extension("output_transitSchedule"),
            Self::TransitVehicles => compression.with_extension("output_transitVehicles"),
            Self::Plans => compression.with_extension("output_plans"),
            Self::ExperiencedPlans => compression.with_extension("output_experienced_plans"),
        }
    }
}

/// Returns the file name of the events that partition `rank` writes.
pub fn events_file_name(rank: u32, compression: CompressionType) -> String {
    format!("events.{rank}.{}", compression.extension())
}

pub fn resolve_path(config_path: &Option<PathBuf>, file_path: &PathBuf) -> PathBuf {
    // This is a bit hacky, but tests rely on that. Paul, jul'25
    if file_path.is_absolute() || file_path.starts_with("./") {
        return file_path.clone();
    }

    if let Some(path) = config_path.as_ref().and_then(|c| c.parent()) {
        path.join(file_path)
    } else {
        file_path.clone()
    }
}

pub fn is_url(path: impl AsRef<Path>) -> bool {
    path.as_ref().starts_with("http://") || path.as_ref().starts_with("https://")
}

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
    Facilities,
    Vehicles,
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
        self.output_file_name(compression, None)
    }

    /// Returns the main output file name with an optional run-ID prefix.
    pub fn output_file_name(self, compression: CompressionType, run_id: Option<&str>) -> String {
        let prefix = run_id.map_or_else(String::new, |id| format!("{id}."));
        format!(
            "{prefix}output_{}.{}",
            self.stem(),
            self.extension(compression)
        )
    }

    /// Returns the iteration file name, without the main output's `output_` prefix.
    pub fn iteration_file_name(
        self,
        compression: CompressionType,
        run_id: Option<&str>,
        iteration: u32,
    ) -> String {
        let prefix = run_id.map_or_else(String::new, |id| format!("{id}."));
        format!(
            "{prefix}{iteration}.{}.{}",
            self.stem(),
            self.extension(compression)
        )
    }

    fn stem(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::ScoringParameters => "scoring_parameters",
            Self::Network => "network",
            Self::Facilities => "facilities",
            Self::Vehicles => "vehicles",
            Self::TransitSchedule => "transitSchedule",
            Self::TransitVehicles => "transitVehicles",
            Self::Plans => "plans",
            Self::ExperiencedPlans => "experienced_plans",
            Self::Ids => "ids",
        }
    }

    fn extension(self, compression: CompressionType) -> &'static str {
        match self {
            Self::Config | Self::ScoringParameters => "yml",
            Self::Ids => "binpb",
            _ => compression.extension(),
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

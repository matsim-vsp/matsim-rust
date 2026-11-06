//! Converts the scoring parameters of a MATSim config, typically an `output_config.xml` with all
//! parameters that a run used, into the YAML file that `scoring.scoring_parameters_path` refers to.

use std::path::PathBuf;

use clap::Parser;
use tracing::info;

use matsim_rust::simulation::io::xml::config::{IOConfig, scoring_parameter_sets};
use matsim_rust::simulation::logging::init_std_out_logging_thread_local;
use matsim_rust::simulation::scoring::write_scoring_parameter_sets;

fn main() {
    let _g = init_std_out_logging_thread_local();
    let args = InputArgs::parse();
    info!("Starting scoring parameter conversion with args: {args:?}");

    let config = IOConfig::from_file(&args.config);
    let sets = scoring_parameter_sets(&config).unwrap_or_else(|e| {
        panic!(
            "Cannot convert the scoring parameters of {:?}: {e}",
            args.config
        )
    });
    write_scoring_parameter_sets(&sets, &args.output);

    for set in &sets {
        info!(
            "Wrote the scoring parameters for {} with {} activity and {} mode parameters.",
            set.describe_subpopulation(),
            set.activity_params.len(),
            set.mode_params.len()
        );
    }
    info!(
        "Wrote {} scoring parameter sets to {:?}.",
        sets.len(),
        args.output
    );
}

#[derive(Parser, Debug)]
struct InputArgs {
    /// A MATSim config, may be compressed with gzip or zstd.
    #[arg(short, long)]
    pub config: PathBuf,
    /// The YAML file to write.
    #[arg(short, long)]
    pub output: PathBuf,
}

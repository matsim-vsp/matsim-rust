//! Regenerate the final-iteration analysis report of a completed run.
//!
//! The command reuses the final iteration, ID mapping and metadata recorded by the run, so it
//! never reruns QSim. Only the analysis outputs are rewritten; event files, plans, the output
//! network and the ID store are read but left untouched.

use clap::Parser;
use rust_qsim::simulation::analysis;
use rust_qsim::simulation::logging::init_std_out_logging_thread_local;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing::{error, info};

fn main() -> ExitCode {
    let _guard = init_std_out_logging_thread_local();
    let args = AnalyzeArgs::parse();
    match analysis::reanalyze_completed_run(&args.run_dir, args.interval_seconds) {
        Ok(report) => {
            info!("Analysis report: {}", report.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            error!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct AnalyzeArgs {
    /// Output directory of a completed run that recorded an analysis report.
    #[arg(long, short)]
    run_dir: PathBuf,
    /// Width of the exported link-volume intervals in seconds. Defaults to the width the recorded
    /// report used, so analysis settings can change without rerunning QSim.
    #[arg(long, short)]
    interval_seconds: Option<u32>,
}

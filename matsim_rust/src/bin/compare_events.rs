use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use matsim_rust::simulation::events::utils::{compare_event_files, compare_event_folder};
use matsim_rust::simulation::logging::init_std_out_logging_thread_local;
use tracing::{error, info};

/// Compares two event sources and exits with 0 if they are equal and 1 if not. Both are either files (XML, compressed
/// XML or protobuf) or folders with partitioned `events.<rank>.<format>` files, as the simulation writes them. Events
/// of the same time step may be in a different order. Meant for integration tests that run the simulation as a
/// process, e.g. against an external router.
#[derive(Parser, Debug)]
#[command(about)]
struct InputArgs {
    /// Expected events: a file or a folder.
    expected: PathBuf,
    /// Actual events: a file or a folder, the same kind as `expected`.
    actual: PathBuf,
}

fn main() -> ExitCode {
    let _g = init_std_out_logging_thread_local();
    let args = InputArgs::parse();
    info!("Comparing events with args: {args:?}");

    for path in [&args.expected, &args.actual] {
        if !path.exists() {
            error!("Events do not exist: {}", path.display());
            return ExitCode::FAILURE;
        }
    }

    let result = match (args.expected.is_dir(), args.actual.is_dir()) {
        (true, true) => compare_event_folder(&args.expected, &args.actual),
        (false, false) => compare_event_files(&args.expected, &args.actual),
        _ => {
            error!(
                "Expected and actual events must both be files or both be folders: {} and {}",
                args.expected.display(),
                args.actual.display()
            );
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(()) => {
            info!("Events are equal.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!("Events differ: {e}");
            ExitCode::FAILURE
        }
    }
}

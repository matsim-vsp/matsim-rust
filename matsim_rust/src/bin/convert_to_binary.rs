use clap::Parser;
use tracing::info;

use matsim_rust::utilities::convert_to_binary::InputArgs;

fn main() {
    let _guard = matsim_rust::simulation::logging::init_std_out_logging_thread_local();
    let args = InputArgs::parse();

    matsim_rust::utilities::convert_to_binary::run(&args, |_, _, _, _, _| {});

    info!("Finished conversion. Exiting.")
}

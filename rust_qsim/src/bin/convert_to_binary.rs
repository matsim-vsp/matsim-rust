use clap::Parser;
use tracing::info;

use rust_qsim::utilities::convert_to_binary::InputArgs;

fn main() {
    rust_qsim::simulation::logging::init_std_out_logging_thread_local();
    let args = InputArgs::parse();

    rust_qsim::utilities::convert_to_binary::run(&args, |_, _, _, _, _| {});

    info!("Finished conversion. Exiting.")
}

use clap::Parser;
use rust_qsim::simulation::config::{CommandLineArgs, Config};
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::logging::init_std_out_logging_thread_local;
use rust_qsim::simulation::scenario::Scenario;
use std::sync::Arc;
use tracing::info;

fn main() {
    let _guard = init_std_out_logging_thread_local();

    let args = CommandLineArgs::parse();
    info!("Started with args: {:?}", args);

    // Load and adapt config
    let config = Arc::new(Config::from_args(args));

    // Load and adapt mod
    let scenario = Scenario::load(config);

    // Create and run simulation
    match ControllerBuilder::default_with_scenario(scenario).build() {
        Ok(controller) => controller.run(),
        Err(e) => panic!("Failed to build controller: {}", e),
    }
}

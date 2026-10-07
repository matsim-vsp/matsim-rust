use clap::Parser;
use matsim_rust::simulation::config::{CommandLineArgs, Config};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::logging::init_std_out_logging_thread_local;
use matsim_rust::simulation::scenario::Scenario;
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
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();

    controller.run()
}

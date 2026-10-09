use clap::Parser;
use matsim_rust::simulation::config::{CommandLineArgs, Config};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::logging::init_std_out_logging_thread_local;
use matsim_rust::simulation::scenario::Scenario;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

#[derive(Parser, Debug)]
struct LocalQSimArgs {
    #[command(flatten)]
    config: CommandLineArgs,
    /// Keep the Rust route service alive after QSim and publish its address here.
    #[arg(long)]
    routing_service_ready_file: Option<PathBuf>,
}

#[hotpath::main]
fn main() {
    let _guard = init_std_out_logging_thread_local();

    let args = LocalQSimArgs::parse();
    info!("Started with args: {:?}", args);

    // Load and adapt config
    let config = Arc::new(Config::from_args(args.config));

    // Load and adapt mod
    let scenario = Scenario::load(config);

    // Create and run simulation
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();

    let (router, population) = controller.run();
    if let Some(ready_file) = args.routing_service_ready_file {
        info!("QSim finished; starting SILO route service");
        matsim_rust::external_services::silo_routing::serve(
            router,
            population,
            "127.0.0.1:0",
            &ready_file,
        )
        .unwrap_or_else(|error| panic!("{error}"));
    }
}

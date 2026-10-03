use clap::Parser;
use rust_qsim::external_services::routing::RoutingServiceAdapterFactory;
use rust_qsim::external_services::{AdapterHandleBuilder, AsyncExecutor, ExternalServiceType};
use rust_qsim::simulation::config::Config;
use rust_qsim::simulation::controller::ExternalServices;
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::logging::init_std_out_logging_thread_local;
use rust_qsim::simulation::population::agent_source::PreplanningHorizonAgentSource;
use rust_qsim::simulation::scenario::Scenario;
use std::sync::{Arc, Barrier};
use tracing::info;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about, long_about = None)]
struct RoutingCommandLineArgs {
    #[clap(long, short, num_args = 1.., value_delimiter = ' ')]
    router_ip: Vec<String>,
    #[clap(flatten)]
    delegate: rust_qsim::simulation::config::CommandLineArgs,
}

fn main() {
    let _guard = init_std_out_logging_thread_local();
    let args = RoutingCommandLineArgs::parse();

    info!("Starting with args: {:?}", args);

    let config = Arc::new(Config::from_args(args.delegate));

    // Creating the routing adapter is only one task, so we add 1 and not the number of worker threads!
    let total_thread_count = config.partitioning().num_parts + 1;
    let barrier = Arc::new(Barrier::new(total_thread_count as usize));

    // Configuring the routing adapter. We need
    // - the IP address of the router service
    // - the configuration of the simulation
    // - the shutdown handles of the executor (= receiver of shutdown signals from the controller)
    // The AsyncExecutor will spawn a thread for the routing service adapter and an async runtime.
    let executor = AsyncExecutor::from_config(&config, barrier.clone());
    let factory = RoutingServiceAdapterFactory::new(
        args.router_ip,
        config.clone(),
        executor.shutdown_handles(),
    );

    // Spawning the routing service adapter in a separate thread. The adapter will be run in its own tokio runtime.
    // This function returns
    // - the join handle of the adapter thread
    // - a channel for sending requests to the adapter
    // - a channel for sending shutdown signal for the adapter
    let (router_handle, send, send_sd) = executor.spawn_thread("router", factory);

    // Creating the adapter handle. This is necessary for regulated shutdown of the adapter thread. Otherwise, the adapter might be stuck in a loop.
    let handle = AdapterHandleBuilder::default()
        .shutdown_sender(send_sd)
        .handle(router_handle)
        .build();

    let adapters = match handle {
        Ok(handle) => vec![handle],
        Err(e) => panic!("Failed to build adapter handle: {}", e),
    };

    // The request sender is passed to the controller.
    let mut services = ExternalServices::default();
    services.insert(ExternalServiceType::Routing("pt".into()), send.into());

    // Load mod
    let scenario = Scenario::load(config);

    // Create controller
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .external_services(services)
        .agent_source(PreplanningHorizonAgentSource)
        .global_barrier(barrier)
        .adapter_handles(adapters)
        .build();

    // Run controller
    match controller {
        Ok(controller) => controller.run(),
        Err(e) => panic!("Failed to build controller: {}", e),
    }
}

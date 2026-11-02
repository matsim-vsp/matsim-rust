use macros::deterministic_id_test;
use matsim_rust::external_services::routing::RoutingServiceAdapterFactory;
use matsim_rust::external_services::{AdapterHandleBuilder, AsyncExecutor, ExternalServiceType};
use matsim_rust::simulation::config::{CommandLineArgs, Config, PartitionMethod, TransitDriver};
use matsim_rust::simulation::controller::ExternalServices;
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::events::utils::compare_event_folder;
use matsim_rust::simulation::population::agent_source::PreplanningHorizonAgentSource;
use matsim_rust::simulation::scenario::Scenario;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

#[deterministic_id_test(matsim_rust)]
fn pt_tutorial_matches_expected_events() {
    let config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/pt_tutorial/pt_tutorial_config.yml",
    ));
    let output_dir = config.output().output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
    compare_event_folder(
        "./tests/resources/pt_tutorial/expected_events",
        output_dir.join("events"),
    )
    .unwrap();
}

/// Runs `pt_tutorial` with schedule-based transit and compares the events with the expected ones.
/// These match SBB's deterministic transit engine in MATSim, apart from its door times (boarding and
/// alighting one second after the arrival, the departure three seconds later) and the tick per leg.
/// With two partitions, stop 3 is on its own partition, so that the vehicles change partitions. METIS
/// would put the whole transit network of the tutorial on one partition. Both kinds of drivers give
/// the same events.
#[deterministic_id_test(matsim_rust)]
fn pt_tutorial_schedule_based_matches_expected_events() {
    for (parts, driver) in [
        (1, TransitDriver::Scheduled),
        (2, TransitDriver::Scheduled),
        (1, TransitDriver::Planned),
    ] {
        let mut config = Config::from_args(CommandLineArgs::new_with_path(
            "./tests/resources/pt_tutorial/pt_tutorial_schedule_based_config.yml",
        ));
        config.transit_mut().driver = driver;
        if parts == 2 {
            config.network_mut().path = Some(PathBuf::from(
                "./tests/resources/pt_tutorial/multimodalnetwork_stop_3_apart.xml",
            ));
            config.partitioning_mut().num_parts = 2;
            config.partitioning_mut().method = PartitionMethod::None;
        }
        let output_dir = PathBuf::from(format!(
            "./test_output/simulation/pt_tutorial_schedule_based_{parts}_{driver:?}"
        ));
        config.output_mut().output_dir = output_dir.clone();

        let scenario = Scenario::load(config);
        ControllerBuilder::default_with_scenario(scenario)
            .build()
            .unwrap()
            .run();
        compare_event_folder(
            "./tests/resources/pt_tutorial/expected_events_schedule_based",
            output_dir.join("events"),
        )
        .unwrap();
    }
}

#[deterministic_id_test(matsim_rust)]
#[ignore]
fn pt_adaptive_with_access_egress() {
    test_pt_adaptive(PathBuf::from(
        "./assets/pt_tutorial/plans_1-access_egress.xml",
    ))
}

#[deterministic_id_test(matsim_rust)]
#[ignore]
fn pt_adaptive_with_dummy() {
    test_pt_adaptive(PathBuf::from("./assets/pt_tutorial/plans_1-dummy.xml"))
}

// to be tested with running routing service;
// --config /Users/paulh/git/matsim-rust/matsim_rust/assets/pt_tutorial/config.xml --output output/v6.4/test-router
fn test_pt_adaptive(pop_path: PathBuf) {
    let mut config_args = CommandLineArgs::new_with_path(
        "./tests/resources/pt_tutorial/pt_tutorial_config_adaptive.yml",
    );

    config_args
        .overrides
        .push((String::from("routing.mode"), String::from("ad-hoc")));

    let mut c = Config::from_args(config_args);
    c.population_mut().path = Some(pop_path);

    let config = Arc::new(c);

    let total_thread_count = config.partitioning().num_parts + 1;
    let global_barrier = Arc::new(Barrier::new(total_thread_count as usize));

    let executor = AsyncExecutor::from_config(&config, global_barrier.clone());

    let routing_factory = RoutingServiceAdapterFactory::new(
        vec!["http://localhost:50051"],
        config.clone(),
        executor.shutdown_handles(),
    );

    let (handle, send, shutdown) = executor.spawn_thread("routing_adapter", routing_factory);

    let mut services = ExternalServices::default();
    services.insert(ExternalServiceType::Routing("pt".into()), send.into());

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .external_services(services)
        .global_barrier(global_barrier)
        .agent_source(PreplanningHorizonAgentSource)
        .adapter_handles(vec![
            AdapterHandleBuilder::default()
                .shutdown_sender(shutdown)
                .handle(handle)
                .build()
                .unwrap(),
        ])
        .build()
        .unwrap();
    controller.run();
}

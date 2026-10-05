use macros::deterministic_id_test;
use rust_qsim::external_services::routing::RoutingServiceAdapterFactory;
use rust_qsim::external_services::{AdapterHandleBuilder, AsyncExecutor, ExternalServiceType};
use rust_qsim::simulation::config::{CommandLineArgs, Config};
use rust_qsim::simulation::controller::ExternalServices;
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::events::utils::compare_event_folder;
use rust_qsim::simulation::population::agent_source::PreplanningHorizonAgentSource;
use rust_qsim::simulation::scenario::Scenario;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

#[deterministic_id_test(rust_qsim)]
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

#[deterministic_id_test(rust_qsim)]
fn pt_tutorial_transit_analysis_reports_teleported_service() {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/pt_tutorial/pt_tutorial_config.yml",
    ));
    config.output_mut().output_dir = "./test_output/simulation/pt_tutorial_analysis".into();
    config.output_mut().analysis.enabled = true;
    let output_dir = config.output().output_dir.clone();

    let scenario = Scenario::load(config);
    ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap()
        .run();

    let report = output_dir.join("analysis");
    let trips = std::fs::read_to_string(report.join("transit_trips.csv")).unwrap();
    // Person 102 waits 412 s for the 07:50 departure and rides 541 s, one second behind schedule.
    assert!(trips.contains("\"102\",\"pt\",teleported,boarded,\"Blue Line\",\"1to3\",\"1\",\"3\",27788.000000,28200.000000,28741.000000,412.000000,541.000000,28740.000000,1.000000,\"11\",\"tr_1\""), "{trips}");
    // The tutorial's vehicle file declares no transit vehicles, so no load factor exists.
    let availability = std::fs::read_to_string(report.join("transit_availability.csv")).unwrap();
    assert!(availability.contains("\"load_factor\",unavailable,"));
    assert!(
        std::fs::read_to_string(report.join("index.html"))
            .unwrap()
            .contains("<h2>Public transport</h2>")
    );
}

#[deterministic_id_test(rust_qsim)]
#[ignore]
fn pt_adaptive_with_access_egress() {
    test_pt_adaptive(PathBuf::from(
        "./assets/pt_tutorial/plans_1-access_egress.xml",
    ))
}

#[deterministic_id_test(rust_qsim)]
#[ignore]
fn pt_adaptive_with_dummy() {
    test_pt_adaptive(PathBuf::from("./assets/pt_tutorial/plans_1-dummy.xml"))
}

// to be tested with running routing service;
// --config /Users/paulh/git/parallel_qsim_rust/rust_qsim/assets/pt_tutorial/config.xml --output output/v6.4/test-router
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

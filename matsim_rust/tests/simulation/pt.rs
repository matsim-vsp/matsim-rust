use macros::deterministic_id_test;
use matsim_rust::external_services::routing::RoutingServiceAdapterFactory;
use matsim_rust::external_services::{AdapterHandleBuilder, AsyncExecutor, ExternalServiceType};
use matsim_rust::simulation::config::{CommandLineArgs, Config};
use matsim_rust::simulation::controller::ExternalServices;
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::events::EventsManager;
use matsim_rust::simulation::events::utils::{compare_event_folder, read_events};
use matsim_rust::simulation::id::Id;
use matsim_rust::simulation::population::agent_source::PreplanningHorizonAgentSource;
use matsim_rust::simulation::scenario::Scenario;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;
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

/// Runs `pt_tutorial` with schedule-based transit and returns the events folder.
fn run_schedule_based(num_parts: u32, run: &str) -> PathBuf {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/pt_tutorial/pt_tutorial_schedule_based_config.yml",
    ));
    config.partitioning_mut().num_parts = num_parts;
    // Without a main mode, all transit links would be put into one partition.
    config.qsim_mut().main_modes.push("train".to_string());
    let output_dir = PathBuf::from(format!(
        "./test_output/simulation/pt_tutorial_schedule_based_{num_parts}_{run}"
    ));
    config.output_mut().output_dir = output_dir.clone();

    let scenario = Scenario::load(config);
    if num_parts == 2 {
        // The vehicles cross partitions between the first and the last stop.
        let partition = |link: &str| scenario.network.get_link(&Id::create(link)).partition;
        assert_ne!(partition("11"), partition("33"));
    }
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();
    output_dir.join("events")
}

#[deterministic_id_test(matsim_rust)]
fn pt_tutorial_schedule_based_is_reproducible_and_independent_of_partitioning() {
    let single = run_schedule_based(1, "a");
    let single_again = run_schedule_based(1, "b");
    let two_parts = run_schedule_based(2, "a");

    let types = Rc::new(RefCell::new(BTreeSet::new()));
    let collected = types.clone();
    let mut events = EventsManager::new();
    events.on_any(move |event| {
        collected.borrow_mut().insert(event.type_());
    });
    read_events(&mut events, single.join("events.0.binpb")).unwrap();
    for expected in [
        "TransitDriverStarts",
        "waitingForPt",
        "VehicleArrivesAtFacility",
        "VehicleDepartsAtFacility",
        "PersonEntersVehicle",
    ] {
        assert!(
            types.borrow().contains(expected),
            "Missing {expected} events"
        );
    }
    assert!(!types.borrow().contains("travelled with pt"));
    assert!(!types.borrow().contains("stuckAndAbort"));

    compare_event_folder(&single, &single_again).unwrap();
    compare_event_folder(&single, &two_parts).unwrap();
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

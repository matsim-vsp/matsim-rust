#[path = "../common.rs"]
mod common;

use macros::deterministic_id_test;
use matsim_rust::external_services::routing::RoutingServiceAdapterFactory;
use matsim_rust::external_services::{AdapterHandleBuilder, AsyncExecutor, ExternalServiceType};
use matsim_rust::simulation::config::{CommandLineArgs, Config};
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

/// A passenger left waiting at the end of the day never boarded, so the simulation has to write
/// the stuck event MATSim writes for one. Ending the run before the first vehicle departs is the
/// cheapest way to make that happen.
#[deterministic_id_test(matsim_rust)]
fn passengers_still_waiting_when_the_run_ends_are_stuck() {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/pt_simulated/one_part.yml",
    ));
    // The tutorial's first passengers reach a stop around 07:46 and board around 07:50; end
    // the day in between so someone is left waiting.
    config.qsim_mut().end_time = 28_000;
    config.output_mut().output_dir = "./test_output/simulation/pt_simulated_stranded".into();
    let output_dir = config.output().output_dir.clone();

    ControllerBuilder::default_with_scenario(Scenario::load(config))
        .build()
        .unwrap()
        .run();

    let bytes: Vec<u8> = std::fs::read(output_dir.join("events/events.0.binpb")).unwrap();
    let has = |needle: &[u8]| bytes.windows(needle.len()).any(|window| window == needle);
    assert!(
        has(b"waitingForPt"),
        "no passenger ever waited for a vehicle"
    );
    assert!(
        has(b"stuckAndAbort"),
        "the waiting passengers were not stuck"
    );
}

/// The tutorial's own vehicles file declares no vehicles, so it cannot run the transit engine.
/// This uses the same network, plans and schedule with a vehicles file that declares the two
/// transit vehicles the schedule drives, and checks that partitioning does not change the result.
#[deterministic_id_test(matsim_rust)]
fn simulated_transit_vehicles_reach_the_same_state_in_one_and_two_partitions() {
    let run = |config_path: &str| {
        let config = Config::from_args(CommandLineArgs::new_with_path(config_path));
        let output_dir = config.output().output_dir.clone();
        ControllerBuilder::default_with_scenario(Scenario::load(config))
            .build()
            .unwrap()
            .run();
        output_dir
    };
    let one_part = run("./tests/resources/pt_simulated/one_part.yml");
    let two_parts = run("./tests/resources/pt_simulated/two_parts.yml");

    compare_event_folder(one_part.join("events"), two_parts.join("events")).unwrap();

    // The comparison above would also pass if transit had been teleported, so check that the
    // vehicles really ran. Event type names are stored as plain strings in the protobuf files.
    for (dir, expected_files) in [(&one_part, 1), (&two_parts, 2)] {
        let files: Vec<_> = std::fs::read_dir(dir.join("events"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "binpb"))
            .collect();
        assert_eq!(files.len(), expected_files, "{dir:?}");
        let bytes: Vec<u8> = files
            .iter()
            .flat_map(|path| std::fs::read(path).unwrap())
            .collect();
        for event_type in [
            b"TransitDriverStarts".as_slice(),
            b"VehicleArrivesAtFacility",
            b"VehicleDepartsAtFacility",
            b"waitingForPt",
        ] {
            assert!(
                bytes
                    .windows(event_type.len())
                    .any(|window| window == event_type),
                "{dir:?} recorded no {event_type:?}: transit was not simulated"
            );
        }
    }
}

#[deterministic_id_test(matsim_rust)]
fn timetable_transit_results_match_with_cross_partition_route() {
    let run = |num_parts, output_dir: &str| {
        let mut config = Config::from_args(CommandLineArgs::new_with_path(
            "./tests/resources/pt_simulated/timetable_mixed.yml",
        ));
        config.partitioning_mut().num_parts = num_parts;
        config.output_mut().output_dir = output_dir.into();
        let output_dir = config.output().output_dir.clone();
        let mut scenario = Scenario::load(config);
        if num_parts > 1 {
            common::force_train_boundary(&mut scenario);
        }
        ControllerBuilder::default_with_scenario(scenario)
            .build()
            .unwrap()
            .run();
        output_dir
    };

    let one_part = run(1, "./test_output/simulation/pt_timetable_mixed_one_part");
    let two_parts = run(2, "./test_output/simulation/pt_timetable_mixed_two_parts");
    compare_event_folder(one_part.join("events"), two_parts.join("events")).unwrap();
}

#[deterministic_id_test(matsim_rust)]
fn pt_tutorial_transit_analysis_reports_teleported_service() {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/pt_tutorial/pt_tutorial_config.yml",
    ));
    config.output_mut().output_dir = "./test_output/simulation/pt_tutorial_analysis".into();
    config.output_mut().analysis.enabled = true;
    // The observation file lives outside the output directory, which the run recreates.
    let observed = tempfile::tempdir().unwrap();
    let observed_path = observed.path().join("observed_transit.csv");
    std::fs::write(
        &observed_path,
        "scope,line_id,stop_id,station_id,period_start_seconds,period_end_seconds,metric,unit,value,source\n\
         stop,,1,,25200,28800,boardings,persons,1,counter-1\n",
    )
    .unwrap();
    config.output_mut().analysis.transit_observed_data = Some(observed_path);
    let output_dir = config.output().output_dir.clone();

    let scenario = Scenario::load(config);
    ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap()
        .run();

    let report = output_dir.join("analysis");
    let trips = std::fs::read_to_string(report.join("transit_trips.csv")).unwrap();
    // Person 102 waits 413 s for the 07:50 departure and rides 540 s, one second behind schedule.
    assert!(trips.contains("\"102\",\"pt\",teleported,boarded,\"Blue Line\",\"1to3\",\"1\",\"3\",27787.000000,28200.000000,28740.000000,413.000000,540.000000,28740.000000,0.000000,\"11\",\"tr_1\""), "{trips}");
    // The tutorial's vehicle file declares no transit vehicles, so no load factor exists.
    let availability = std::fs::read_to_string(report.join("transit_availability.csv")).unwrap();
    assert!(availability.contains("\"load_factor\",unavailable,"));
    assert!(
        std::fs::read_to_string(report.join("index.html"))
            .unwrap()
            .contains("<h2>Public transport</h2>")
    );
    let matches = std::fs::read_to_string(report.join("transit_validation_matches.csv")).unwrap();
    assert!(matches.contains("\"stop\",\"\",\"1\",\"\",25200,\"boardings\",1.000000,1,1.000000,1.000000,0.000000,0.000000,1.000000,"), "{matches}");

    // A standalone rerun rebuilds the same transit tables from the recorded schedule, vehicle
    // capacities and observation path.
    let tables = [
        "transit_trips.csv",
        "transit_stop_hourly.csv",
        "transit_availability.csv",
        "transit_validation_matches.csv",
    ];
    let before: Vec<_> = tables
        .iter()
        .map(|table| std::fs::read(report.join(table)).unwrap())
        .collect();
    matsim_rust::simulation::analysis::reanalyze_completed_run(&output_dir, None).unwrap();
    for (table, before) in tables.iter().zip(before) {
        assert_eq!(
            before,
            std::fs::read(report.join(table)).unwrap(),
            "{table} changed on rerun"
        );
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

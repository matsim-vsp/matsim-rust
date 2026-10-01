use macros::deterministic_id_test;
use rust_qsim::simulation::config::{CommandLineArgs, Config, ModeParameter, WriteEvents};
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::events::utils::compare_event_folder;
use rust_qsim::simulation::events::{LinkEnterEvent, LinkLeaveEvent, PersonStuckEvent};
use rust_qsim::simulation::framework_events::WorkerListenerRegisterFunction;
use rust_qsim::simulation::scenario::Scenario;
use rust_qsim::simulation::time::SimTime;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[deterministic_id_test(rust_qsim)]
fn three_links_single_part_matches_expected_events() {
    let config_args =
        CommandLineArgs::new_with_path("./tests/resources/3-links/3-links-config-1.yml");
    let config = Config::from_args(config_args);
    let output_dir = config.output().output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();

    compare_event_folder(
        "./tests/resources/3-links/expected_events",
        output_dir.join("events"),
    )
    .unwrap();
}

#[deterministic_id_test(rust_qsim)]
fn three_links_two_parts_match_expected_events() {
    let config_args =
        CommandLineArgs::new_with_path("./tests/resources/3-links/3-links-config-2.yml");
    let config = Config::from_args(config_args);
    let output_dir = config.output().output_dir.clone();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .build()
        .unwrap();
    controller.run();

    compare_event_folder(
        "./tests/resources/3-links/expected_events",
        output_dir.join("events"),
    )
    .unwrap();
}

#[derive(Clone, Default)]
struct StuckEvents {
    events: Arc<Mutex<Vec<PersonStuckEvent>>>,
}

impl StuckEvents {
    fn register_fn(&self) -> Box<WorkerListenerRegisterFunction> {
        let events = self.events.clone();
        Box::new(move |event_manager, _, _| {
            event_manager.on::<PersonStuckEvent, _>(move |event| {
                events.lock().unwrap().push(event.clone());
            });
        })
    }
}

#[deterministic_id_test(rust_qsim)]
fn unfinished_activities_emit_stuck_events_in_stable_person_order() {
    let events = run_stuck_scenario(
        "./tests/resources/3-links/3-links-config-1.yml",
        Some("./assets/3-links/3-agent.xml"),
        32_399,
        32_399,
        "./test_output/simulation/stuck_activity",
    );

    assert_eq!(
        vec!["100", "200", "300"],
        events
            .iter()
            .map(|event| event.person.external())
            .collect::<Vec<_>>()
    );
    for event in events {
        assert_eq!(SimTime::from_secs(32_399), event.time);
        assert_eq!(None, event.link);
        assert_eq!(None, event.leg_mode);
        assert_eq!(None, event.reason);
    }
}

/// Force the simulation to stop having an agent not finished its leg. This should emit a stuck event for that agent.
#[deterministic_id_test(rust_qsim)]
fn network_leg_emits_same_stuck_event_with_one_and_two_partitions() {
    let single = run_stuck_scenario(
        "./tests/resources/3-links/3-links-config-1.yml",
        None,
        32_400,
        32_410,
        "./test_output/simulation/stuck_network_single",
    );
    let partitioned = run_stuck_scenario(
        "./tests/resources/3-links/3-links-config-2.yml",
        None,
        32_400,
        32_410,
        "./test_output/simulation/stuck_network_two_parts",
    );

    assert_eq!(single, partitioned);
    assert_eq!(1, single.len());
    let event = &single[0];
    assert_eq!(SimTime::from_secs(32_410), event.time);
    assert_eq!("100", event.person.external());
    assert_eq!(
        Some("link2"),
        event.link.as_ref().map(|link| link.external())
    );
    assert_eq!(
        Some("car"),
        event.leg_mode.as_ref().map(|mode| mode.external())
    );
    assert_eq!(None, event.reason);
}

fn run_stuck_scenario(
    config_path: &str,
    population_path: Option<&str>,
    start_time: u32,
    end_time: u32,
    output_dir: &str,
) -> Vec<PersonStuckEvent> {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(config_path));
    if let Some(population_path) = population_path {
        config.population_mut().path = Some(PathBuf::from(population_path));
        config.qsim_mut().main_modes.push("bike".to_string());
        config
            .scoring_mut()
            .mode_params
            .push(ModeParameter::default_for_mode("bike"));
    }
    config.qsim_mut().start_time = start_time;
    config.qsim_mut().end_time = end_time;
    config.output_mut().write_events = WriteEvents::None;
    config.output_mut().output_dir = PathBuf::from(output_dir);

    let stuck_events = StuckEvents::default();
    let additional_handlers = (0..config.partitioning().num_parts)
        .map(|rank| (rank, vec![stuck_events.register_fn()]))
        .collect::<HashMap<_, _>>();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .worker_listener_register_fn(additional_handlers)
        .build()
        .unwrap();
    controller.run();

    let events = stuck_events.events.lock().unwrap().clone();
    events
}

#[derive(Clone, Default)]
struct BoundaryEventTimes {
    enters: Arc<Mutex<Vec<SimTime>>>,
    leaves: Arc<Mutex<Vec<SimTime>>>,
}

impl BoundaryEventTimes {
    fn register_fn(&self) -> Box<WorkerListenerRegisterFunction> {
        let enters = self.enters.clone();
        let leaves = self.leaves.clone();

        Box::new(move |events, _, _, _| {
            events.on::<LinkEnterEvent, _>(move |event| {
                if event.link.external() == "link2" {
                    enters.lock().unwrap().push(event.time);
                }
            });
            events.on::<LinkLeaveEvent, _>(move |event| {
                if event.link.external() == "link2" {
                    leaves.lock().unwrap().push(event.time);
                }
            });
        })
    }
}

/// Explicitly test the following situation: A link has travel time <1s and it is a split out link. That
/// caused problems, so we set the min travel time per link to 1 tick.
/// This test runs the scenario single- and two-threaded. Both runs must enter the boundary link at the
/// same time and leave it after the same one-tick queue-travel delay.
///
/// Link 2 is the corresponding split out link.
#[deterministic_id_test(rust_qsim)]
fn short_boundary_link_has_same_leave_time_with_one_and_two_partitions() {
    let single = run_short_boundary_scenario(
        "./tests/resources/3-links/3-links-config-1.yml",
        "./test_output/simulation/short_boundary_single",
    );
    let partitioned = run_short_boundary_scenario(
        "./tests/resources/3-links/3-links-config-2.yml",
        "./test_output/simulation/short_boundary_two_parts",
    );

    let single_enters = single.enters.lock().unwrap().clone();
    let partitioned_enters = partitioned.enters.lock().unwrap().clone();
    assert_eq!(vec![SimTime::from_secs(32410)], single_enters);
    assert_eq!(single_enters, partitioned_enters);

    let single_leaves = single.leaves.lock().unwrap().clone();
    let partitioned_leaves = partitioned.leaves.lock().unwrap().clone();
    assert_eq!(vec![SimTime::from_secs(32412)], single_leaves);
    assert_eq!(single_leaves, partitioned_leaves);
}

fn run_short_boundary_scenario(config_path: &str, output_dir: &str) -> BoundaryEventTimes {
    let config_args = CommandLineArgs::new_with_path(config_path);
    let mut config = Config::from_args(config_args);
    config.network_mut().path = Some(PathBuf::from(
        "./tests/resources/3-links/3-links-short-boundary-network.xml",
    ));
    config.output_mut().output_dir = PathBuf::from(output_dir);

    let event_times = BoundaryEventTimes::default();
    let additional_handler = (0..config.partitioning().num_parts)
        .map(|rank| (rank, vec![event_times.register_fn()]))
        .collect::<HashMap<_, _>>();

    let scenario = Scenario::load(config);
    let controller = ControllerBuilder::default_with_scenario(scenario)
        .worker_listener_register_fn(additional_handler)
        .build()
        .unwrap();
    controller.run();

    event_times
}

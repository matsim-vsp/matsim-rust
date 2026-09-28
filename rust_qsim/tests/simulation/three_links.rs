use macros::deterministic_id_test;
use rust_qsim::simulation::config::{CommandLineArgs, Config};
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::events::utils::compare_event_folder;
use rust_qsim::simulation::events::{LinkEnterEvent, LinkLeaveEvent};
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

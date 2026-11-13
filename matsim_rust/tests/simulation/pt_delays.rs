//! Scenario `pt_delays` (`assets/pt_delays`, see its README): a small transit network with
//! alternatives between routes and transfers, for the development of exogenous transit delays (see
//! `docs/exogenous-pt-delays.md`). The tests here cover the base case without delays.

use macros::deterministic_id_test;
use matsim_rust::simulation::config::{CommandLineArgs, Config, PartitionMethod};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::events::utils::compare_event_folder;
use matsim_rust::simulation::id::Id;
use matsim_rust::simulation::scenario::Scenario;
use matsim_rust::simulation::scenario::network::{Network, Node};
use std::path::PathBuf;

/// Runs the base case with one partition and with two, where hub is on a partition of its own (see
/// [`put_hub_on_partition_1`]), and compares the events with the expected ones. In these, each
/// person rides the departures that its name describes, and every vehicle arrives at and departs
/// from each stop at the planned time, with delay 0. After an intended change of the result, replace the reference with the events of
/// the run with one partition, converted to XML with `merge_proto2xml_events`.
#[deterministic_id_test(matsim_rust)]
fn pt_delays_base_case() {
    for parts in [1, 2] {
        let mut config = Config::from_args(CommandLineArgs::new_with_path(
            "./assets/pt_delays/pt_delays_config.yml",
        ));
        if parts == 2 {
            config.partitioning_mut().num_parts = 2;
            config.partitioning_mut().method = PartitionMethod::None;
        }
        let output_dir = PathBuf::from(format!("./test_output/simulation/pt_delays_{parts}"));
        config.output_mut().output_dir = output_dir.clone();

        let mut scenario = Scenario::load(config);
        if parts == 2 {
            put_hub_on_partition_1(&mut scenario.network);
        }
        ControllerBuilder::default_with_scenario(scenario)
            .build()
            .unwrap()
            .run();
        compare_event_folder(
            "./assets/pt_delays/expected_events_base_case",
            output_dir.join("events"),
        )
        .unwrap();
    }
}

/// Puts the node hub and the links into it on partition 1 and leaves everything else on partition 0
/// (a link belongs to the partition of its to-node). So local changes partitions twice, feeder ends
/// on partition 1 and trunk starts there, and the transfers at hub happen on another partition than
/// the trips start.
fn put_hub_on_partition_1(network: &mut Network) {
    let hub = Id::<Node>::get_from_ext("hub");
    network.get_node_mut(&hub).partition = 1;
    for link in network.get_node(&hub).in_links.clone() {
        network.get_link_mut(&link).partition = 1;
    }
}

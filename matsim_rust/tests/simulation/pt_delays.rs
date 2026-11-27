//! Scenario `pt_delays` (`assets/pt_delays`, see its README): a small transit network with
//! alternatives between routes and transfers, for the development of exogenous transit delays (see
//! `docs/exogenous-pt-delays.md` in `parallel-qsim-berlin`). The tests cover the base case without
//! delays and the delay variants of the README, in which one departure has delays at one stop of
//! its route (its attribute `delays`, see `STOP_DELAYS_ATTRIBUTE`), which the drivers of
//! `StopDelayDriverAgentSource` carry out. The persons follow their plans: a
//! passenger boards the next vehicle of its planned line that serves its egress stop.
//!
//! Each test runs with one partition and with two, where hub is on a partition of its own (see
//! [`put_hub_on_partition_1`]), and compares the events of both with the expected ones in
//! `assets/pt_delays/expected_events_<test>`. After an intended change of the result, replace the
//! reference with the events of the run with one partition, converted to XML with
//! `merge_proto2xml_events`.

use macros::deterministic_id_test;
use matsim_rust::simulation::config::{CommandLineArgs, Config, PartitionMethod};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::events::utils::compare_event_folder;
use matsim_rust::simulation::id::Id;
use matsim_rust::simulation::pt::stop_delays::{STOP_DELAYS_ATTRIBUTE, StopDelayDriverAgentSource};
use matsim_rust::simulation::scenario::Scenario;
use matsim_rust::simulation::scenario::network::{Network, Node};
use matsim_rust::simulation::scenario::transit::TransitDeparture;
use std::path::PathBuf;

/// Delays of one departure at stops of its route, as in the attributes of the departure.
struct Delay {
    /// The external id of the departure; the ids of this scenario are unique across routes.
    departure: &'static str,
    /// The value of its attribute `delays`: a JSON array with the stops and their offsets.
    delays: &'static str,
}

/// Runs the scenario with `delays` with one partition and with two, and compares the events of
/// both with `assets/pt_delays/expected_events_<name>`.
fn run_and_compare(name: &str, delays: &[Delay]) {
    for parts in [1, 2] {
        let mut config = Config::from_args(CommandLineArgs::new_with_path(
            "./assets/pt_delays/pt_delays_config.yml",
        ));
        if parts == 2 {
            config.partitioning_mut().num_parts = 2;
            config.partitioning_mut().method = PartitionMethod::None;
        }
        let output_dir =
            PathBuf::from(format!("./test_output/simulation/pt_delays_{name}_{parts}"));
        config.output_mut().output_dir = output_dir.clone();

        let mut scenario = Scenario::load(config);
        if parts == 2 {
            put_hub_on_partition_1(&mut scenario.network);
        }
        for delay in delays {
            set_delay(&mut scenario, delay);
        }
        ControllerBuilder::default_with_scenario(scenario)
            .add_agent_source(StopDelayDriverAgentSource)
            .build()
            .unwrap()
            .run();
        compare_event_folder(
            format!("./assets/pt_delays/expected_events_{name}"),
            output_dir.join("events"),
        )
        .unwrap();
    }
}

/// Sets the attribute of `delay` on its departure.
fn set_delay(scenario: &mut Scenario, delay: &Delay) {
    let id = Id::<TransitDeparture>::get_from_ext(delay.departure);
    let schedule = &mut scenario.transit_schedule;
    let (index, _) = schedule
        .indexed_departures()
        .find(|(_, departure)| departure.id == id)
        .unwrap_or_else(|| panic!("No departure {}.", delay.departure));
    schedule
        .departure_mut(index)
        .attributes
        .insert(STOP_DELAYS_ATTRIBUTE, delay.delays);
}

/// Without delays, each person rides the departures that its name describes, and every vehicle
/// arrives at and departs from each stop at the planned time, with delay 0.
#[deterministic_id_test(matsim_rust)]
fn pt_delays_base_case() {
    run_and_compare("base_case", &[]);
}

/// `express_0805` reaches center 10 minutes late, at 08:25 (delay 600).
/// `takes_later_but_faster_express` still rides it and starts work at 08:25:02.
#[deterministic_id_test(matsim_rust)]
fn pt_delays_express_0805_late() {
    run_and_compare(
        "express_0805_late",
        &[Delay {
            departure: "express_0805",
            delays: r#"[{"stopIndex":1,"arrivalDelayOffset":600}]"#,
        }],
    );
}

/// `feeder_0800` reaches hub 3 minutes late, at 08:09, after `trunk_0808` left at 08:08.
/// `transfers_with_two_minutes_at_hub` takes the next trunk, `trunk_0815`, and reaches center at
/// 08:21 instead of 08:14.
#[deterministic_id_test(matsim_rust)]
fn pt_delays_feeder_0800_late() {
    run_and_compare(
        "feeder_0800_late",
        &[Delay {
            departure: "feeder_0800",
            delays: r#"[{"stopIndex":1,"arrivalDelayOffset":180}]"#,
        }],
    );
}

/// `trunk_0815` leaves hub 2 minutes late, at 08:17. `waits_at_hub_after_just_missing_trunk`,
/// who reaches hub at 08:16, now catches it and reaches center at 08:23 instead of 08:28.
#[deterministic_id_test(matsim_rust)]
fn pt_delays_trunk_0815_late_2min() {
    run_and_compare(
        "trunk_0815_late_2min",
        &[Delay {
            departure: "trunk_0815",
            delays: r#"[{"stopIndex":0,"departureDelayOffset":120}]"#,
        }],
    );
}

/// `trunk_0815` leaves hub 5 minutes late, at 08:20. `starts_at_hub_after_trunk_departed`, who
/// leaves home at 08:18, and `waits_at_hub_after_just_missing_trunk` both catch it and reach center
/// at 08:26 instead of 08:28.
#[deterministic_id_test(matsim_rust)]
fn pt_delays_trunk_0815_late_5min() {
    run_and_compare(
        "trunk_0815_late_5min",
        &[Delay {
            departure: "trunk_0815",
            delays: r#"[{"stopIndex":0,"departureDelayOffset":300}]"#,
        }],
    );
}

/// `local_0800` leaves suburb 5 minutes late, at 08:05. `local_0802`, scheduled two minutes later,
/// does not overtake it: it leaves suburb and reaches every stop after `local_0800`.
#[deterministic_id_test(matsim_rust)]
fn pt_delays_local_0800_late() {
    run_and_compare(
        "local_0800_late",
        &[Delay {
            departure: "local_0800",
            delays: r#"[{"stopIndex":0,"departureDelayOffset":300}]"#,
        }],
    );
}

/// `local_0800` takes 4 minutes longer from hub to center and reaches center at 08:24; `local_0802`
/// waits in front of center until then. With two partitions, the driver crosses from partition 0
/// to hub on partition 1 and back, and the delay takes effect when it leaves hub on partition 1, so
/// the events show that the delay travels with the driver.
#[deterministic_id_test(matsim_rust)]
fn pt_delays_local_0800_late_before_center() {
    run_and_compare(
        "local_0800_late_before_center",
        &[Delay {
            departure: "local_0800",
            delays: r#"[{"stopIndex":3,"arrivalDelayOffset":240}]"#,
        }],
    );
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

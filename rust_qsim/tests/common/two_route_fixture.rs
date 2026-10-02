// Shared test fixture, compiled by the integration test and the controller's private tests.
use crate::simulation::config::{
    CompressionType, Config, OverwriteFiles, PartitionMethod, StrategySetting, WriteEvents,
};
use crate::simulation::id::Id;
use crate::simulation::replanning::DefaultStrategy;
use crate::simulation::replanning::selectors::DefaultSelector;
use crate::simulation::scenario::network::{Link, Network, Node};
use crate::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalNetworkRoute, InternalPerson,
    InternalPlan, InternalRoute, Population,
};
use crate::simulation::scenario::vehicles::{Garage, InternalVehicle, InternalVehicleType};
use crate::simulation::scenario::{Coordinate, Scenario};
use crate::simulation::time::SimTime;
use nohash_hasher::IntSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

// At one departure every two seconds, 1,000 persons sustain 1,800 vehicles/h
// for about 2,000 seconds, below the combined route capacity of 2,400 vehicles/h.
pub(crate) const NUM_PERSONS: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteChoice {
    Short,
    Long,
}

pub(crate) fn two_route_scenario(
    num_parts: u32,
    output_dir: &Path,
    route_for_person: impl Fn(usize) -> RouteChoice,
) -> Scenario {
    let network = two_route_network(num_parts);
    let (population, garage) = two_route_population(route_for_person);
    Scenario {
        network,
        garage,
        population,
        transit_schedule: Default::default(),
        config: Arc::new(two_route_config(num_parts, output_dir)),
    }
}

fn two_route_config(num_parts: u32, output_dir: &Path) -> Config {
    let mut config = Config::default();
    config.partitioning_mut().num_parts = num_parts;
    config.partitioning_mut().method = PartitionMethod::None;
    config.computational_setup_mut().scoring_threads = num_parts;
    config.computational_setup_mut().replanning_threads = num_parts;

    config.controller_mut().last_iteration = 200;
    config.controller_mut().compression_type = CompressionType::Zst;
    config.output_mut().overwrite_files = OverwriteFiles::DeleteDirectoryIfExists;
    config.output_mut().output_dir = output_dir.to_path_buf();
    config.output_mut().write_events = WriteEvents::File;

    config.qsim_mut().start_time = 0;
    config.qsim_mut().end_time = 7200;
    config.qsim_mut().main_modes = vec!["car".to_string()];

    config
        .replanning_mut()
        .fraction_of_iterations_to_disable_innovation = 0.8;
    config.replanning_mut().strategy_settings = vec![
        StrategySetting {
            name: DefaultStrategy::ReRoute.as_str().to_string(),
            weight: 0.2,
            subpopulation: "person".to_string(),
        },
        StrategySetting {
            name: DefaultSelector::SelectExpBeta.as_str().to_string(),
            weight: 0.8,
            subpopulation: "person".to_string(),
        },
    ];

    // Routing still reads the configured utilities when a custom scorer is installed.
    // These coefficients make its link disutility equal travel time in seconds.
    for params in &mut config.scoring_mut().agent_params {
        params.performing = 0.0;
    }
    for params in &mut config.scoring_mut().mode_params {
        params.marginal_utility_of_traveling = -3600.0;
        params.marginal_utility_of_distance = 0.0;
        params.monetary_distance_cost_rate = 0.0;
        params.constant = 0.0;
        params.daily_money_constant = 0.0;
        params.daily_utility_constant = 0.0;
    }
    config
}

/// Seven directed links: departure -> start -> (short-1 -> short-2 | long-1 -> long-2) -> end.
/// The alternatives connect nodes a and b: short is 400 m at 600 vehicles/h,
/// long is 800 m at 1,800 vehicles/h. Shared departure/start/end links are each 100 m
/// at 36,000 vehicles/h; all links have one lane and a free speed of 10 m/s.
/// Starting on departure makes start a regular link with enter/leave events,
/// so the travel-time calculator can observe its travel and queueing time.
fn two_route_network(num_parts: u32) -> Network {
    let mut network = Network::new();
    for (name, x, y) in [
        ("origin", 0.0, 0.0),
        ("a", 100.0, 0.0),
        ("short", 300.0, 100.0),
        ("long", 300.0, -100.0),
        ("b", 500.0, 0.0),
        ("destination", 600.0, 0.0),
        ("departure-origin", -100.0, 0.0),
    ] {
        network.add_node(Node {
            id: Id::create(name),
            coord: Coordinate::new_2d(x, y),
            in_links: Vec::new(),
            out_links: Vec::new(),
            partition: if num_parts == 2 && matches!(name, "b" | "destination") {
                1
            } else {
                0
            },
            cmp_weight: 1,
        });
    }

    for (name, from, to, length, capacity) in [
        ("start", "origin", "a", 100.0, 36_000.0),
        ("short-1", "a", "short", 200.0, 600.0),
        ("short-2", "short", "b", 200.0, 600.0),
        ("long-1", "a", "long", 400.0, 1800.0),
        ("long-2", "long", "b", 400.0, 1800.0),
        ("end", "b", "destination", 100.0, 36_000.0),
        ("departure", "departure-origin", "origin", 100.0, 36_000.0),
    ] {
        let to = Id::get_from_ext(to);
        // Links belong to their destination node's partition, as in METIS partitioning.
        let partition = network.get_node(&to).partition;
        network.add_link(Link {
            id: Id::create(name),
            from: Id::get_from_ext(from),
            to,
            length,
            capacity,
            freespeed: 10.0,
            permlanes: 1.0,
            modes: IntSet::from_iter([Id::create("car")]),
            partition,
            attributes: Default::default(),
        });
    }
    network
}

/// 1,000 identical car users, each with one initial plan and departure at 100 + 2 * index seconds.
fn two_route_population(route_for_person: impl Fn(usize) -> RouteChoice) -> (Population, Garage) {
    let mut garage = Garage::new();
    let car_type = Id::create("car");
    garage.add_veh_type(InternalVehicleType {
        id: car_type.clone(),
        length: 7.5,
        width: 1.0,
        max_v: 10.0,
        pce: 1.0,
        fef: 1.0,
        net_mode: Id::create("car"),
        attributes: Default::default(),
    });

    let persons = (0..NUM_PERSONS)
        .map(|index| {
            let person_id = Id::create(&format!("person-{index:02}"));
            garage.add_veh_by_type(&person_id, &car_type);
            let vehicle_id = garage.veh_id(&person_id, &car_type);
            let plan = initial_plan(
                vehicle_id,
                SimTime::from_secs(100 + 2 * index as u64),
                route_for_person(index),
            );
            InternalPerson::new(person_id, plan)
        })
        .collect();
    (Population::from_persons(persons), garage)
}

/// One A-to-B trip: home -> walk -> car interaction -> car -> car interaction -> walk -> work.
/// The default car route follows [departure, start, short-1, short-2, end], including both endpoint links.
/// A fixed long-route choice replaces the two branch links with long-1 and long-2.
/// Walk legs have zero distance; car interactions have zero duration. There is no return trip.
fn initial_plan(
    vehicle_id: Id<InternalVehicle>,
    departure: SimTime,
    route: RouteChoice,
) -> InternalPlan {
    let branch = match route {
        RouteChoice::Short => ["short-1", "short-2"],
        RouteChoice::Long => ["long-1", "long-2"],
    };
    let departure_link = Id::get_from_ext("departure");
    let end = Id::get_from_ext("end");
    let origin_coord = Coordinate::new_2d(-50.0, 0.0);
    let destination_coord = Coordinate::new_2d(550.0, 0.0);
    let mut plan = InternalPlan::default();
    plan.add_act(InternalActivity::new(
        Some(origin_coord.clone()),
        "home",
        departure_link.clone(),
        None,
        Some(departure),
        None,
    ));
    // Zero-distance access/egress and stage activities keep this trip valid during
    // preparation, so its explicitly chosen initial network route is preserved.
    plan.add_leg(zero_distance_walk(&departure_link));
    plan.add_act(car_interaction(departure_link.clone(), origin_coord));
    plan.add_leg(InternalLeg {
        mode: Id::create("car"),
        routing_mode: Some(Id::create("car")),
        dep_time: None,
        trav_time: None,
        route: Some(InternalRoute::Network(InternalNetworkRoute::new(
            InternalGenericRoute::new(departure_link, end.clone(), None, None, Some(vehicle_id)),
            ["departure", "start", branch[0], branch[1], "end"]
                .into_iter()
                .map(Id::get_from_ext)
                .collect(),
        ))),
        attributes: Default::default(),
    });
    plan.add_act(car_interaction(end.clone(), destination_coord.clone()));
    plan.add_leg(zero_distance_walk(&end));
    plan.add_act(InternalActivity::new(
        Some(destination_coord),
        "work",
        end,
        None,
        None,
        None,
    ));
    plan
}

fn zero_distance_walk(link: &Id<Link>) -> InternalLeg {
    InternalLeg::new(
        InternalRoute::Generic(InternalGenericRoute::new(
            link.clone(),
            link.clone(),
            Some(Duration::ZERO),
            Some(0.0),
            None,
        )),
        "walk",
        "car",
        Duration::ZERO,
        None,
    )
}

fn car_interaction(link: Id<Link>, coord: Coordinate) -> InternalActivity {
    InternalActivity::new(
        Some(coord),
        "car interaction",
        link,
        None,
        None,
        Some(Duration::ZERO),
    )
}

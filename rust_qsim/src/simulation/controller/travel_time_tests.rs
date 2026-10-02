#[path = "../../../tests/common/two_route_fixture.rs"]
mod two_route_fixture;

use super::{Controller, ControllerBuilder};
use crate::simulation::events::utils::read_events;
use crate::simulation::events::{
    EventsManager, LinkEnterEvent, LinkLeaveEvent, PersonArrivalEvent, PersonDepartureEvent,
    VehicleEntersTrafficEvent,
};
use crate::simulation::id::Id;
use crate::simulation::replanning::routing::travel_time_calculator::{
    GlobalTravelTimeCalculator, TravelTimeGetter,
};
use crate::simulation::replanning::routing::{RoutingRequestBuilder, TripRouter};
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::facilities::Facility;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::{InternalLeg, InternalPerson, InternalPlanElement};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scoring::OnlyTravelTimeDependentScoring;
use crate::simulation::time::SimTime;
use macros::deterministic_id_test;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use two_route_fixture::{NUM_PERSONS, RouteChoice, two_route_scenario};

const BIN_SIZE: u64 = 900;
// Allow two simulation ticks for physical expectations, but only floating-point
// rounding when comparing the TTC's average with an independent event average.
const TRAFFIC_TOLERANCE: f64 = 2.0;
const TTC_TOLERANCE: f64 = 0.000_001;

#[derive(Debug, Clone, Copy)]
enum Case {
    WithoutCongestion,
    AllShort,
    Equilibrium,
}

impl Case {
    fn route(self, index: usize) -> RouteChoice {
        let short = match self {
            Self::WithoutCongestion => index.is_multiple_of(3),
            Self::AllShort => true,
            // Each successive initial short-route user adds 6 - 2 = 4 seconds
            // of waiting. User 10 reaches 40s; one short departure every 6s
            // thereafter maintains that queue at the short route's capacity.
            Self::Equilibrium => index < 10 || (index - 10).is_multiple_of(3),
        };
        if short {
            RouteChoice::Short
        } else {
            RouteChoice::Long
        }
    }
}

#[deterministic_id_test]
fn travel_times_after_one_iteration_without_congestion() {
    run_and_check(Case::WithoutCongestion);
}

#[deterministic_id_test]
fn travel_times_after_one_iteration_all_short() {
    run_and_check(Case::AllShort);
}

#[deterministic_id_test]
fn travel_times_after_one_iteration_at_equilibrium() {
    run_and_check(Case::Equilibrium);
}

fn one_iteration_controller(case: Case, output: &Path) -> Controller {
    let mut scenario = two_route_scenario(1, output, |index| case.route(index));
    let config = Arc::get_mut(&mut scenario.config).unwrap();
    config.controller_mut().last_iteration = 0;
    config.computational_setup_mut().random_seed = 4711;
    config.travel_time_calculator_mut().bin_size = BIN_SIZE as u32;

    ControllerBuilder::default_with_scenario(scenario)
        .scoring_function(Box::new(OnlyTravelTimeDependentScoring))
        .build()
        .unwrap()
}

#[deterministic_id_test]
fn router_after_one_iteration_without_congestion() {
    run_and_check_router(Case::WithoutCongestion);
}

#[deterministic_id_test]
fn router_after_one_iteration_all_short() {
    run_and_check_router(Case::AllShort);
}

#[deterministic_id_test]
fn router_after_one_iteration_at_equilibrium() {
    run_and_check_router(Case::Equilibrium);
}

fn run_and_check_router(case: Case) {
    let dir = tempfile::tempdir().unwrap();
    let controller = one_iteration_controller(case, &dir.path().join("output"));
    // Clone shares the actual registered modules, including their live TTC.
    // No routing module is rebuilt after the worker publishes its snapshot.
    let router = controller.trip_router.clone();
    let calculator = Arc::clone(&controller.travel_time_calculator);
    let network = Arc::clone(&controller.scenario.core.network);
    let garage = Arc::clone(&controller.scenario.core.garage);
    let person = controller.scenario.population.persons[&Id::get_from_ext("person-00")].clone();
    let vehicle_id = garage.veh_id(person.id(), &Id::get_from_ext("car"));
    let probe = RouterProbe {
        router: &router,
        calculator: &calculator,
        network: &network,
        person: &person,
        vehicle: &garage.vehicles[&vehicle_id],
    };
    let mut failures = Vec::new();
    probe.check(case, true, 1350, &mut failures);
    controller.run();
    probe.check(case, false, 1350, &mut failures);
    // In equilibrium, start takes 11s: departure at 895s means branch entry
    // at 906s. A lookup frozen at the request time would use the warm-up bin.
    probe.check(case, false, 895, &mut failures);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

struct RouterProbe<'a> {
    router: &'a TripRouter,
    calculator: &'a GlobalTravelTimeCalculator,
    network: &'a Network,
    person: &'a InternalPerson,
    vehicle: &'a InternalVehicle,
}

impl RouterProbe<'_> {
    /// Fixture utilities make disutility equal elapsed seconds. Independently
    /// evaluate the two known paths without calling A* or its cost adapter.
    fn candidate_time(&self, route: RouteChoice, departure: SimTime) -> Duration {
        let branch = match route {
            RouteChoice::Short => ["short-1", "short-2"],
            RouteChoice::Long => ["long-1", "long-2"],
        };
        let mut now = departure;
        for link in ["start", branch[0], branch[1]] {
            let travel_time = self.calculator.get_link_travel_time(
                &Id::get_from_ext("car"),
                self.network.get_link(&Id::get_from_ext(link)),
                now,
                Some(self.vehicle),
                TravelTimeGetter::Average,
            );
            now = now.saturating_add(travel_time);
        }
        // Routing starts at departure's end node and adds the arrival link's
        // geometric travel time. Neither endpoint has a complete event pair.
        let end = self.network.get_link(&Id::get_from_ext("end"));
        now.duration_since(departure)
            + Duration::from_secs_f64(end.length / end.freespeed.min(self.vehicle.max_v))
    }

    fn check(&self, case: Case, before_mobsim: bool, time: u64, failures: &mut Vec<String>) {
        let departure = SimTime::from_secs(time);
        let choices = [RouteChoice::Short, RouteChoice::Long];
        let costs = choices.map(|route| self.candidate_time(route, departure).as_secs_f64());
        let phase = if before_mobsim { "before" } else { "after" };
        let context = format!(
            "Router {case:?}, {phase} Mobsim, departure {time}s, \
             short {:.9}s, long {:.9}s",
            costs[0], costs[1]
        );
        let from = Facility::new_link_wrapper(
            Coordinate::new_2d(-50.0, 0.0),
            Id::get_from_ext("departure"),
        );
        let to =
            Facility::new_link_wrapper(Coordinate::new_2d(550.0, 0.0), Id::get_from_ext("end"));
        let mode = Id::get_from_ext("car");
        let request = RoutingRequestBuilder::default()
            .from(&from)
            .to(&to)
            .departure_time(departure)
            .person(Some(self.person))
            .vehicle(Some(self.vehicle))
            .build()
            .unwrap();
        let elements = self
            .router
            .calc_route(&mode, request.clone())
            .unwrap_or_else(|error| panic!("{context}: {error}"));
        let repeated = self
            .router
            .calc_route(&mode, request)
            .unwrap_or_else(|error| panic!("{context}: repeated request: {error}"));
        let car = only_car_leg(&elements, &context);
        let repeated_car = only_car_leg(&repeated, &context);
        let route = car
            .route
            .as_ref()
            .and_then(|route| route.as_network())
            .unwrap_or_else(|| panic!("{context}: car leg has no network route"));
        let links: Vec<_> = route.route().iter().map(|link| link.external()).collect();
        println!(
            "{context}: selected {links:?}, leg time {:?}, route time {:?}",
            car.trav_time,
            route.generic_delegate().trav_time()
        );
        // A tied optimum may be either path, but repeated identical requests
        // must return identical links, times and vehicle metadata.
        assert_eq!(
            car.route, repeated_car.route,
            "{context}: repeated route differs"
        );
        assert_eq!(
            car.trav_time, repeated_car.trav_time,
            "{context}: repeated leg time differs"
        );
        assert_eq!(
            car.routing_mode,
            Some(mode),
            "{context}: wrong routing mode"
        );
        assert_eq!(
            car.dep_time,
            Some(departure),
            "{context}: access changed car departure"
        );
        let generic = route.generic_delegate();
        assert_eq!(
            generic.start_link(),
            from.link(),
            "{context}: wrong start link"
        );
        assert_eq!(generic.end_link(), to.link(), "{context}: wrong end link");
        assert_eq!(
            generic.vehicle().as_ref(),
            Some(self.vehicle.id()),
            "{context}: wrong vehicle"
        );

        let paths: [Vec<Id<Link>>; 2] = choices.map(|choice| {
            let branch = match choice {
                RouteChoice::Short => ["short-1", "short-2"],
                RouteChoice::Long => ["long-1", "long-2"],
            };
            ["departure", "start", branch[0], branch[1], "end"]
                .into_iter()
                .map(Id::get_from_ext)
                .collect()
        });
        let selected = paths
            .iter()
            .position(|path| path == route.route())
            .unwrap_or_else(|| panic!("{context}: unexpected link sequence {links:?}"));
        for pair in route.route().windows(2) {
            assert_eq!(
                self.network.get_link(&pair[0]).to,
                self.network.get_link(&pair[1]).from,
                "{context}: disconnected links in {links:?}"
            );
        }
        for element in &elements {
            if let InternalPlanElement::Leg(leg) = element {
                if leg.mode.external() != "car" {
                    assert_eq!(
                        leg.trav_time,
                        Some(Duration::ZERO),
                        "{context}: nonzero access/egress time"
                    );
                    assert_eq!(
                        leg.route.as_ref().unwrap().as_generic().distance(),
                        Some(0.0),
                        "{context}: nonzero access/egress distance"
                    );
                }
            }
        }
        let result_context = format!("{context}, selected {links:?}");
        for (name, value) in [
            ("leg time", car.trav_time),
            ("route time", generic.trav_time()),
        ] {
            let value = value.unwrap_or_else(|| panic!("{result_context}: missing {name}"));
            check_close(
                failures,
                &format!("{result_context}: {name} vs independent path"),
                value.as_secs_f64(),
                costs[selected],
                TTC_TOLERANCE,
            );
        }
        check_close(
            failures,
            &format!("{result_context}: chosen path vs minimum"),
            costs[selected],
            costs[0].min(costs[1]),
            TTC_TOLERANCE,
        );
        if time == 1350 {
            let expected = if before_mobsim {
                [60.0, 100.0]
            } else {
                match case {
                    Case::WithoutCongestion => [63.0, 103.0],
                    Case::AllShort => [852.0, 749.0],
                    Case::Equilibrium => [103.0, 103.0],
                }
            };
            for (index, name) in ["short", "long"].into_iter().enumerate() {
                check_close(
                    failures,
                    &format!("{result_context}: {name} candidate vs fixed expectation"),
                    costs[index],
                    expected[index],
                    TTC_TOLERANCE,
                );
            }
        }
    }
}

fn only_car_leg<'a>(elements: &'a [InternalPlanElement], context: &str) -> &'a InternalLeg {
    let cars: Vec<_> = elements
        .iter()
        .filter_map(|element| {
            if let InternalPlanElement::Leg(leg) = element {
                if leg.mode.external() == "car" {
                    return Some(leg);
                }
            }
            None
        })
        .collect();
    assert_eq!(cars.len(), 1, "{context}: expected exactly one car leg");
    cars[0]
}

fn run_and_check(case: Case) {
    // PREPARE
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("output");
    let controller = one_iteration_controller(case, &output);

    let calculator = Arc::clone(&controller.travel_time_calculator);
    let network = Arc::clone(&controller.scenario.core.network);
    controller.run();

    // CHECK
    // 1) Observed travel times by TTC
    let observations = read_observations(&output.join("ITERS/it.0/events/events.0.xml.zst"), case);
    let mut failures = Vec::new();
    for ((link, start), samples) in &observations.bins {
        let expected = samples.mean();
        let actual = lookup(&calculator, &network, link, *start);
        println!(
            "{case:?}: {link} [{start}, {}): {} observations, events {expected:.6}s, \
             TTC {actual:.6}s, difference {:.9}s",
            start + BIN_SIZE,
            samples.count,
            (actual - expected).abs()
        );
        check_close(
            &mut failures,
            &format!(
                "TTC vs events: {case:?}, {link} [{start}, {})",
                start + BIN_SIZE
            ),
            actual,
            expected,
            TTC_TOLERANCE,
        );
        if matches!(case, Case::WithoutCongestion) {
            let free_time = match link.as_str() {
                "start" => 11.0,
                "short-1" | "short-2" => 21.0,
                "long-1" | "long-2" => 41.0,
                _ => unreachable!(),
            };
            check_close(
                &mut failures,
                &format!(
                    "Traffic expectation: {case:?}, {link} [{start}, {})",
                    start + BIN_SIZE
                ),
                expected,
                free_time,
                TRAFFIC_TOLERANCE,
            );
        }
    }

    // 2) Unobserved travel times by TTC fallback
    let mut unobserved_links = vec![("departure", 10.0), ("end", 10.0)];
    if matches!(case, Case::AllShort) {
        unobserved_links.extend([("long-1", 40.0), ("long-2", 40.0)]);
    }
    for (link, expected) in unobserved_links {
        assert!(
            observations
                .bins
                .keys()
                .all(|(observed, _)| observed != link)
        );
        for start in (0..=7200).step_by(BIN_SIZE as usize) {
            let actual = lookup(&calculator, &network, link, start);
            check_close(
                &mut failures,
                &format!("TTC fallback: {case:?}, {link} at {start}s"),
                actual,
                expected,
                TTC_TOLERANCE,
            );
        }
        println!("{case:?}: {link} unobserved, TTC fallback {expected:.2}s");
    }

    print_trip_means(case, &observations.trips, None);
    let departure_wait: Samples = observations
        .trips
        .iter()
        .map(|trip| trip.departure_wait)
        .collect();
    let max_wait = observations
        .trips
        .iter()
        .map(|trip| trip.departure_wait)
        .max()
        .unwrap();
    println!(
        "{case:?}: departure time outside TTC observations: mean {:.2}s, maximum {:.2}s \
         (including the departure tick)",
        departure_wait.mean(),
        max_wait.as_secs_f64()
    );

    match case {
        Case::WithoutCongestion | Case::AllShort => {
            for trip in &observations.trips {
                let expected = match case {
                    Case::AllShort => 64.0 + 4.0 * trip.index as f64,
                    _ if trip.route == RouteChoice::Short => 64.0,
                    _ => 104.0,
                };
                // Free car times include one departure tick, regular link times
                // (length/speed + one tick), and 10s on the arrival link.
                check_close(
                    &mut failures,
                    &format!("Traffic expectation: {case:?}, {}", trip.person),
                    trip.travel_time.as_secs_f64(),
                    expected,
                    TRAFFIC_TOLERANCE,
                );
            }
            if matches!(case, Case::AllShort) {
                let captures_queue = observations.bins.iter().any(|((link, start), samples)| {
                    let free_time = if link == "start" { 11.0 } else { 21.0 };
                    samples.mean() > free_time + TRAFFIC_TOLERANCE
                        && lookup(&calculator, &network, link, *start)
                            > free_time + TRAFFIC_TOLERANCE
                });
                if !captures_queue {
                    failures.push(
                        "TTC/traffic expectation: AllShort must capture queueing".to_string(),
                    );
                }
            }
        }
        Case::Equilibrium => {
            // Bin 0 contains queue formation; bin 1 lies wholly within steady inflow.
            for link in ["short-1", "short-2", "long-1", "long-2"] {
                assert!(observations.bins.contains_key(&(link.to_string(), 900)));
            }
            let short = lookup(&calculator, &network, "short-1", 900)
                + lookup(&calculator, &network, "short-2", 900);
            let long = lookup(&calculator, &network, "long-1", 900)
                + lookup(&calculator, &network, "long-2", 900);
            println!(
                "{case:?}: stable TTC branch totals: short {short:.2}s, long {long:.2}s, short waiting {:.2}s",
                short - 42.0
            );
            for (name, value) in [("short branch", short), ("long branch", long)] {
                check_close(
                    &mut failures,
                    &format!("Traffic expectation: stable TTC {name}"),
                    value,
                    82.0,
                    TRAFFIC_TOLERANCE,
                );
            }
            check_close(
                &mut failures,
                "Traffic expectation: stable TTC route difference",
                short,
                long,
                TRAFFIC_TOLERANCE,
            );
            check_close(
                &mut failures,
                "Traffic expectation: stable short-route waiting",
                short - 42.0,
                40.0,
                TRAFFIC_TOLERANCE,
            );

            for (start, end) in std::iter::once((700, 1900))
                .chain((0..4).map(|index| (700 + 300 * index, 1000 + 300 * index)))
            {
                let means = print_trip_means(case, &observations.trips, Some((start, end)));
                assert!(
                    means.iter().all(|samples| samples.count > 0),
                    "Both routes must occur in [{start}, {end})"
                );
                for (route, samples) in ["short", "long"].into_iter().zip(&means) {
                    check_close(
                        &mut failures,
                        &format!("Traffic expectation: {route} [{start}, {end})"),
                        samples.mean(),
                        104.0,
                        TRAFFIC_TOLERANCE,
                    );
                }
                check_close(
                    &mut failures,
                    &format!("Traffic expectation: route difference [{start}, {end})"),
                    means[0].mean(),
                    means[1].mean(),
                    TRAFFIC_TOLERANCE,
                );
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn lookup(
    calculator: &GlobalTravelTimeCalculator,
    network: &Network,
    link: &str,
    time: u64,
) -> f64 {
    calculator
        .get_link_travel_time(
            &Id::get_from_ext("car"),
            network.get_link(&Id::get_from_ext(link)),
            SimTime::from_secs(time),
            None,
            TravelTimeGetter::Average,
        )
        .as_secs_f64()
}

fn check_close(
    failures: &mut Vec<String>,
    context: &str,
    actual: f64,
    expected: f64,
    tolerance: f64,
) {
    if !actual.is_finite() || (actual - expected).abs() > tolerance {
        failures.push(format!(
            "{context}: {actual:.9}s, expected {expected:.9}s ± {tolerance}s"
        ));
    }
}

#[derive(Default)]
struct Samples {
    total: Duration,
    count: usize,
}

impl Samples {
    fn add(&mut self, duration: Duration) {
        self.total += duration;
        self.count += 1;
    }

    fn mean(&self) -> f64 {
        assert!(self.count > 0);
        self.total.as_secs_f64() / self.count as f64
    }
}

impl FromIterator<Duration> for Samples {
    fn from_iter<T: IntoIterator<Item = Duration>>(iter: T) -> Self {
        let mut result = Self::default();
        for duration in iter {
            result.add(duration);
        }
        result
    }
}

struct Trip {
    person: String,
    index: usize,
    route: RouteChoice,
    departure: SimTime,
    travel_time: Duration,
    departure_wait: Duration,
}

struct Observations {
    trips: Vec<Trip>,
    bins: BTreeMap<(String, u64), Samples>,
}

#[derive(Default)]
struct RawPerson {
    departure: Option<SimTime>,
    arrival: Option<SimTime>,
}

#[derive(Default)]
struct RawVehicle {
    person: String,
    enters: BTreeMap<String, SimTime>,
    leaves: BTreeMap<String, SimTime>,
}

#[derive(Default)]
struct RawObservations {
    persons: BTreeMap<String, RawPerson>,
    vehicles: BTreeMap<String, RawVehicle>,
}

fn read_observations(path: &Path, case: Case) -> Observations {
    let raw = Rc::new(RefCell::new(RawObservations::default()));
    let mut events = EventsManager::new();
    let departures = Rc::clone(&raw);
    events.on::<PersonDepartureEvent, _>(move |event| {
        if event.leg_mode.external() == "car" {
            let mut raw = departures.borrow_mut();
            let person = raw
                .persons
                .entry(event.person.external().to_string())
                .or_default();
            assert!(person.departure.replace(event.time).is_none());
        }
    });
    let arrivals = Rc::clone(&raw);
    events.on::<PersonArrivalEvent, _>(move |event| {
        if event.leg_mode.external() == "car" {
            let mut raw = arrivals.borrow_mut();
            let person = raw
                .persons
                .entry(event.person.external().to_string())
                .or_default();
            assert!(person.arrival.replace(event.time).is_none());
        }
    });
    let traffic = Rc::clone(&raw);
    events.on::<VehicleEntersTrafficEvent, _>(move |event| {
        assert_eq!(event.network_mode.external(), "car");
        assert_eq!(event.link.external(), "departure");
        assert!(
            traffic
                .borrow_mut()
                .vehicles
                .insert(
                    event.vehicle.external().to_string(),
                    RawVehicle {
                        person: event.person.external().to_string(),
                        ..Default::default()
                    }
                )
                .is_none()
        );
    });
    let enters = Rc::clone(&raw);
    events.on::<LinkEnterEvent, _>(move |event| {
        let mut raw = enters.borrow_mut();
        let vehicle = raw.vehicles.get_mut(event.vehicle.external()).unwrap();
        assert!(
            vehicle
                .enters
                .insert(event.link.external().to_string(), event.time)
                .is_none()
        );
    });
    let leaves = Rc::clone(&raw);
    events.on::<LinkLeaveEvent, _>(move |event| {
        let mut raw = leaves.borrow_mut();
        let vehicle = raw.vehicles.get_mut(event.vehicle.external()).unwrap();
        assert!(
            vehicle
                .leaves
                .insert(event.link.external().to_string(), event.time)
                .is_none()
        );
    });
    read_events(&mut events, path).unwrap();
    let raw = raw.take();
    assert_eq!(raw.persons.len(), NUM_PERSONS);
    assert_eq!(raw.vehicles.len(), NUM_PERSONS);

    let mut observations = Observations {
        trips: Vec::new(),
        bins: BTreeMap::new(),
    };
    let mut persons_seen = BTreeSet::new();
    for vehicle in raw.vehicles.values() {
        assert!(
            persons_seen.insert(vehicle.person.as_str()),
            "Duplicate car trip for {}",
            vehicle.person
        );
        let index: usize = vehicle
            .person
            .strip_prefix("person-")
            .unwrap()
            .parse()
            .unwrap();
        assert!(index < NUM_PERSONS);
        assert_eq!(vehicle.person, format!("person-{index:02}"));
        let route = case.route(index);
        let branch = match route {
            RouteChoice::Short => ["short-1", "short-2"],
            RouteChoice::Long => ["long-1", "long-2"],
        };
        assert_eq!(
            vehicle
                .enters
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["start", branch[0], branch[1], "end"]),
            "Unexpected entered links for {}",
            vehicle.person
        );
        assert_eq!(
            vehicle
                .leaves
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["departure", "start", branch[0], branch[1]]),
            "Unexpected left links for {}",
            vehicle.person
        );
        let person = &raw.persons[&vehicle.person];
        let departure = person.departure.expect("Missing car departure");
        let arrival = person.arrival.expect("Missing car arrival");
        assert!(arrival >= departure);
        assert!(vehicle.leaves["departure"] >= departure);
        // Every regular link has one matched pair, and adjacent links share
        // the same transition timestamp. End has an arrival instead of a leave.
        let mut previous_leave = vehicle.leaves["departure"];
        for link in ["start", branch[0], branch[1]] {
            let enter = vehicle.enters[link];
            let leave = vehicle.leaves[link];
            assert_eq!(
                enter, previous_leave,
                "Discontinuous trip for {} on {link}",
                vehicle.person
            );
            assert!(leave >= enter);
            let bin_start = enter.as_secs() / BIN_SIZE * BIN_SIZE;
            observations
                .bins
                .entry((link.to_string(), bin_start))
                .or_default()
                .add(leave.duration_since(enter));
            previous_leave = leave;
        }
        assert_eq!(vehicle.enters["end"], previous_leave);
        assert!(arrival >= previous_leave);
        observations.trips.push(Trip {
            person: vehicle.person.clone(),
            index,
            route,
            departure,
            travel_time: arrival.duration_since(departure),
            departure_wait: vehicle.leaves["departure"].duration_since(departure),
        });
    }
    assert_eq!(
        observations
            .bins
            .iter()
            .filter(|((link, _), _)| link == "start")
            .map(|(_, samples)| samples.count)
            .sum::<usize>(),
        NUM_PERSONS
    );
    observations
}

fn print_trip_means(case: Case, trips: &[Trip], window: Option<(u64, u64)>) -> [Samples; 2] {
    let mut means = [Samples::default(), Samples::default()];
    for trip in trips {
        if let Some((start, end)) = window {
            if trip.departure < SimTime::from_secs(start)
                || trip.departure >= SimTime::from_secs(end)
            {
                continue;
            }
        }
        let route = if trip.route == RouteChoice::Short {
            0
        } else {
            1
        };
        means[route].add(trip.travel_time);
    }
    for (route, samples) in ["short", "long"].into_iter().zip(&means) {
        if samples.count == 0 {
            println!("{case:?}: {route}, window {window:?}: no trips");
        } else {
            println!(
                "{case:?}: {route}, window {window:?}: {} trips, car mean {:.2}s",
                samples.count,
                samples.mean()
            );
        }
    }
    means
}

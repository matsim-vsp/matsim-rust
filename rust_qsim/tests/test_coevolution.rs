#[path = "common/two_route_fixture.rs"]
mod two_route_fixture;

use macros::deterministic_id_test;
// The shared fixture imports crate::simulation in either test compilation context.
use rust_qsim::simulation;
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::events::utils::{
    compare_event_folder, read_events, read_partitioned_events,
};
use rust_qsim::simulation::events::{
    EventsManager, LinkEnterEvent, PersonArrivalEvent, PersonDepartureEvent,
    VehicleEntersTrafficEvent,
};
use rust_qsim::simulation::scoring::OnlyTravelTimeDependentScoring;
use rust_qsim::simulation::time::SimTime;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use two_route_fixture::{NUM_PERSONS, RouteChoice, two_route_scenario};

// Departures start at second 100; omit 600 seconds of warm-up and stop before
// the inflow ends. Four five-minute windows also expose continued queue growth.
const MEASUREMENT_START: u64 = 700;
const WINDOW_DURATION: u64 = 300;
const NUM_WINDOWS: usize = 4;
const MEASUREMENT_END: u64 = MEASUREMENT_START + WINDOW_DURATION * NUM_WINDOWS as u64;
// Fixed acceptance target for both route differences and temporal variation;
// allow small fluctuations from stochastic selection and discrete capacity slots.
const ROUTE_MEAN_TRAVEL_TIME_TOLERANCE: Duration = Duration::from_secs(10);

#[deterministic_id_test(rust_qsim)]
fn two_route_scenario_after_warm_up_single_threaded() {
    run_and_check_two_route_scenario("single_threaded", 1);
}

#[deterministic_id_test(rust_qsim)]
fn two_route_scenario_after_warm_up_multi_threaded() {
    run_and_check_two_route_scenario("multi_threaded", 2);
}

fn run_and_check_two_route_scenario(test_name: &str, num_parts: u32) {
    let reference = PathBuf::from("tests/resources/coevolution/expected_events");
    let actual = run_two_route_scenario(
        num_parts,
        &PathBuf::from("./test_output/simulation/test_coevolution")
            .join(test_name)
            .join("output"),
    );

    let actual_times = route_travel_times(|events| {
        read_partitioned_events(events, &actual, "events", num_parts, "xml.zst").unwrap();
    });
    // Check the scenario independently before accepting any reference events.
    assert_route_travel_times_close(test_name, &actual_times);

    compare_event_folder(&reference, &actual)
        .unwrap_or_else(|error| panic!("Final iteration events differ for {test_name}: {error}"));

    let reference_times = route_travel_times(|events| {
        read_events(events, reference.join("events.0.xml")).unwrap();
    });
    assert_eq!(
        reference_times, actual_times,
        "Final iteration routes, departures or car travel times differ for {test_name}"
    );
}

fn run_two_route_scenario(num_parts: u32, output_dir: &Path) -> PathBuf {
    let scenario = two_route_scenario(num_parts, output_dir, |_| RouteChoice::Short);
    let last_iteration = scenario.config.controller().last_iteration;
    ControllerBuilder::default_with_scenario(scenario)
        .scoring_function(Box::new(OnlyTravelTimeDependentScoring))
        .build()
        .unwrap()
        .run();
    output_dir
        .join("ITERS")
        .join(format!("it.{last_iteration}"))
        .join("events")
}

#[derive(Debug, PartialEq, Eq)]
struct CarTrip {
    departure: SimTime,
    route: RouteChoice,
    travel_time: Duration,
}

#[derive(Default)]
struct ObservedCarTrip {
    departure: Option<SimTime>,
    arrival: Option<SimTime>,
    route: Option<RouteChoice>,
}

/// Measure the car leg from departure to arrival, including queueing, and
/// classify its route by the first branch link actually entered by its vehicle.
fn route_travel_times(read: impl FnOnce(&mut EventsManager)) -> BTreeMap<String, CarTrip> {
    let trips = Rc::new(RefCell::new(BTreeMap::<String, ObservedCarTrip>::new()));
    let vehicle_persons = Rc::new(RefCell::new(BTreeMap::<String, String>::new()));
    let mut events = EventsManager::new();

    // store departure time by person
    let departure_trips = Rc::clone(&trips);
    events.on::<PersonDepartureEvent, _>(move |departure| {
        if departure.leg_mode.external() == "car" {
            let mut trips = departure_trips.borrow_mut();
            let trip = trips
                .entry(departure.person.external().to_string())
                .or_default();
            assert!(trip.departure.replace(departure.time).is_none());
        }
    });

    // store person by vehicle
    let traffic_vehicle_persons = Rc::clone(&vehicle_persons);
    events.on::<VehicleEntersTrafficEvent, _>(move |traffic| {
        if traffic.network_mode.external() == "car" {
            traffic_vehicle_persons.borrow_mut().insert(
                traffic.vehicle.external().to_string(),
                traffic.person.external().to_string(),
            );
        }
    });

    // store route choice by person
    let entered_trips = Rc::clone(&trips);
    events.on::<LinkEnterEvent, _>(move |enter| {
        let route = match enter.link.external() {
            "short-1" => RouteChoice::Short,
            "long-1" => RouteChoice::Long,
            _ => return,
        };
        let vehicle_persons = vehicle_persons.borrow();
        let person = vehicle_persons.get(enter.vehicle.external()).unwrap();
        assert!(
            entered_trips
                .borrow_mut()
                .get_mut(person)
                .unwrap()
                .route
                .replace(route)
                .is_none()
        );
    });

    // store arrival time by person
    let arrival_trips = Rc::clone(&trips);
    events.on::<PersonArrivalEvent, _>(move |arrival| {
        if arrival.leg_mode.external() == "car" {
            let mut trips = arrival_trips.borrow_mut();
            let trip = trips
                .entry(arrival.person.external().to_string())
                .or_default();
            assert!(trip.arrival.replace(arrival.time).is_none());
        }
    });

    read(&mut events);
    let trips = trips.take();

    assert_eq!(trips.len(), NUM_PERSONS,);
    trips
        .into_iter()
        .map(|(person, trip)| {
            let departure = trip.departure.unwrap();
            let arrival = trip.arrival.unwrap();
            let route = trip.route.unwrap();
            assert!(
                arrival >= departure,
                "Arrival precedes departure for {person}"
            );
            (
                person,
                CarTrip {
                    departure,
                    route,
                    travel_time: arrival.duration_since(departure),
                },
            )
        })
        .collect()
}

/// Check two things here (after a warmup phase of 700 seconds):
/// 1) The mean travel times for the two routes are close to each other over the entire measurement period.
/// 2) The mean travel times for each route do not vary too much over the measurement windows.
fn assert_route_travel_times_close(test_name: &str, travel_times: &BTreeMap<String, CarTrip>) {
    let overall =
        route_mean_travel_times(test_name, travel_times, MEASUREMENT_START, MEASUREMENT_END);
    let windows: [[f64; 2]; NUM_WINDOWS] = std::array::from_fn(|index| {
        let start = MEASUREMENT_START + index as u64 * WINDOW_DURATION;
        route_mean_travel_times(test_name, travel_times, start, start + WINDOW_DURATION)
    });
    let tolerance = ROUTE_MEAN_TRAVEL_TIME_TOLERANCE.as_secs_f64();
    // Report both route differences and temporal drift, even if either fails.
    let mut failures = Vec::new();

    for (start, end, means) in std::iter::once((MEASUREMENT_START, MEASUREMENT_END, overall)).chain(
        windows.iter().enumerate().map(|(index, means)| {
            let start = MEASUREMENT_START + index as u64 * WINDOW_DURATION;
            (start, start + WINDOW_DURATION, *means)
        }),
    ) {
        let difference = (means[0] - means[1]).abs();
        if difference > tolerance {
            failures.push(format!(
                "Route means differ by {difference:.2}s for {test_name} in [{start}, {end}), \
                 exceeding {tolerance}s: short {:.2}s, long {:.2}s",
                means[0], means[1]
            ));
        }
    }

    for (route_index, route_name) in ["short", "long"].into_iter().enumerate() {
        let (min_index, min_means) = windows
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| left[route_index].total_cmp(&right[route_index]))
            .unwrap();
        let (max_index, max_means) = windows
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left[route_index].total_cmp(&right[route_index]))
            .unwrap();
        let variation = max_means[route_index] - min_means[route_index];
        let min_start = MEASUREMENT_START + min_index as u64 * WINDOW_DURATION;
        let max_start = MEASUREMENT_START + max_index as u64 * WINDOW_DURATION;
        println!("{test_name}: {route_name} window mean variation {variation:.2}s");
        if variation > tolerance {
            failures.push(format!(
                "{route_name} route means vary by {variation:.2}s for {test_name}, exceeding \
                 {tolerance}s: minimum {:.2}s in [{min_start}, {}), maximum {:.2}s in [{max_start}, {})",
                min_means[route_index],
                min_start + WINDOW_DURATION,
                max_means[route_index],
                max_start + WINDOW_DURATION
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn route_mean_travel_times(
    test_name: &str,
    travel_times: &BTreeMap<String, CarTrip>,
    start: u64,
    end: u64,
) -> [f64; 2] {
    let mut counts = [0_usize; 2];
    let mut total_tt = [Duration::ZERO; 2];
    for trip in travel_times.values() {
        // Filter by actual car departure, not arrival: include the entire trip,
        // even if it finishes after this half-open measurement window.
        if trip.departure < SimTime::from_secs(start) || trip.departure >= SimTime::from_secs(end) {
            continue;
        }
        let index = match trip.route {
            RouteChoice::Short => 0,
            RouteChoice::Long => 1,
        };
        counts[index] += 1;
        total_tt[index] += trip.travel_time;
    }
    assert!(
        counts.iter().all(|count| *count > 0),
        "Both routes must be used for {test_name} in [{start}, {end}): {counts:?}"
    );
    let short_mean = total_tt[0].as_secs_f64() / counts[0] as f64;
    let long_mean = total_tt[1].as_secs_f64() / counts[1] as f64;
    let difference = (short_mean - long_mean).abs();
    let tolerance = ROUTE_MEAN_TRAVEL_TIME_TOLERANCE.as_secs_f64();
    println!(
        "{test_name}: final car travel times in [{start}, {end}): short {short_mean:.2}s ({} persons), \
         long {long_mean:.2}s ({} persons), difference {difference:.2}s (tolerance {tolerance}s)",
        counts[0], counts[1]
    );
    [short_mean, long_mean]
}

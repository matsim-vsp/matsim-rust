//! Opt-in, real-scenario benchmark. Keep this harness identical on both source revisions.
use super::*;
use crate::simulation::events::{LinkEnterEvent, PersonArrivalEvent};
use crate::simulation::framework_events::MobsimEvent;
use macros::deterministic_id_test;
use std::cell::Cell;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Clone, Copy, std::fmt::Debug, Default, PartialEq, Eq)]
struct Counts {
    completed_trips: u64,
    link_entries: u64,
}

#[deterministic_id_test]
#[ignore = "Berlin performance measurement: requires --release and eight fresh scenario loads"]
fn berlin_performance() {
    assert!(!cfg!(debug_assertions), "Run this benchmark with --release");
    let dynamics = std::env::var("MATSIM_BENCH_DYNAMICS").unwrap_or("Queue".into());
    assert!(matches!(dynamics.as_str(), "Queue" | "KinematicWaves"));
    for parts in [1, 2] {
        let mut measurements = Vec::new();
        let mut expected_counts = None;
        for run in 0..4 {
            let (elapsed, counts) = run_once(parts, &dynamics);
            if let Some(expected) = expected_counts {
                assert_eq!(expected, counts, "Fresh runs must reproduce result counts");
            }
            expected_counts = Some(counts);
            println!(
                "BERLIN_RUN dynamics={dynamics} parts={parts} run={run} warmup={} seconds={:.6} completed_trips={} link_entries={}",
                run == 0,
                elapsed.as_secs_f64(),
                counts.completed_trips,
                counts.link_entries
            );
            if run > 0 {
                measurements.push(elapsed.as_secs_f64());
            }
        }
        measurements.sort_by(f64::total_cmp);
        let counts = expected_counts.unwrap();
        println!(
            "BERLIN_SUMMARY dynamics={dynamics} parts={parts} median={:.6} min={:.6} max={:.6} range={:.6} completed_trips={} link_entries={}",
            measurements[1],
            measurements[0],
            measurements[2],
            measurements[2] - measurements[0],
            counts.completed_trips,
            counts.link_entries
        );
    }
}

fn run_once(parts: u32, dynamics: &str) -> (Duration, Counts) {
    let mut config = Config::from_path("./assets/berlin-v6.4/config.yml");
    // Deserialize through the public config representation so that the same harness also builds
    // against the pre-implementation revision, which only supports Queue.
    let mut qsim = serde_yaml::to_value(config.qsim()).unwrap();
    qsim["traffic_dynamics"] = serde_yaml::Value::String(dynamics.into());
    config.set_qsim(serde_yaml::from_value(qsim).unwrap());
    config.partitioning_mut().num_parts = parts;
    config.output_mut().write_events = WriteEvents::None;
    config.output_mut().output_dir = PathBuf::from(format!(
        "./test_output/berlin-performance/{dynamics}-{parts}"
    ));
    assert_eq!(0, config.controller().first_iteration);
    assert_eq!(0, config.controller().last_iteration);
    assert_eq!(0.001, config.qsim().sample_size);

    let totals = Arc::new(Mutex::new(Counts::default()));
    let mut listeners: HashMap<u32, Vec<Box<WorkerListenerRegisterFunction>>> = HashMap::new();
    for rank in 0..parts {
        let totals = totals.clone();
        listeners.insert(
            rank,
            vec![Box::new(move |events, mobsim, _, _| {
                // No cross-partition lock in either per-vehicle callback.
                let counts = Rc::new(Cell::new(Counts::default()));
                let arrivals = counts.clone();
                events.on::<PersonArrivalEvent, _>(move |_| {
                    let mut count = arrivals.get();
                    count.completed_trips += 1;
                    arrivals.set(count);
                });
                let entries = counts.clone();
                events.on::<LinkEnterEvent, _>(move |_| {
                    let mut count = entries.get();
                    count.link_entries += 1;
                    entries.set(count);
                });
                mobsim.on_event(move |event| {
                    if matches!(&event.payload, MobsimEvent::BeforeCleanup) {
                        let mut total = totals.lock().unwrap();
                        total.completed_trips += counts.get().completed_trips;
                        total.link_entries += counts.get().link_entries;
                    }
                });
            })],
        );
    }

    // Reading, METIS, plan preparation, partition construction and worker startup are outside
    // the timer. Worker initialization on the first command is part of run_mobsim. Every call
    // owns a fresh scenario, capacities, population and worker pool.
    let scenario = Scenario::load(config);
    let mut controller = ControllerBuilder::default_with_scenario(scenario)
        .scoring_function(Box::new(scoring::OnlyTravelTimeDependentScoring))
        .worker_listener_register_fn(listeners)
        .build()
        .unwrap();
    prepare_for_mobsim(&mut controller.scenario, &controller.trip_router).unwrap();
    let inputs = controller
        .scenario
        .split_for_mobsim(&controller.link_storage_capacities);
    let output = io::resolve_path(
        controller.config.context(),
        &controller.config.output().output_dir,
    );
    prepare_output_directory(&output, controller.config.output().overwrite_files).unwrap();
    fs::create_dir_all(output.join("logs")).unwrap();
    let mut workers = controller.start_mobsim_workers();
    let start = Instant::now();
    let agents = workers.run_mobsim(0, true, inputs);
    let elapsed = start.elapsed();
    // run_mobsim waits for every partition, including communication and final synchronization.
    workers.shutdown();
    drop(agents);
    let counts = *totals.lock().unwrap();
    (elapsed, counts)
}

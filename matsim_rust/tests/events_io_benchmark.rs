//! Benchmarks for reading event files. They need event files of a Berlin v7.1 1pct run, which are
//! not part of the repository, and are therefore ignored by default. First generate the event files
//! (this runs one iteration of the simulation with 4 partitions), then run the benchmarks in release
//! mode:
//!
//! ```shell
//! cargo test -p matsim-rust --release --test events_io_benchmark generate -- --ignored --nocapture --test-threads=1
//! cargo test -p matsim-rust --release --test events_io_benchmark read -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `read_events_reproduces_files` checks that reading the generated files reproduces them exactly,
//! i.e., that the order of the events is unchanged.
//!
//! The config of the run can be set with `MATSIM_RUST_BERLIN_V71_1PCT_CONFIG`. By default, it is
//! expected in a parallel-qsim-berlin checkout next to this repository. The event files are written
//! to `MATSIM_RUST_BERLIN_V71_1PCT_EVENTS_DIR`, by default below `test_output/benchmarks`.

use flate2::read::GzDecoder;
use macros::deterministic_id_test;
use matsim_rust::simulation::config::{CompressionType, Config, OverwriteFiles, WriteEvents};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::events::EventsManager;
use matsim_rust::simulation::events::utils::{
    convert_proto_to_xml_events, read_events, read_partitioned_events,
};
use matsim_rust::simulation::id;
use matsim_rust::simulation::io::xml::events::XmlEventsWriter;
use matsim_rust::simulation::scenario::Scenario;
use matsim_rust::simulation::scoring::OnlyTravelTimeDependentScoring;
use std::cell::Cell;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

const CONFIG_ENV: &str = "MATSIM_RUST_BERLIN_V71_1PCT_CONFIG";
const EVENTS_DIR_ENV: &str = "MATSIM_RUST_BERLIN_V71_1PCT_EVENTS_DIR";
const NUM_PARTS: u32 = 4;

#[deterministic_id_test(matsim_rust)]
#[ignore = "generates the input of the event benchmarks, needs the Berlin v7.1 1pct input"]
fn generate_berlin_v71_1pct_events() {
    let config_path = std::env::var_os(CONFIG_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../parallel-qsim-berlin/input/v7.1/berlin-v7.1.1pct.config.yml")
        });
    if !config_path.exists() {
        println!("Skipping: {config_path:?} does not exist. Set {CONFIG_ENV} to the config file.");
        return;
    }

    let mut config = Config::from_path(&config_path);
    config.partitioning_mut().num_parts = NUM_PARTS;
    let output = config.output_mut();
    output.output_dir = output_dir();
    output.overwrite_files = OverwriteFiles::DeleteDirectoryIfExists;
    output.write_events = WriteEvents::File;
    let controller = config.controller_mut();
    controller.first_iteration = 0;
    controller.last_iteration = 0;
    controller.compression_type = CompressionType::Proto;

    let scenario = Scenario::load(config);
    ControllerBuilder::default_with_scenario(scenario)
        .scoring_function(Box::new(OnlyTravelTimeDependentScoring))
        .build()
        .unwrap()
        .run();

    // The ids of the run are still in the store, so that the proto events can be converted.
    let xml = events_dir().join("events.xml.gz");
    timed("convert proto events to xml", || {
        convert_proto_to_xml_events(events_dir(), NUM_PARTS, xml).unwrap();
    });
}

#[deterministic_id_test(matsim_rust)]
#[ignore = "benchmark, needs generated Berlin v7.1 1pct events"]
fn read_partitioned_proto_events() {
    if !inputs_exist() {
        return;
    }
    timed("load ids", || id::load_from_file(&ids_path()));
    let (mut manager, count) = counting_manager();
    let start = Instant::now();
    read_partitioned_events(&mut manager, events_dir(), "events", NUM_PARTS, "binpb").unwrap();
    report("partitioned proto events", start, count.get());
}

#[deterministic_id_test(matsim_rust)]
#[ignore = "benchmark, needs generated Berlin v7.1 1pct events"]
fn read_xml_events() {
    if !inputs_exist() {
        return;
    }
    timed("load ids", || id::load_from_file(&ids_path()));
    let (mut manager, count) = counting_manager();
    let start = Instant::now();
    read_events(&mut manager, events_dir().join("events.xml.gz")).unwrap();
    report("xml events", start, count.get());
}

#[deterministic_id_test(matsim_rust)]
#[ignore = "benchmark, needs generated Berlin v7.1 1pct events"]
fn read_xml_events_without_ids() {
    if !inputs_exist() {
        return;
    }
    // All ids are created while reading.
    let (mut manager, count) = counting_manager();
    let start = Instant::now();
    read_events(&mut manager, events_dir().join("events.xml.gz")).unwrap();
    report("xml events without ids", start, count.get());
}

#[deterministic_id_test(matsim_rust)]
#[ignore = "benchmark, needs generated Berlin v7.1 1pct events"]
fn read_proto_and_write_xml_events() {
    if !inputs_exist() {
        return;
    }
    timed("load ids", || id::load_from_file(&ids_path()));
    let output = output_dir().join("benchmark").join("events.xml.gz");
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    timed("convert proto events to xml", || {
        convert_proto_to_xml_events(events_dir(), NUM_PARTS, output).unwrap();
    });
}

#[deterministic_id_test(matsim_rust)]
#[ignore = "needs generated Berlin v7.1 1pct events"]
fn read_events_reproduces_files() {
    if !inputs_exist() {
        return;
    }
    id::load_from_file(&ids_path());
    let xml = events_dir().join("events.xml.gz");
    let folder = output_dir().join("reproduction");
    std::fs::create_dir_all(&folder).unwrap();

    // Merging the partitions again yields the file written when the events were generated.
    let converted = folder.join("converted.xml.gz");
    convert_proto_to_xml_events(events_dir(), NUM_PARTS, converted.clone()).unwrap();
    assert_same_lines(&xml, &converted);

    // Reading the XML file and writing its events again yields the same file.
    let rewritten = folder.join("rewritten.xml.gz");
    let mut manager = EventsManager::new();
    XmlEventsWriter::register_fn(rewritten.clone())(&mut manager);
    read_events(&mut manager, &xml).unwrap();
    assert_same_lines(&xml, &rewritten);
}

fn assert_same_lines(expected: &Path, actual: &Path) {
    let open = |path: &Path| BufReader::new(GzDecoder::new(File::open(path).unwrap())).lines();
    let mut actual_lines = open(actual);
    let mut count = 0;
    for (number, expected_line) in open(expected).enumerate() {
        let actual_line = actual_lines
            .next()
            .unwrap_or_else(|| panic!("{actual:?} ends before line {}", number + 1));
        assert_eq!(
            expected_line.unwrap(),
            actual_line.unwrap(),
            "line {} of {actual:?}",
            number + 1
        );
        count += 1;
    }
    assert!(actual_lines.next().is_none(), "{actual:?} has more lines");
    println!("{actual:?} has the same {count} lines as {expected:?}");
}

fn output_dir() -> PathBuf {
    std::env::var_os(EVENTS_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("test_output/benchmarks/berlin-v7.1-1pct-events")
        })
}

fn events_dir() -> PathBuf {
    output_dir().join("events")
}

fn ids_path() -> PathBuf {
    output_dir().join("output_ids.binpb")
}

fn inputs_exist() -> bool {
    let mut required = vec![ids_path(), events_dir().join("events.xml.gz")];
    required.extend((0..NUM_PARTS).map(|i| events_dir().join(format!("events.{i}.binpb"))));
    if let Some(missing) = required.iter().find(|path| !path.exists()) {
        println!(
            "Skipping benchmark: {missing:?} does not exist. Run generate_berlin_v71_1pct_events first or set {EVENTS_DIR_ENV}."
        );
        return false;
    }
    println!(
        "Event files: {:.1} MB proto, {:.1} MB xml.gz",
        (0..NUM_PARTS)
            .map(|i| file_size(&events_dir().join(format!("events.{i}.binpb"))))
            .sum::<u64>() as f64
            / 1e6,
        file_size(&events_dir().join("events.xml.gz")) as f64 / 1e6
    );
    true
}

fn counting_manager() -> (EventsManager, Rc<Cell<u64>>) {
    let count = Rc::new(Cell::new(0));
    let counter = count.clone();
    let mut manager = EventsManager::new();
    manager.on_any(move |_| counter.set(counter.get() + 1));
    (manager, count)
}

fn report(label: &str, start: Instant, events: u64) {
    let secs = start.elapsed().as_secs_f64();
    println!(
        "BENCHMARK {label}: {secs:.3}s, {events} events, {:.0} events/s, {} rayon threads",
        events as f64 / secs,
        rayon::current_num_threads()
    );
    assert!(events > 0);
}

fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    println!("BENCHMARK {label}: {:.3}s", start.elapsed().as_secs_f64());
    result
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("Could not read metadata of {path:?}: {e}"))
        .len()
}

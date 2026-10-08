//! Benchmarks for reading large populations. They need the Berlin v7.1 1pct input files, which
//! are not part of the repository, and are therefore ignored by default. Run them in release mode:
//!
//! ```shell
//! cargo test -p matsim-rust --release --test population_io_benchmark -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The input directory can be set with `MATSIM_RUST_BERLIN_V71_1PCT_DIR`. By default, it is
//! expected in a shared-svn checkout next to the directory containing this repository.

use macros::deterministic_id_test;
use matsim_rust::simulation::config::PartitionMethod;
use matsim_rust::simulation::id;
use matsim_rust::simulation::scenario::network::Network;
use matsim_rust::simulation::scenario::population::Population;
use matsim_rust::simulation::scenario::vehicles::Garage;
use std::path::{Path, PathBuf};
use std::time::Instant;

const DIR_ENV: &str = "MATSIM_RUST_BERLIN_V71_1PCT_DIR";
const EXPECTED_PERSONS: usize = 52_301;

const IDS: &str = "berlin-v7.1.ids.binpb";
const NETWORK: &str = "berlin-v7.1.network.binpb";
const VEHICLES: &str = "berlin-v7.1.vehicles.binpb";
const PROTO_PLANS: &str = "berlin-v7.1.plans.binpb";
const XML_PLANS: &str = "population-filtered.1pct.xml.gz";

#[deterministic_id_test(matsim_rust)]
#[ignore = "benchmark, needs the Berlin v7.1 1pct input from shared-svn"]
fn read_proto_population_berlin_v71_1pct() {
    bench_population(PROTO_PLANS);
}

#[deterministic_id_test(matsim_rust)]
#[ignore = "benchmark, needs the Berlin v7.1 1pct input from shared-svn"]
fn read_xml_population_berlin_v71_1pct() {
    bench_population(XML_PLANS);
}

fn bench_population(plans_file: &str) {
    let dir = input_dir();
    let required = [IDS, NETWORK, VEHICLES, plans_file].map(|file| dir.join(file));
    if let Some(missing) = required.iter().find(|path| !path.exists()) {
        println!(
            "Skipping benchmark: {missing:?} does not exist. Set {DIR_ENV} to the input directory."
        );
        return;
    }
    let [ids_path, network_path, vehicles_path, plans_path] = required;

    // Load everything the population depends on in the same order as Scenario::load, so that
    // links and vehicles already exist in the id store when the population is read.
    timed("ids", || id::load_from_file(&ids_path));
    let _network = timed("network", || {
        Network::from_file_path(&network_path, 1, &PartitionMethod::None)
    });
    let mut garage = timed("vehicles", || Garage::from_file(&vehicles_path));

    let start = Instant::now();
    let population = Population::from_file(&plans_path, &mut garage);
    let duration = start.elapsed();

    let file_mb = file_size(&plans_path) as f64 / 1e6;
    let secs = duration.as_secs_f64();
    println!(
        "BENCHMARK population {plans_file}: {secs:.3}s, {} persons, {:.0} persons/s, {file_mb:.1} MB on disk, {:.1} MB/s, {} rayon threads",
        population.persons.len(),
        population.persons.len() as f64 / secs,
        file_mb / secs,
        rayon::current_num_threads(),
    );

    assert_eq!(EXPECTED_PERSONS, population.persons.len());
}

fn input_dir() -> PathBuf {
    std::env::var_os(DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../shared-svn/projects/rust-qsim/berlin-v7.1/1pct")
        })
}

fn timed<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = f();
    println!(
        "BENCHMARK setup {label}: {:.3}s",
        start.elapsed().as_secs_f64()
    );
    result
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path)
        .unwrap_or_else(|e| panic!("Could not read metadata of {path:?}: {e}"))
        .len()
}

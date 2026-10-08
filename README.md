# MATSim Rust

This project implements MATSim's QSim in Rust, with multithreaded network simulation,
iterative scoring and replanning, and final-iteration analysis. It aims to preserve
MATSim Java behavior and event semantics. This project served
as [reference implementation](https://github.com/matsim-org/matsim-libs/pull/4255) for a distributed version of the QSim
in the MATSim-Java core.

The most recent release can be cited with the following reference

[![DOI](https://zenodo.org/badge/498376436.svg)](https://zenodo.org/doi/10.5281/zenodo.13928119)

The project is described in a journal publication:

- [High-Performance Mobility Simulation: Implementation of a Parallel Distributed Message-Passing Algorithm for MATSim](https://doi.org/10.3390/info16020116)

And two conference papers, which were presented at ISPDC 24 in Chur, Switzerland, July 2024:

- [High-Performance Simulations for Urban Planning: Implementing Parallel Distributed Multi-Agent Systems in MATSim](https://doi.org/10.1109/ISPDC62236.2024.10705395)
- [Real-Time Routing in Traffic Simulations: A Distributed Event Processing Approach](https://doi.org/10.1109/ISPDC62236.2024.10705399)

## Current capabilities

Recent upgrades extend the simulation, routing, and analysis workflow:

- Persistent partition workers share immutable scenario data, collect experienced plans,
  and publish mode-specific travel-time snapshots for the next iteration's routing.
- Public transport vehicles can run through the network, board and alight passengers,
  and compete for link capacity. Enable this with `transit.simulate_vehicles: true`;
  transit legs are teleported by default.
- Activity facilities support mode-specific link selection. The controller also accepts
  custom scoring functions and replanning strategies.
- Traffic signals use approach-link green windows. `qsim.remove_stuck_vehicles: true`
  removes blocked vehicles after the stuck threshold; the default forces them onward.
  See [architecture](docs/architecture.md) for ownership and MATSim compatibility limits.
- A* routing reuses search buffers and initializes only discovered nodes. Adaptive
  rerouting and batched route proposals are opt-in. The route cache is disabled by
  default; setting `MATSIM_ENABLE_ROUTE_CACHE` enables it, regardless of the variable's
  value. See [routing experiments](docs/katgpt-integration-opportunities.md) for the
  implemented techniques, their limits, and the reproducible experiment runner.
- The post-simulation SILO service exposes route queries using simulated travel times,
  with explicit failure categories.
- Automatic reports cover network use, congestion, journeys, activity patterns, transit,
  and policy comparisons. Optional inputs add validation, accessibility, equity,
  economic appraisal, and externally modeled environmental outcomes.

## How this project is organized

The project is organized as a cargo workspace with multiple crates. The main crates are:

- `matsim-rust` (directory `matsim_rust`): The core library containing the simulation logic
- `macros`: The crate containing (test) macros used in the project

Work with the `matsim-rust` crate for the simulation. The other crates are only for development purposes. In Rust
code, the crate is imported as `matsim_rust`.

Up to version 0.3.0, the core crate was called `rust_qsim` and the repository `parallel_qsim_rust`.

See [architecture](docs/architecture.md), [testing](docs/tests.md), and
[analysis](docs/analysis.md) for the detailed contracts.

## Prerequisites

The project relies on METIS as external dependency. This means this dependency is not
compiled with the project, but need to be present on the operating system.

### METIS

The project uses the [metis](https://crates.io/crates/metis) crate as a dependency which
is a wrapper for the [METIS C Library](https://github.com/KarypisLab/METIS). The C-Library is
expected to be present on the machine. Also, the `metis` crate requires `libclang` on the machine
this project is built on.

### MPI -- deprecated

Up to version 0.2.0, the project supported MPI as a feature for distributed execution. We decided to not support MPI
anymore. The code is still present in the repository, but not maintained anymore. If you want to use MPI, please
checkout version 0.2.0 or earlier.

Currently, only Rust's multithreading capabilities are used for parallelism.

### Install dependencies

Install the dependencies below before building. Rustup uses the version pinned in
`rust-toolchain.toml`, currently Rust 1.94.0. The crates use Rust 2024.

#### Linux - apt

Install dev versions of required packages because dev stuff is required during compilation.
`build-essential` and `cmake` are needed because the `protobuf-src` dependency compiles
the `protoc` compiler from source.

```shell
sudo apt -y install build-essential cmake libclang-dev llvm-dev libmetis-dev
```

#### macOS

The dependencies are available via [homebrew](https://brew.sh/) on macOS.

```shell
brew install metis cmake
```

The project contains a `config.toml` which tries to set the `CPATH` and the `RUSTFLAGS` environment variable. In case
this doesn't work, they need to be set like the following:

```shell
export CPATH=$HOMEBREW_PREFIX/include
export RUSTFLAGS="-L$HOMEBREW_PREFIX/lib"
```

The variables are necessary to compile the METIS library.

The `CXX` variable may also need to be set for compiling some included dependencies like `protobuf-src`.

```shell
export CXX=clang++
```

#### Math Cluster (TU Berlin)

The math cluster has all dependencies installed. They need to be enabled via the module system:

```shell
module load metis-5.1
```

#### HLRN (CPU-CLX Partition)

https://nhr-zib.atlassian.net/wiki/spaces/PUB/pages/430586/CPU+CLX+partition

##### Setup conda

Unfortunately, there is no `libclang` dependency installed. You need to install it yourself via `conda`. If you use it
for the first time, load the conda module and initialize it, such that it is available in your shell whenever you login:

```shell
module load anaconda3/2023.09
conda init bash
```

Then create your own environment and install `libclang` and `llvmdev`:

```shell
conda create -n your_env_name
conda activate your_env_name
conda install libclang llvmdev
```

Remember to pass the login at the beginning of SLURM scripts: `#!/bin/bash --login`.

##### Load dependencies

The HLRN cluster has **some** dependencies installed. They need to be enabled via the module system:

```shell
module load intel/2024.2
```

So, before you run the project, you need to activate the environment:

```shell
conda activate your_env_name
```

The activation automatically updates the environment variables such that `libclang` files can be found by the compiler.

Source: https://nhr-zib.atlassian.net/wiki/spaces/PUB/pages/430343/Anaconda+conda+and+Mamba

#### HLRN (CPU-GENOA Partition)

https://nhr-zib.atlassian.net/wiki/spaces/PUB/pages/119832634/CPU+Genoa+partition

##### Compilation

In contrast to the CLX partition, you don't need anaconda here. But, you need to compile with AMD compiler.

```shell
module load openmpi/aocc/5.0.3
export CC=clang
export CXX=clang++
export RUSTFLAGS="-C linker=clang"
```

Hint: You need to set these environment variables, otherwise there are compilation errors (paul, jan'25).
Hint 2: These settings are probably not necessary anymore without MPI usage (paul, sep'25).

##### Execution

For some reason, the runtime linker doesn't find the correct libraries. You need to add them manually before execution (
in the job script):

```shell
export LD_LIBRARY_PATH=$LD_LIBRARY_PATH:/sw/comm/openmpi/5.0.3/genoa.el9/aocc/lib
```

## Build

From the repository root, build the release binaries using the Rust toolchain pinned in
`rust-toolchain.toml` and the dependencies in `Cargo.lock`:

```shell
cargo build --release --locked
```

The binaries are written to `target/release/`. The main simulation executable is
`target/release/local_qsim`. The release profile enables optimizations and retains debug information.

## Test

Run the workspace tests without the long Berlin scenarios:

```shell
cargo test --workspace -- --test-threads=1 --skip berlin::
```

Add `--nocapture` for immediate output. Tests that use the global ID store or logging
use `#[deterministic_id_test]` for repeatable, serialized setup. See
[testing](docs/tests.md) for isolation rules and known native METIS failures.

The pull request checks use release mode, the optional `http` feature, and warnings as errors:

```shell
cargo fmt --all -- --check
RUSTFLAGS="-D warnings" cargo build --release --locked
RUSTFLAGS="-D warnings" cargo test --release --locked --features http -- --test-threads=1 --skip berlin::
```

Run Berlin integration tests separately, always in release mode. CI runs these on
`main` and manual dispatch, rather than on pull requests:

```shell
RUSTFLAGS="-D warnings" cargo test --release --locked --test simulation berlin:: -- --test-threads=1
```

## Run locally (multithreaded)

Execute

```shell
./target/release/local_qsim --config /path/to/config.yml
```

or

```shell
cargo run --release --bin local_qsim -- --config /path/to/config.yml
```

to run the simulation.

For a small XML-based scenario, run from the crate directory so the config's relative input paths resolve:

```
cd matsim_rust
cargo run --release --bin local_qsim -- --config tests/resources/equil/equil-config-1.yml
```

## Serve routes to SILO after QSim

With `--routing-service-ready-file`, `local_qsim` keeps its trip router and population alive after
the last iteration and answers route queries over TCP, so SILO can reuse the simulated travel times
for the rest of its year:

```shell
cargo run --release --bin local_qsim -- --config /path/to/config.yml --routing-service-ready-file /path/to/ready
```

The service binds to a free port on `127.0.0.1` and writes its address (for example
`127.0.0.1:41235`) to the ready file once it accepts connections. The process then keeps serving
until it is killed.

The config must list the modes to route in `routing.network_modes` (for example `[ "car" ]`).
Without it no network router exists: QSim stops in prepare-for-sim for plans with network legs,
and route requests for that mode fail with `invalid_request`. QSim also scores every plan, so
`scoring` needs parameters for each activity type, mode, and subpopulation in the population.

The protocol is line-delimited JSON: one request object per line, one response object per line.

```json
{"mode": "car", "from_x": -20000.0, "from_y": 0.0, "from_link_id": "1", "to_x": 0.0, "to_y": 0.0, "to_link_id": "20", "departure_time_seconds": 21600.0, "person_id": null}
```

```json
{"travel_time_seconds": 1234.0, "distance_meters": 25000.0, "error": null, "failure_category": null}
```

`person_id` is optional. Unknown links or persons, non-finite coordinates, and negative departure
times produce a response with `error` set instead of closing the connection. A request whose
origin and destination are the same link returns zero time and distance, as MATSim does for
intrazonal trips. Failed responses include `failure_category`: `malformed_request`,
`invalid_request`, `invalid_link`, `missing_person`, `no_path`, or `service_error`.

A response without a `failure_category` key comes from an older `local_qsim`, not from this
version. Clients degrade those to `service_error`, which hides which failures are ordinary
`no_path` answers, so check the binary's build when every failure looks the same.

For `pt` requests that no transit line connects, the transit router falls back to the car router
when one is configured, including for requests without a person. Without a fallback the response
reports the no-path error. See `matsim_rust/tests/resources/equil/equil-config-silo-routing.yml` for a
minimal config used by the integration test. Its network adds an unreachable `island` link to the
equil network, so the tests can produce a `no_path` answer.

## Analyze simulation results

Enable `output.analysis.enabled` in the config's Output module to write a
final-iteration report to `<output_dir>/analysis`. For example:

```yaml
modules:
  output:
    type: Output
    output_dir: ./output
    write_events: File
    analysis:
      enabled: true
      interval_seconds: 3600
```

Or enable analysis for an existing config from the command line:

```shell
cargo run --release --bin local_qsim -- --config /path/to/config.yml --set output.analysis.enabled=true
```

Open `analysis/index.html` for the offline report. CSV, JSON, and SVG exports include:

- Link volumes, coverage, PCE-weighted capacity utilization, speeds, vehicle distance
  and travel time, free-flow-relative delay, and en-route agent counts.
- Legs and journeys, mode shares, travel distributions, completion and stuck status,
  daily activity chains and durations, and activity/travel time reconciliation.
- Link classification and urban-area summaries. A supplied zone system adds zonal
  origin-destination flows and zone boundary crossings.
- Transit waiting and in-vehicle time, boardings, alightings, and occupancy where the
  recorded events and vehicle capacities support them. DRT and taxi service metrics
  likewise depend on recorded service events.
- Runtime context, including phase timings, worker count, build information, and
  available hardware and memory measurements.

Optional supplied inputs add observed traffic and transit validation, travel survey
comparison, accessibility to opportunities, demographic burdens and equity,
economic appraisal, modeled emissions, and modeled noise and exposure. The report
states missing inputs and unavailable metrics explicitly. It does not infer passenger
kilometers from vehicle counts, calculate emissions, or treat plan scores as welfare.
See [analysis](docs/analysis.md) for configuration, input formats, and metric definitions.

### Reanalyze a completed run

Regenerate a report from saved outputs without rerunning QSim:

```shell
cargo run --release --bin analyze -- --run-dir /path/to/output
```

Override the recorded interval width for the new report:

```shell
cargo run --release --bin analyze -- --run-dir /path/to/output --interval-seconds 1800
```

The command requires a run that already recorded an analysis report. It reads replay
settings from `analysis/manifest.json` and `analysis/run_metadata.json`, along with the
saved event files, output network, and ID store. It rewrites only analysis outputs.
Recorded classification, geography, and optional input paths are reused; externally
supplied files must still exist at those paths.

A completed report has `"status": "complete"` in its manifest. If a required module
fails, diagnostics go to `analysis-failure/` and the last complete report remains
intact. `module_status.json` distinguishes required and optional modules and records
unavailable inputs. Reanalysis exits non-zero on failure. A successful retry removes
the failure directory, and interrupted publication recovers the last good report.

### Compare completed runs

Compare saved journey mode shares across the latest completed iterations:

```shell
cargo run --release --bin analyze -- --run-dir /path/to/baseline --compare-run-dir /path/to/alternative
```

Repeat `--compare-run-dir` to add runs. The report is written to
`baseline/analysis/cross_run_comparison`. For broader metric and completion-status
comparisons, configure `output.analysis.comparison_runs` or use the
`compare_completed_runs` library interface documented in [analysis](docs/analysis.md).

Summarize seed uncertainty and parameter sensitivity with an ensemble manifest:

```shell
cargo run --release --bin analyze -- --run-dir /path/to/baseline --ensemble-manifest /path/to/ensemble.json
```

This reads completed runs and writes `baseline/ensemble/`. The manifest format,
compatibility checks, and statistical assumptions are documented in
[analysis](docs/analysis.md).

## Create input files

The simulator accepts XML and protobuf inputs. To convert XML inputs to protobuf
for faster loading, run:

```shell
cargo run --bin convert_to_binary --release -- --network network.xml --population population.xml --vehicles vehicles.xml --output-dir output --run-id run
```

Keep the generated ID store with its matching protobuf inputs and configure it via
`ids.path`. Internal IDs depend on that mapping.

Optionally, `--transit-schedule` and `--facilities` convert a transit schedule and an activity facilities file as
well. Facilities are referenced in the config via `facilities.path`.

## RustRover settings

If you use RustRover, you need to disable "Optimize Import". Otherwise, the imports are sorted differently compared to
`cargo fmt`. This is a known
issue: https://youtrack.jetbrains.com/issue/RUST-18774/Optimize-import-should-take-into-account-rustfmt-formatting-rules
Hopefully, this will be fixed soon.

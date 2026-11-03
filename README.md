# MATSim Rust

This is a port of MATSim to Rust. This project served
as [reference implementation](https://github.com/matsim-org/matsim-libs/pull/4255) for a distributed version of the QSim
in the MATSim-Java core.

The most recent release can be cited with the following reference

[![DOI](https://zenodo.org/badge/498376436.svg)](https://zenodo.org/doi/10.5281/zenodo.13928119)

The project is described in a journal publication:

- [High-Performance Mobility Simulation: Implementation of a Parallel Distributed Message-Passing Algorithm for MATSim](https://doi.org/10.3390/info16020116)

And two conference papers, which were presented at ISPDC 24 in Chur, Switzerland, July 2024:

- [High-Performance Simulations for Urban Planning: Implementing Parallel Distributed Multi-Agent Systems in MATSim](https://doi.org/10.1109/ISPDC62236.2024.10705395)
- [Real-Time Routing in Traffic Simulations: A Distributed Event Processing Approach](https://doi.org/10.1109/ISPDC62236.2024.10705399)

## How this project is organized

The project is organized as a cargo workspace with multiple crates. The main crates are:

- `matsim-rust` (directory `matsim_rust`): The core library containing the simulation logic
- `macros`: The crate containing (test) macros used in the project

Work with the `matsim-rust` crate for the simulation. The other crates are only for development purposes. In Rust
code, the crate is imported as `matsim_rust`.

Up to version 0.3.0, the core crate was called `rust_qsim` and the repository `parallel_qsim_rust`.

Check out further documentation in the `docs` folder.

## Set Up Prerequisites

Some dependencies are compiled from bundled C and C++ sources. Building the project therefore requires a C and C++
compiler and CMake. On Linux, OpenSSL is required as well, because the remote download support (feature `http`) and the
tests use `reqwest`.

MPI has not been supported for a long time; the last version supporting it was 0.2.0. Only Rust's multithreading
capabilities are used for parallelism.

### METIS

The project uses the [metis](https://crates.io/crates/metis) crate as a dependency which
is a wrapper for the [METIS C Library](https://github.com/KarypisLab/METIS). The C-Library is
compiled from the sources bundled with `metis-sys`, so an installed METIS library is not linked. The workspace takes
`metis-sys` from a fork (see `[patch.crates-io]` in `Cargo.toml`), which builds METIS such that partitions do not depend
on the platform.

### Install dependencies

The dependencies named above need to be installed before the project can be built.

#### Linux - apt

```shell
sudo apt -y install build-essential cmake pkg-config libssl-dev
```

#### macOS

The C and C++ compilers are part of the Xcode Command Line Tools. CMake is available via [homebrew](https://brew.sh/).

```shell
brew install cmake
```

The `CXX` variable may also need to be set for compiling some included dependencies like `protobuf-src`.

```shell
export CXX=clang++
```

#### Math Cluster (TU Berlin)

The math cluster has all dependencies installed.

#### HLRN (CPU-CLX Partition)

https://nhr-zib.atlassian.net/wiki/spaces/PUB/pages/430586/CPU+CLX+partition

##### Load dependencies

The HLRN cluster has **some** dependencies installed. They need to be enabled via the module system:

```shell
module load intel/2024.2
```

#### HLRN (CPU-GENOA Partition)

https://nhr-zib.atlassian.net/wiki/spaces/PUB/pages/119832634/CPU+Genoa+partition

Previously, the project was compiled with the AMD compiler from an OpenMPI module, which was needed for MPI. This is
probably not necessary anymore (paul, sep'25).

## Build

The project is built using cargo.

```shell
cargo build --release
```

## Test

To execute all tests run:

```
cargo test -- --test-threads=1
```

To have immediate output add `--nocapture` to the command.

Note (Sep 205): The `--test-threads=1` option is used currently to ensure that the global ID store does not get
overwritten by multiple parallel test threads. This will eventually be refined to allow all read-only tests to run in
parallel and forcing sequencial order only for read-write tests.

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

For example, after successfully running the tests first, try

```
cd matsim_rust
cargo run --release --bin local_qsim -- --config tests/resources/equil/equil-config-1.yml
```

To make runs traceable, the git state of the source tree is embedded at compile time (e.g. `v1.0.0-12-g4f96e9b2-dirty`).
It is printed by `local_qsim --version` and logged as the first message whenever logging is initialized, including the
per-process log files in the output directory.
The suffix `-dirty` means the binary was built with uncommitted changes to tracked files; `-nogit` means no git metadata
was available at build time.

## Create input files

You need to create protobuf files from the xml files. This can be done with the following command:

```shell
cargo run --bin convert_to_binary --release -- --network network.xml --population population.xml --vehicles vehicles.xml --output-dir output --run-id run
```

Optionally, `--transit-schedule` and `--facilities` convert a transit schedule and an activity facilities file as
well. Facilities are referenced in the config via `facilities.path`.

## RustRover settings

If you use RustRover, you need to disable "Optimize Import". Otherwise, the imports are sorted differently compared to
`cargo fmt`. This is a known
issue: https://youtrack.jetbrains.com/issue/RUST-18774/Optimize-import-should-take-into-account-rustfmt-formatting-rules
Hopefully, this will be fixed soon.

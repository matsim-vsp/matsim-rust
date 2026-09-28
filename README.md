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

- `rust-qsim`: The core library containing the simulation logic
- `macros`: The crate containing (test) macros used in the project

Work with the `rust-qsim` crate for the simulation. The other crates are only for development purposes.

Check out further documentation in the `docs` folder.

## Set Up Prerequisites

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

The dependencies named above need to be installed before the project can be buit

#### Linux - apt

Install dev versions of required packages because dev stuff is required during compilation

```shell
sudo apt -y install libclang-dev llvm-dev libmetis-dev
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
cd rust_qsim
cargo run --release --bin local_qsim -- --config tests/resources/equil/equil-config-1.yml
```

## Create input files

You need to create protobuf files from the xml files. This can be done with the following command:

```shell
cargo run --bin convert_to_binary --release -- --network network.xml --population population.xml --vehicles vehicles.xml --output-dir output --run-id run
```

## RustRover settings

If you use RustRover, you need to disable "Optimize Import". Otherwise, the imports are sorted differently compared to
`cargo fmt`. This is a known
issue: https://youtrack.jetbrains.com/issue/RUST-18774/Optimize-import-should-take-into-account-rustfmt-formatting-rules
Hopefully, this will be fixed soon.

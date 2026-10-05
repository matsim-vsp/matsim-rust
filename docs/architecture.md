# Architecture Overview

This document provides an overview of the architecture of the project, detailing its main components, their
interactions, and the overall design principles.

## `rust_qsim`

The core implementation of the Rust QSim is oriented towards [MATSim Java](https://github.com/matsim-org/matsim-libs).
In particular, we tried to minimize the differences between the physics of both simulations, including link dynamics
and output of events.

### General Scenario Handling

Starting the simulation mostly works as in MATSim Java. All XML input files need to be converted into protobuf for
faster reading. These files need to be referenced in a configuration file. Based on the config, a scenario is built,
based on that the controller -- pretty much like in MATSim Java.

Scenario ownership is split into three lifecycles. `Scenario` owns the input data while files are read.
The controller turns it into `ControllerScenario`, which keeps immutable data in a shared `ScenarioCore`
(`Arc<Network>`, `Arc<Garage>`, `Arc<Config>`) and owns the mutable `Population`.

For mobsim, the controller splits the population into `MobsimInput`s. Each input contains a `MobsimPartition` with the
shared scenario data and a fresh partition network runtime, plus a `PopulationShard`. Persistent QSim workers receive
these inputs per iteration and return agents, which the controller materializes back into the next full population.

Each worker owns a thread-local travel-time collector shared between its event buses without a cross-thread lock.
The collector associates vehicles with the network mode of their current leg and records link observations separately
per mode. After a worker's Mobsim run (including agent draining), it emits `BeforeCleanup` before sending its normal
worker result. A thread-local completion listener consolidates the collector's observed links and submits them to the
shared travel-time calculator. The last submission atomically publishes the complete, immutable snapshot. The controller
only waits for worker results, so publication has finished before `AfterMobsim`.
The shared router reads the snapshot without taking the submission lock; unobserved links use freespeed.
`prepare_for_sim` uses the previous iteration's snapshot, or an empty snapshot for the first iteration. The workers'
iteration-reset hooks clear the collectors before the next Mobsim. No event-file output is required for travel-time
collection.

### Final-Iteration Analysis

`output.analysis` publishes a local report once, after a successful run, from the last completed iteration only.
It replays the written event partitions, so it requires `output.write_events: File`, and it needs a positive
`qsim.sample_size` because observed volumes are scaled up to the unsampled population.

Volumes are passenger-car-equivalent weighted, which matches how `LocalLink` charges its flow cap, and the
V/C denominator is the link's own whole-link capacity multiplied by the interval width. Lane counts are
exported next to the capacity but never applied to it a second time. `link_capacity.csv` keeps raw vehicle
counts, observed PCE volumes and sample-scaled volumes as separate columns, and exports
`effective_capacity_pce`, the V/C denominator. A non-positive capacity leaves only the ratio blank, while
the volumes stay reportable because they do not involve the capacity; missing PCE invalidates the PCE
columns as well. Every case is reported per link through `entry_vc_status`/`exit_vc_status`, and
`vc_histogram.csv` separates links whose ratio is unusable from links that genuinely carried no traffic.
A link that carried no vehicles on a side is counted as unused whatever its capacity says, so an idle
network is not hidden behind unavailable ratios.

Each interval is credited only with the capacity of the window the simulation covered, so a final
interval that is shorter than `analysis.interval_seconds` is not treated as a whole one.

Every name in `metric_catalog.json` is the column it describes, so a consumer can look a metric up in
the table that exports it.

PCE totals are accumulated as exact integers at a fixed scale, not as running floating-point sums,
because floating-point addition does not commute. Otherwise the same vehicles crossing a link
simultaneously could produce totals that differ in the last bit, and a ratio sitting exactly on a
histogram bin edge would land in different bins depending on the order in which event partitions were
replayed.

### External Services

As a next step, we integrated the ability to communicate to external services. They are intended to be used during the
simulation for real-time updates of plans (like routing). We have seen in previous work that synchronous calls of such
services slow down the simulation a lot. This is why we implemented a more complex architecture allowing asynchronous
calls to such services.

During execution, we have the following threads running:

- $n$ QSim threads
- $1$ external service adapter thread
- $r$ routing communication threads (used by tokio runtime)

Both $n$ and $r$ are configurable. Any request to an external service is sent to the adapter thread via a channel.
Every thread is able to send requests to the adapter. The adapter thread allows abstraction of the actual external
service: it might mock a service, it can perform calculation itself or forward it to other threads, or it might forward
it to an actual external service.

In every case, the adapter thread answers requests asynchronously (see trait `RequestAdapter`). Therefore, a tokio
runtime is built starting its own threads.

## `macros`

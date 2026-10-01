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

Worker extensions can observe state moving between partitions through a fourth, thread-local
`PartitionChangeExtensionsManager` bus alongside the simulation-event, Mobsim-lifecycle, and partition-event buses.
When a vehicle or teleporting agent leaves a partition, the bus moves one typed attachment slot per registered
extension into the normal network message. The receiving worker installs all attachments before it emits partition
enter events and hands the entity to its local engine. The network message broker only transports these opaque slots;
it does not inspect or clone their contents.

Experienced-plan collection uses this migration bus. Each worker creates one `BackpackingEngine` in an
`Rc<RefCell<_>>`; its backpacks move with vehicles and teleporting agents and keep both their partial plans and any
future scoring events. At `BeforeCleanup`, every worker converts the backpacks currently on that partition into one
partial population and sends it to the controller over a dedicated backchannel before publishing its normal worker
result. Consequently, all partial populations are available when the controller emits `AfterMobsim`. The scoring
module verifies iteration and rank and merges the populations deterministically by person ID. The controller then
scores each reconstructed experienced plan and copies the result to exactly the selected original plan. Experienced
plans receive the same score and are written only when `scoring.write_experienced_plans` and the configured plan
writing interval allow it. Collection and scoring always run, even when experienced-plan output is disabled, so
replanning can consume the updated selected-plan scores. Backpacks do not return to an initial or "home" partition.

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

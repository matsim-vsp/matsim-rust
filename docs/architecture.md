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

`simulation::analysis` owns the final-iteration report. After a successful run, the controller calls
`analyze_final_iteration` once, which replays the final iteration's event partitions in
chronological order and publishes per-interval link-volume, link-speed, link-classification and
agent-travel tables plus a self-contained HTML report under `<output_dir>/analysis`. The interval
width comes from
`output.analysis.interval_seconds` (3600 by default), so the tables are hourly unless configured
otherwise. Inputs that cannot be recovered from the event files -- the final-iteration
expected-travel snapshot and the vehicle/PCE catalog -- are moved into a compact
`AnalysisRunMetadata` rather than by borrowing or copying the scenario; the run has finished by
then, so no population-scale clone happens.

`link_speed` and `agent_travel` are computed from that one replay, which returns them together as a
`ReplayedAnalysis`. Link speeds need the position along a link, so `link_visit` classifies the
four link events once and both the volumes and the speed collector agree on what an entry and an
exit are; see `analysis/link_speed.rs` for the full-link-traversal rule and its deliberate
deviation from MATSim. Because a speed is only ever reported for an interval that also holds its
link entry, volume, coverage, group and speed tables are written from the one `interval_starts`
list, so a row of one table always has a row in the others.

Reports distinguish three states:

- **complete** -- the required `link_coverage` module finished; `manifest.json` says `complete` and
  `analysis/index.html` presents the completed report.
- **failed** -- a required module failed. Diagnostics go to `<output_dir>/analysis-failure`
  (`manifest.json`, `module_status.json`, `failure.txt`, and its own `index.html`) and the completed
  report in `analysis/` is left untouched, so a failed attempt never overwrites working output with
  something that looks complete.
- **unavailable** -- an optional module has no implementation or no configured input. These are
  listed in `module_status.json` and do not fail the report. Each entry states whether it is
  `required`, which is what makes the first two states machine-readable.

An optional module that this build computes carries no unavailability reason and therefore inherits
the run's outcome, so a failed run cannot report `link_speed` or `agent_travel` as complete.

All report directories are staged in a sibling `.analysis-*-staging` directory and swapped into
place through `.analysis-*-backup`. A staging directory left by an interrupted run is discarded,
and `reclaim_backup` restores a backup whose published counterpart is missing. That reclaim runs
before a rerun inspects anything else, so an interrupted publish cannot strand the last good
report even when the rerun then fails.

`reanalyze_completed_run` regenerates a report from a completed run's saved outputs. It reads the
recorded final iteration, partitions, event format, seed and interval from
`<output_dir>/analysis/manifest.json`, the expected-travel and vehicle/PCE metadata plus the output
network name from `analysis/run_metadata.json`, and the run's `output_ids.binpb` when present, then
calls the same `analyze_final_iteration` interface. Standalone and automatic reports therefore
agree for the same settings. Because the replay parameters come from the recorded report rather
than a config file, a rerun does not depend on the run's original inputs still being available.
`interval_seconds` can override the recorded width so analysis settings change without rerunning
QSim; only the analysis outputs are rewritten, while event files, plans, the output network and the
ID store are read but left untouched.

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

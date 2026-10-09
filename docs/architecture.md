# Architecture Overview

This document provides an overview of the architecture of the project, detailing its main components, their
interactions, and the overall design principles.

## `matsim-rust`

The core implementation of the Rust QSim is oriented towards [MATSim Java](https://github.com/matsim-org/matsim-libs).
In particular, we tried to minimize the differences between the physics of both simulations, including link dynamics
and output of events.

### Traffic signals

`qsim.signals` optionally names the three MATSim signal files -- `systems`, `groups` and
`control`. All three are required; a partial set is rejected as a configuration error
rather than silently running without signals. The files are read once during scenario
load, after the network, because they reference approach links by external id.

The three files reduce to a per-approach-link list of green windows inside a repeating
cycle, held in `ScenarioCore::signals` and shared by every partition. Each partition
narrows that plan to the links it owns, because signal state depends on the link and the
time alone. A vehicle on an approach link may enter its next link while the current
second is inside one of the link's green windows, and always may when the link carries
no signal. A link governed by several groups is green as soon as any of them is.

A red signal holds the vehicle in `evaluate_front_vehicle` and, unlike a full out-link,
suspends the link's stuck timer. Without that distinction a signal could only ever hold
a vehicle for `stuck_threshold` seconds regardless of how long its red runs, because
the engine would otherwise declare the waiting vehicle stuck and force it through.
Genuine blockage is unaffected: a vehicle that is red *and* blocked still accrues stuck
time and still escapes.

Two deviations from MATSim Java are deliberate. MATSim resolves a turn against the
signals of the node and compares the vehicle's candidate next links against them, which
models conflicting turns at a junction; this port keys on the approach link, because
that is what the input format names. Opposing turns off one approach therefore share a
state, and turns from different approaches never contend, so a vehicle faces no
turn-acceptance conflict. See `rust_qsim/src/simulation/network/signals.rs`.

### General Scenario Handling

Starting the simulation mostly works as in MATSim Java. All XML input files need to be converted into protobuf for
faster reading. These files need to be referenced in a configuration file. Based on the config, a scenario is built,
based on that the controller -- pretty much like in MATSim Java.

Network links must list their allowed modes explicitly: a link without modes allows no mode. Unlike MATSim, which
assumes `car` for links without a `modes` attribute, nothing is implied. Loading a network with such links logs a
warning.

Scenario ownership is split into three lifecycles. `Scenario` owns the input data while files are read.
The controller turns it into `ControllerScenario`, which keeps immutable data in a shared `ScenarioCore`
(`Arc<Network>`, `Arc<Garage>`, `Arc<TransitSchedule>`, `Arc<ActivityFacilities>`, `Arc<Config>`) and owns the
mutable `Population`.

Input data is converted into internal types without deriving missing values. Fields that can be derived stay `Option`
after the conversion and are resolved during preparation; code running afterward uses accessors such as
`InternalActivity::link_id` instead of unwrapping. Preparation is split into two steps in `scenario::prepare`:

- `prepare_for_sim` runs once on the loaded `Scenario`, before the controller shares it, e.g. with the routing modules.
  It connects the facilities to the network and resolves the activity locations. Both run in parallel.
- `prepare_for_mobsim` runs before every mobsim iteration. It validates and repairs plans, e.g. by routing trips.

For mobsim, the controller splits the population into `MobsimInput`s. Each input contains a `MobsimPartition` with the
shared scenario data and a fresh partition network runtime, plus a `PopulationShard`. Persistent QSim workers receive
these inputs per iteration and return agents, which the controller materializes back into the next full population.

Transit vehicles are simulated when `transit.simulate_vehicles` is set, and teleported otherwise. Turning a `Scenario` into
a `ControllerScenario` then expands the schedule into per-departure vehicle runs once, before the Mobsim threads start, and
every partition shares them. Routes whose service mode is listed in
`transit.deterministic_service_modes` run stop-to-stop at timetable offsets; other services use the queue network engine.
Timetable vehicles start on the partition of their first route link. Their scheduled link transitions hand the vehicle,
driver, and riders to the next link's owner through the normal vehicle migration message. This keeps boarding queues on
the partition that owns each stop link, and the worker that handles an alighting stop resumes the passenger there. Queue
vehicles use the network engine and occupy links and compete for capacity like every other vehicle; timetable vehicles
follow their scheduled stop times and do not consume network link capacity.

Each worker owns a thread-local travel-time collector shared between its event buses without a cross-thread lock.
The collector associates vehicles with the network mode of their current leg and records link observations separately
per mode. After a worker's Mobsim run (including agent draining), it emits `BeforeCleanup` before sending its normal
worker result. A thread-local completion listener consolidates the collector's observed links and submits them to the
shared travel-time calculator. The last submission atomically publishes the complete, immutable snapshot. The controller
only waits for worker results, so publication has finished before `AfterMobsim`.
The shared router reads the snapshot without taking the submission lock; unobserved links use freespeed.
`prepare_for_mobsim` uses the previous iteration's snapshot, or an empty snapshot for the first iteration. The workers'
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
Scoring uses the public `PlanScorer` trait and reads only the experienced plan. The controller builder accepts a
`Box<dyn PlanScorer>`; without one, it creates `CharyparNagelScoringFunction`. The alternative
`OnlyTravelTimeDependentScoring` assigns the negative elapsed seconds of completed trips, including transfer waits.
An empty experienced plan receives zero points from either built-in scorer.

### Final-Iteration Analysis

`simulation::analysis` owns the final-iteration report. After a successful run, the controller calls
`analyze_final_iteration` once, which replays the final iteration's event partitions in
chronological order and publishes per-interval link-volume, link-speed, link-classification and
agent-travel tables plus an offline visual HTML report with separate SVG assets under `<output_dir>/analysis`. The interval
width comes from
`output.analysis.interval_seconds` (3600 by default), so the tables are hourly unless configured
otherwise. Inputs that cannot be recovered from the event files -- the final-iteration
expected-travel snapshot, the vehicle/PCE catalog and the simulated sample fraction -- are moved
into a compact `AnalysisRunMetadata` rather than by borrowing or copying the scenario; the run has
finished by then, so no population-scale clone happens. The report needs a positive
`qsim.sample_size`, because observed volumes are scaled up by its reciprocal to describe the
unsampled population.

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

All report directories are staged in a sibling `.analysis-*-staging` directory and swapped into
place through `.analysis-*-backup`. A staging directory left by an interrupted run is discarded,
and `reclaim_backup` restores a backup whose published counterpart is missing. That reclaim runs
before a rerun inspects anything else, so an interrupted publish cannot strand the last good
report even when the rerun then fails.

`reanalyze_completed_run` regenerates a report from a completed run's saved outputs. It reads the
recorded final iteration, partitions, event format, seed and interval from
`<output_dir>/analysis/manifest.json`, the expected-travel, vehicle/PCE and sample-size metadata
plus the output network name from `analysis/run_metadata.json`, and the run's `output_ids.binpb`
when present, then calls the same `analyze_final_iteration` interface. Standalone and automatic
reports therefore agree for the same settings. Because the replay parameters come from the recorded
report rather than a config file, a rerun does not depend on the run's original inputs still being
available. `interval_seconds` can override the recorded width so analysis settings change without
rerunning QSim; only the analysis outputs are rewritten, while event files, plans, the output
network and the ID store are read but left untouched.

Journey analysis groups observed legs between consecutive substantive plan activities. Stage
activities containing `interaction` do not end a journey. The main mode follows the MATSim analysis
mode hierarchy, and journey distance sums the route distances captured from the prepared selected
plan, including model-derived teleportation distances. Journeys with incomplete observed legs keep
their completion and route-distance fields but have no duration.

#### Demographic outcomes and equity

`simulation::analysis::demographic` groups people by the person attributes
`output.analysis.person_group_attributes` names. The group labels, the optional person weight and
the optional monetary cost are read from the population next to the expected-travel capture,
before the final mobsim is the only moment the attributes exist, and travel with
`AnalysisRunMetadata` so a standalone rerun groups the same people.

The module reads `person_daily.csv` -- the agent travel module's table -- for the person outcomes
it aggregates, so the group burdens, the equity comparison and the person table cannot describe
the same day differently. A burden is defined only for a person whose day is `complete` or
`no_travel`; every other person keeps their group size and is counted in `incomplete_persons`
instead of lowering a mean. The equity comparison reads each configured comparison run's
published report and counts winners and losers per group under the stated criterion
`lower_daily_completed_travel_time`, rejecting a comparison run that was not grouped by the same
attributes, and reporting the persons each count excludes. Other modules join the group tables by
writing `<module>_group_outcomes.csv` with the columns `dimension,group,metric,unit,value` into
the report directory.

#### PCE volumes and capacity utilization

`simulation::analysis::capacity` adds per-link capacity utilization. Volumes are weighted by PCE,
which is the unit `LocalLink` charges its flow cap in, and are scaled up by the reciprocal of the
sample size, so the ratio reproduces exactly the utilization the flow cap enforced:

    expanded_pce / (capacity * interval_hours)

Lane counts are exported next to the capacity but never applied to it a second time.
`link_capacity.csv` keeps raw vehicle counts, observed PCE volumes and sample-scaled volumes as
separate columns, and exports `effective_capacity_pce`, the ratio's denominator. Each interval is
credited only with the capacity of the window the simulation covered, so a final interval shorter
than `analysis.interval_seconds` is not treated as a whole one.

A ratio is blank with a stated reason rather than estimated: a non-positive or non-finite capacity
invalidates only the ratio, because the volumes do not involve the capacity, while missing or
unusable PCE invalidates the PCE columns too. `entry_vc_status` and `exit_vc_status` carry the
reason per link. A link that carried no vehicles on a side counts as unused whatever its capacity
says, so an idle network is not hidden behind unavailable ratios; `vc_histogram.csv` keeps the two
apart and bins only links that carried traffic.

PCE totals are accumulated as exact integers at a fixed scale, not as running floating-point sums,
because floating-point addition does not commute. Otherwise the same vehicles crossing a link
simultaneously could produce totals differing in the last bit, and a ratio sitting exactly on a
histogram bin edge would land in different bins depending on the order event partitions were
replayed in.

Every name in `metric_catalog.json` is the column it describes, so a consumer can look a metric up in
the table that exports it.


### External Services

`simulation::analysis` owns the final-iteration report. After a successful run, the controller calls
`analyze_final_iteration` once, which replays the final iteration's event partitions in
chronological order and publishes per-interval link-volume and coverage tables plus an
offline visual HTML report with separate SVG assets under `<output_dir>/analysis`. The interval width comes from
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

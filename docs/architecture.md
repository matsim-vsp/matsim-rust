# Architecture Overview

This document provides an overview of the architecture of the project, detailing its main components, their
interactions, and the overall design principles.

## `matsim-rust`

The core implementation of the Rust QSim is oriented towards [MATSim Java](https://github.com/matsim-org/matsim-libs).
In particular, we tried to minimize the differences between the physics of both simulations, including link dynamics
and output of events.

The rules between agents and engines are stated in [agent-contract.md](agent-contract.md).

### General Scenario Handling

Starting the simulation mostly works as in MATSim Java. Inputs can be loaded from XML or converted into protobuf for
faster reading. Input files are referenced in a configuration file. Based on the config, a scenario is built,
based on that the controller -- pretty much like in MATSim Java.

Network links must list their allowed modes explicitly: a link without modes allows no mode. Unlike MATSim, which
assumes `car` for links without a `modes` attribute, nothing is implied. Loading a network with such links logs a
warning.

With `PartitionMethod::Metis`, the nodes of the network are partitioned with METIS, and every link belongs to the
partition of its `to` node. METIS partitions undirected graphs and requires them to be symmetric, without loops and
without duplicate edges. It does not check this, and invalid input makes it access memory out of bounds. Therefore,
each pair of nodes connected by at least one link becomes one edge. Its weight is the sum of the weights of all links
between both nodes in either direction (each truncated to an integer), but at least 1. Loop links are ignored.
`partitioning.non_main_mode_links` controls links that allow none of the `qsim.main_modes`, e.g. the links of a transit
network whose vehicles are teleported. No vehicle moves on them, but agents perform activities on them, e.g.
`pt interaction`. With the default `AssignAfterPartitioningToNearest`, METIS only partitions the nodes incident to a
link that allows a main mode, and only these links become edges and contribute to vertex weights. Afterwards, every
other node is assigned to the partition of the nearest partitioned node in the x/y plane; ties are broken by the
smallest external node id. Only if no link allows a main mode, e.g. because `qsim.main_modes` is empty, all nodes and
links are partitioned. With `Include`, all nodes and links are partitioned regardless of their modes, as before
October 2026. `Network::from_file_with_partitioning` always partitions all nodes and links. `contiguous: true` requires a connected
graph, i.e. a connected main-mode network with `AssignAfterPartitioningToNearest`. Networks with separate components,
e.g. transit stop nodes with only a loop link or a transit network that is not connected to the road network, need
`contiguous: false` with `Include`. `imbalance_factor` is METIS's load imbalance, the largest allowed partition
weight relative to the mean (1.03 = 3 %); before October 2026 it was the excess itself, so that 1.1 allowed partitions
2.1 times as heavy as the mean. `min_conn: true` sets METIS's `MinConn` option, which tries to minimize the
maximum number of neighbors of a partition (k-way only; it moves vertices at weak interfaces, i.e. interfaces with a
small edge weight, and stops once the maximum is below 1.4 times the mean). Before October 2026, the graph was built from the
directed links, with every incoming link turned into a loop. Network files partitioned with that version were computed
from an invalid graph and should be partitioned again.

METIS draws random numbers from a fixed seed. The workspace takes `metis-sys` from a fork (see `[patch.crates-io]` in
`Cargo.toml`), which builds the bundled METIS with GKlib's own random number generator (`USE_GKRAND`) instead of the C
library's `rand()`, and without floating-point contraction (`-ffp-contract=off`). Hence, METIS yields the same partitions
on every platform. Changing the fork, the seed or the METIS version changes the partitions. The state of the random
number generator is shared by the whole process, so all calls to METIS are serialized.

In every step, a partition waits for messages from its neighbors, i.e. the partitions it shares a link with that can
carry vehicles. Links that allow modes, but none of the `qsim.main_modes`, e.g. the links of a transit network whose
vehicles are teleported, do not make partitions neighbors, unless a partition has no other links to other partitions
(`SimNetworkPartition::find_neighbors`). Since every partition exchanges a message with each neighbor in every step,
the partition with the most neighbors tends to set the pace of all. Messages
from other partitions, e.g. teleported agents whose destination link lies in a partition that is not a neighbor, are
only read if they arrive while the partition is waiting, and never if the partition has no neighbors at all. This
happens if the network consists of components that are not connected by links and a component forms a partition of
its own. After each mobsim run, all workers wait for each other once more and return the messages they never
processed. If there are any, the controller panics, because the agents in them are lost.

Populations are read in parallel, both from XML and from protobuf. A reader thread splits the input into persons,
which are parsed and converted by the rayon thread pool. The id store must stay independent of the number of threads:
for protobuf, all ids except missing subpopulations are only looked up, and these are created in file order afterwards.
For XML, all ids are created sequentially before the persons are converted, in the order in which converting the
persons sorted by id one after another would create them (see `for_each_id_of_io_person`). Internal ids are the
creation index of an id and never depend on hashing. Data is not laid out in vectors whose positions are internal ids,
since their length and gaps would depend on the id store instead of on the data. It is kept by id in an `IntMap`, or a
container hands out typed indices of its own, like the routes and departures of the transit schedule.

Scenario ownership is split into three lifecycles. `Scenario` owns the input data while files are read.
The controller turns it into `ControllerScenario`, which keeps immutable data in a shared `ScenarioCore`
(`Arc<Network>`, `Arc<Garage>`, `Arc<TransitSchedule>`, `Arc<ActivityFacilities>`, `Arc<Config>`) and owns the mutable
`Population`. Next to the core, it keeps the `Arc<TransitSimSchedule>`, which only the mobsim partitions receive.

Input data is converted into internal types without deriving missing values. Fields that can be derived stay `Option`
after the conversion and are resolved during preparation; code running afterward uses accessors such as
`InternalActivity::link_id` instead of unwrapping. Preparation is split into two steps in `scenario::prepare`:

- `prepare_for_sim` runs once on the loaded `Scenario`, before the controller shares it, e.g. with the routing modules.
  It prepares routing interaction ids and person vehicles sequentially, connects the facilities to the network and
  resolves activity locations.
  The latter two steps run in parallel. Vehicle preparation sorts persons by external id and the union of
  `qsim.main_modes` and `routing.network_modes` by mode name, so generated ids do not depend on map or thread order.
- `prepare_for_mobsim` runs before every mobsim iteration. It validates and repairs plans, e.g. by routing trips.
  Missing vehicle references on network routes are resolved from the person's `vehicles` mapping before read-only
  validation; explicit references take precedence and must resolve. Newly routed trips use the person's assigned
  vehicle for their routing mode, rather than reusing the old trip's route vehicle.

`qsim.vehicles_source` follows MATSim's `defaultVehicle`, `modeVehicleTypesFromVehiclesData`, and `fromVehiclesData`.
Rust defaults to the second option and always generates `<person>_<mode>` ids; it has no deprecated option to use the
person id alone for cars. For the first two options, every configured main or network routing mode gets a generated
assignment, replacing that mode's input `vehicles` entry. The person's `vehicleTypes` can override the creation type.
Types are validated before checking for existing vehicles, as in Java; existing vehicles are never replaced.
New vehicles resolve their speed and PCE from the garage by the fallback type id. Unlike Java's fresh default type
object, an already registered `defaultVehicleType` supplies its own values when `defaultVehicle` is used.
`fromVehiclesData` preserves the supplied mappings and catalog and reports missing vehicles or assignments at use.
Each person's new assignments are collected locally and merged only after all its modes succeed. A missing
`vehicles` attribute becomes an empty map even with `fromVehiclesData` or no configured network modes, as in Java.
On an error, that person's assignments remain unchanged; previously created vehicles and completed persons are not
rolled back. Interaction ids are prepared separately before this step, also with `fromVehiclesData`.
Pure teleportation modes do not need vehicles. The catalog remains immutable after this preparation.

Population loaders and the config-free XML-to-protobuf converter do not generate vehicles or fill missing route
references. `vehicles` and `vehicleTypes` are string-map attributes, represented as JSON objects internally, with
their Java class names restored on XML output. Protobuf adds the string-map oneof field 5 without changing scalar
field tags. Older files remain readable, but data already lost by an older converter requires reconversion with the
matching id store. Preparation can change internal ids relative to previous releases; RNG stream encoding is unchanged.

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
future scoring events. The engine subscribes only to the event types that change a partial plan, so that other events,
e.g. link leave events, don't reach it. When a network leg ends, its route gets the distance of MATSim's `EventsToLegs`,
`RouteUtils.calcDistance` with the relative positions of the vehicle's enters/leaves traffic events (`calc_distance`).
Vehicles enter and leave traffic at the end of a link, so this is the length of the links after the departure link up
to and including the arrival link, and 0 if both are the same link.
At `BeforeCleanup`, every worker converts the backpacks currently on that
partition into one partial population and sends it to the controller over a dedicated backchannel before publishing its
normal worker result. Consequently, all partial populations are available when the controller emits `AfterMobsim`. The scoring
module verifies iteration and rank and merges the populations deterministically by person ID. The controller then
scores each reconstructed experienced plan and passes the result to the score assigner, a port of MATSim's
`NewScoreAssignerImpl` with an implicit learning rate of 1. It updates exactly the selected original plan: the new
score replaces the old one until the iteration given by `scoring.fraction_of_iterations_to_start_score_msa`, and from
then on the plan's score is the method-of-successive-averages (MSA) mean of the scores it received since that
iteration. A plan without a score takes the new score without counting it as an MSA contribution. The MSA counter is
in-memory state of each plan. A plan copied during replanning restarts the counter at zero, so its first MSA
contribution replaces any copied score, and a restart from output plans resets all counters, as in MATSim. Without the
setting, MSA never starts. Experienced plans receive the raw score and are written only when
`scoring.write_experienced_plans` and the configured plan writing interval allow it. Collection and scoring always
run, even when experienced-plan output is disabled, so replanning can consume the updated selected-plan scores. The
only exception is `scoring.mode: Disabled`: the controller then registers no backpacking engines, collects no
experienced plans, and sets the score of each selected plan to `None`. Backpacks do not return to an initial or "home"
partition.
Scoring uses the public `PlanScorer` trait and reads only the experienced plan. The controller builder accepts a
`Box<dyn PlanScorer>`; without one, it creates `CharyparNagelScoringFunction`. It scores activity durations with
MATSim's default `typicalDurationScoreComputation = relative`, i.e. a zero-utility duration of
`typicalDuration * exp(-1)`, so an activity of typical duration scores `performing * typicalDuration`. MATSim's
deprecated `uniform` computation is not supported. Like MATSim's `CharyparNagelActivityScoring`, an activity is only
performed between its `opening_time_s` and `closing_time_s`, which may lie beyond 24 hours; the time before and after
counts as waiting with `waiting`. `latestStartTime`, `earliestEndTime` and `minimalDuration` are not supported.
`convert_scoring_parameters` converts the scoring module of a MATSim config, typically an `output_config.xml`, into a
scoring parameters file and rejects such unsupported values instead of dropping them, as well as unknown parameter
sets and module settings that this simulation does not implement: `usingOldScoringBelowZeroUtilityDuration=true`,
`learningRate` or `brainExpBeta` other than 1, and a `typicalDurationSource` other than `activityParams`. Only
`tasteVariations` is skipped with a warning. Missing values take MATSim's
defaults, including `scoringThisActivityAtAll=false` for types ending with `interaction`, so a reduced output config
gives the same file. It derives `aborted_plan_score`
like MATSim's `ScoringParameters`. Like MATSim's `CharyparNagelLegScoring`, the scoring function scores the time until
boarding of a leg with a transit route with `waiting_pt` instead of the marginal utility of traveling, by default with
the one of traveling by `pt`, and adds `utility_of_line_switch` for each leg of a `transit.passenger_modes` mode after
the first one in a trip. The alternative
`OnlyTravelTimeDependentScoring` assigns the negative elapsed seconds of completed trips, including transfer waits.
An empty experienced plan receives zero points from either built-in scorer.

Like MATSim's `scoringParameters`, the scoring parameters come in sets per subpopulation. Each `ScoringParameterSet`
holds the person parameters such as `performing` together with its `activity_params` and `mode_params`. The set without
`subpopulation` is the default set. A person whose subpopulation has no set of its own uses it, like with MATSim's
`getScoringParametersOrDefault`, and activity and mode parameters are not merged with those of the default set. A person
without a subpopulation attribute belongs to `person`. The sets are only read from a file:
`scoring.scoring_parameters_path` names a YAML file with a list of sets, relative to the config, and without it there is
one default set. Like the network and the population, `Scenario::load` reads the sets into
`ScenarioCore::scoring_parameters`, from where the scoring function and the router take them, and rejects several sets
for one subpopulation as well as activity types or modes that occur twice in a set. The controller always writes the
sets to `output_scoring_parameters.yml`, while the output config keeps the input path, as it does for the network.
Deviating from MATSim's
`RandomizingTimeDistanceTravelDisutility`, which uses the default set for everyone, `ScoringBasedTravelTimeAndDisutility`
routes with the person's set; routing without a person and the ALT lower bound use the smallest cost factors of all sets.
Its travel times are those observed in the mobsim for its own mode. `ControllerBuilder::travel_time_mode(mode, other)`
lets the router of a network mode use the travel times observed for another mode instead, like MATSim's
`addTravelTimeBinding`, e.g. ride (teleported along its network route) those of car; the cost factors stay those of
`mode`.

### Link Dynamics

`qsim.link_dynamics` selects `FIFO` (the default, also for legacy `Simulation` configs) or
`PassingQ`, independently of `traffic_dynamics`. The CLI accepts `--set qsim.link_dynamics=PassingQ`.
FIFO uses a `VecDeque`; PassingQ uses the `TimeQueue` from `simulation::queue`, both through the
`Queue` trait and an internal enum wrapper whose delegation is generated by `enum_dispatch`. PassingQ orders vehicles
by earliest exit time and preserves insertion order for equal times. Unlike Java's `PassingVehicleQ`, equal exit times thus
have a guaranteed FIFO order. The buffer and waiting list remain FIFO in both modes.

`qsim.traffic_dynamics` selects `Queue` (the default, including legacy `Simulation` configs) or
`KinematicWaves`. The CLI override accepts `--set qsim.traffic_dynamics=KinematicWaves`; the Rust API
uses `config::TrafficDynamics`. Waiting-list departures retain their existing buffer/flow-capacity
semantics in both modes.

A link consists of a queue, a buffer and a waiting list for departing vehicles. A vehicle may leave the queue after its
free-speed travel time, but not before the next tick. Vehicles that end their leg leave the queue without using flow
capacity; all others move into the buffer if the link has flow capacity left. Departing vehicles move from the waiting
list into the buffer before vehicles from the queue. The node at the end of the link takes vehicles from the buffer in
FIFO order if the next link's admission capacities allow it or the front vehicle is stuck.

As in MATSim's QSim (`QueueWithBuffer` with the fast capacity update), a vehicle consumes flow capacity when it enters the
buffer (see `Flowcap`). It may enter as long as the accumulated capacity is positive, so the capacity can become
negative. The capacity grows with the flow capacity per second, but at most to one tick of capacity minus the PCE of the
vehicles that are still in the buffer. Thus, a vehicle that is blocked in the buffer holds the capacity it has
consumed, and the headway of the next vehicle counts from the moment the blocked vehicle leaves. Every vehicle in the
buffer may leave it; nodes do not check the flow capacity.

If a headway is a whole number of ticks, which is common for sampled scenarios (e.g. 800 veh/h at 0.1 % gives one
vehicle per 4500 s), the exact capacity reaches exactly 0 in some tick. Floating-point rounding leaves a tiny residue
instead, whose sign depends on whether the capacity was accumulated tick by tick or at once after the link was inactive,
which in turn depends on the partitioning. Therefore, "positive" means larger than a margin of `1e-10`, as in DSim. This
reproduces the result of QSim's rule with exact arithmetic.

Until October 2026, the capacity was consumed when a vehicle left the buffer. A vehicle could enter the buffer if the
remaining capacity exceeded the PCE of the vehicles in the buffer, and the node only took it if the remaining capacity
exceeded `1e-10`. Both formulations are equivalent, but floating-point rounding can change single decisions.

A link whose waiting list and buffer are empty, whose flow capacity is full and whose first vehicle is not due yet
sleeps until that vehicle is due (`LocalLink::fall_asleep`): moving it would change nothing, because a full capacity
stays exactly full. A departure wakes it up earlier; a vehicle entering a PassingQ also wakes it if its exit time is
earlier than the scheduled wake-up. Other queue insertions leave it asleep. Because the set of active
links therefore changes more often, the vehicles that end their leg are sorted by link id before they are handed to
the activity engine, so that the order of the agents does not depend on the order of the active links. The order of
events within a time step still depends on it. It is deterministic for a single partition; with several partitions,
the order in which messages from other partitions arrive already varies between runs.

The Queue link dynamics deviate from QSim in the following points:

- QSim accumulates capacity only when it checks whether a vehicle may enter the buffer, and resets its update time to
  the previous time step whenever a vehicle leaves the buffer; capacity not accumulated until then is lost. The Rust
  link accumulates capacity in every tick in which it holds a vehicle.
- QSim admits a vehicle into the buffer if the accumulated capacity is larger than 0, Rust if it is larger than `1e-10`.
- There is no `pcuThresholdForFlowCapacityEasing` and no flow efficiency. A vehicle consumes exactly its PCE.
- A vehicle stays in the queue for at least one tick, so that a link behaves the same whether or not it is split
  between partitions (see `LegEngine::do_step`).

### Schedule-Based Transit

By default, transit legs are teleported with the travel time of their route. With
`transit.simulation: ScheduleBasedTeleportation`, transit vehicles jump from stop to stop according to the schedule
instead, without simulation on the network, and passengers wait at stops, board and alight, as in SBB's deterministic transit
engine for MATSim (`SBBTransitQSimEngine`). The transit vehicles are read from `transit.vehicles_path`. The legs of
`transit.passenger_modes` (default `pt`) ride them; these modes must not be main modes.

`TransitSchedule` holds all data of the schedule files, independent of the simulation: routes and departures in dense
vectors, routes sorted by the external ids of line and route, the routes of a line and the departures of a route
contiguous, and lines and stop facilities by their ids in maps without a particular order. XML and protobuf are read
element by element in the order of the file, so the ids are created as before, and only then laid out. The schedule is
loaded before the population, and `ScenarioCore` keeps it also with teleported transit.

`TransitSimSchedule` is what the mobsim adds to it, like `SimNetworkPartition` to `Network`: the driver and the vehicle
of each departure and the partition of each stop. `From<Scenario> for ControllerScenario` builds it once, after the
population is loaded, so that the ids of persons do not depend on transit. `ControllerScenario` keeps it next to the core
and hands it to each `MobsimScenarioPartition`, so replanning and plan preparation only see the `TransitSchedule`. The
transit engine processes stops in the order of their ids. Drivers have the external ids
`pt_<line>_<route>_<departure>` as in SBB's engine, and their ids are created in the order of these external ids.
Building it panics if the schedule cannot be simulated, e.g. if a stop has no link or, with
`ScheduleBasedTeleportation`, a vehicle serves several departures. Plan preparation requires no travel times for transit
legs, but a transit route that the schedule serves.

`ScheduleBasedTeleportation` cannot simulate vehicle blocks (`Umlauf` in MATSim), in which one vehicle serves several
departures one after the other. So each departure has a vehicle of its own, and the id of a vehicle in the events
identifies one departure. `prepare_for_sim` replaces a vehicle that serves several departures by a copy for each of
them, with the same type and the id `<vehicle>_<line>_<route>_<departure>`; vehicles that serve one departure keep their
id. `TransitSimulation::needs_vehicle_per_departure` decides this, so that a later simulation with vehicle blocks, e.g.
on the links of the network, can let vehicles serve several departures. The controller writes the schedule and the
transit vehicles with the copies to `output_transitSchedule` and `output_transitVehicles`, so that the output matches
the events and can be used as input again. With teleported transit, it writes both unchanged if a schedule was read.

Each departure has a driver, an agent without a person. Like the drivers of MATSim's transit engines, it passes through
three phases: it waits in a `pt interaction` activity at the start link of its route until the departure time, drives
the departure as its leg and then stays in an activity without end until the mobsim drops it. Its activities emit no
events. `TransitDriverAgentSource` creates the drivers of the departures whose first stop is on a partition. A mobsim
asks a list of agent sources one after the other, by default `PopulationAgentSource`; each source takes the persons of
the population for which it creates agents. With schedule-based transit, the controller adds `TransitDriverAgentSource`.
A driver that is still waiting when the mobsim ends becomes stuck, as in MATSim's activity engine.

The `TransitEngine` is part of the `LegEngine` and runs after the network moves and before the messages are exchanged.
The `LegEngine` passes it every agent that offers the role of a driver (`TransitDriverLogic`), whatever its mode, and
the legs of passenger modes that offer the role of a passenger (`PassengerLogic`). When a driver's leg starts, the
engine creates a `TransitVehicle` for it. Each driver serves one departure; drivers that serve the departures of a
vehicle block one after the other could come with a simulation on the network. When its activity ends, the driver copies
its departure from the schedule into a plan of its own and no longer depends on the schedule. It knows its stops and
tells the engine when the vehicle arrives and departs: at the departure time plus the arrival offset plus the previous
stop's nonnegative departure delay, but not before the earliest arrival the engine allows. It departs at the departure
time plus the departure offset, but not before the arrival, so a shorter halt can absorb arrival delays. At
the last stop, the driver leaves the vehicle and ends its leg; drivers that are still in a vehicle when the mobsim ends
are dropped, as in SBB's engine.

Passengers are agents with the role `PassengerLogic`. The engine asks a riding passenger at each stop whether it
alights, and a waiting passenger about each vehicle at its stop whether it boards. Plan-based agents follow MATSim's
`checkLineAndStop`: they board any vehicle of the planned line that still serves their egress stop and alight there. The
engine only works when something is due; arrivals and departures come from queues. A passenger waits at its stop as soon
as its leg starts. A step has three parts: vehicles arrive and passengers alight; at each stop where a vehicle arrived
or passengers started to wait, the waiting passengers board the halting vehicles; vehicles whose departure time has come
leave. A vehicle that departs in a tick thus takes passengers who start to wait in this tick. Their walk to the stop
ended in an earlier tick, so QSim would have them at the stop as well (see [agent-contract.md](agent-contract.md)). A
passenger who comes while a vehicle halts boards right away. Alighting and boarding take no time yet; once they do, a
vehicle leaves when nobody wants to alight or board anymore and its departure time has come. Simultaneous vehicles are
handled in the order of their drivers' ids, and waiting passengers board in the order of their arrival tick and id.

If the next stop is on another partition, the vehicle, its driver, its passengers and their attachments are sent there.
That partition may not be a neighbor, because links allowing no main mode, such as most transit links, do not make
partitions neighbors. Like teleported agents, such vehicles are only received while the partition waits for its
neighbors, and the engine panics if one arrives after it was due. Enable `computational_setup.global_sync` if this
happens.

Backpacking builds the experienced plans of transit legs like MATSim's `EventsToLegs`. It keeps the transit vehicles
that are active on its partition: `TransitDriverStarts` gives the line and the route, and `VehicleArrivesAtFacility` the
stop. Since each departure has a vehicle of its own, the id of the vehicle in an event identifies it. A passenger who
enters a vehicle gets a PT route with the vehicle's stop as access stop and the time of the event as boarding time; when
the passenger leaves the vehicle, its stop is the egress stop. The distance is that of `RouteUtils.calcDistance`: the
lengths of the links of the route's network route after the access link up to and including the egress link (`calc_transit_distance`). A
partition that receives a vehicle takes the line and the route from its driver. Drivers
have no person and thus no backpack, so backpacking ignores their events. It knows the drivers of the active vehicles;
other drivers, e.g. those who still wait for their departure when the mobsim ends and get stuck, are rare and searched
in the schedule.

Deviations from SBB's engine:

- A late departure shifts the next arrival by the departure delay, preserving the scheduled travel time between
  stops. SBB can shorten that travel time to catch up with the schedule. Halts can still absorb arrival delays.
- A vehicle needs at least one tick from one stop to the next, so that the result does not depend on whether the next
  stop is on another partition. SBB allows zero seconds.
- Boarding and alighting take no time. With MATSim's default `ComplexTransitStopHandler`, SBB boards and alights one
  second after the arrival and departs up to three seconds later, even with zero access and egress times.
- Vehicles have no capacity, `delay` is always 0, and there are no chained departures and no link events.
- SBB keeps the ids of vehicles that serve several departures. matsim-rust gives each departure a copy of its own.
- Experienced transit legs have no attributes `enterVehicleTime` and `vehicleId`, like the other experienced legs of
  matsim-rust.
- Passengers who are still in a vehicle at the end of the mobsim report the link of their access stop in the stuck
  event, SBB the link of the next stop.

Apart from these deviations and the tick per leg, the events of `pt_tutorial` match those of SBB's engine, and so do
the experienced plan and its score (`tests/simulation/pt.rs`).

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

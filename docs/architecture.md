# Architecture Overview

This document provides an overview of the architecture of the project, detailing its main components, their
interactions, and the overall design principles.

## `matsim-rust`

The core implementation of the Rust QSim is oriented towards [MATSim Java](https://github.com/matsim-org/matsim-libs).
In particular, we tried to minimize the differences between the physics of both simulations, including link dynamics
and output of events.

### General Scenario Handling

Starting the simulation mostly works as in MATSim Java. All XML input files need to be converted into protobuf for
faster reading. These files need to be referenced in a configuration file. Based on the config, a scenario is built,
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
`contiguous: false` with `Include`. `min_conn: true` sets METIS's `MinConn` option, which tries to minimize the
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
creation index of an id and never depend on hashing.

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
  As in MATSim, trips with a mode of `qsim.main_modes` or `routing.network_modes` are routed with the vehicle
  `<person>_<mode>`; vehicles referenced by the routes of the old trip are not reused.

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
e.g. link leave events, don't reach it. At `BeforeCleanup`, every worker converts the backpacks currently on that
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
`Box<dyn PlanScorer>`; without one, it creates `CharyparNagelScoringFunction`. The alternative
`OnlyTravelTimeDependentScoring` assigns the negative elapsed seconds of completed trips, including transfer waits.
An empty experienced plan receives zero points from either built-in scorer.

### Link Dynamics

A link consists of a queue, a buffer and a waiting list for departing vehicles. A vehicle may leave the queue after its
free-speed travel time, but not before the next tick. Vehicles that end their leg leave the queue without using flow
capacity; all others move into the buffer if the link has flow capacity left. Departing vehicles move from the waiting
list into the buffer before vehicles from the queue. The node at the end of the link takes vehicles from the buffer in
FIFO order if the next link has storage capacity left or the front vehicle is stuck.

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
stays exactly full. A departure wakes it up earlier; a vehicle entering the queue does not. Because the set of active
links therefore changes more often, the vehicles that end their leg are sorted by link id before they are handed to
the activity engine, so that the order of the agents does not depend on the order of the active links. The order of
events within a time step still depends on it. It is deterministic for a single partition; with several partitions,
the order in which messages from other partitions arrive already varies between runs.

The link dynamics deviate from QSim in the following points:

- QSim accumulates capacity only when it checks whether a vehicle may enter the buffer, and resets its update time to
  the previous time step whenever a vehicle leaves the buffer; capacity not accumulated until then is lost. The Rust
  link accumulates capacity in every tick in which it holds a vehicle.
- QSim admits a vehicle into the buffer if the accumulated capacity is larger than 0, Rust if it is larger than `1e-10`.
- There is no `pcuThresholdForFlowCapacityEasing` and no flow efficiency. A vehicle consumes exactly its PCE.
- A vehicle stays in the queue for at least one tick, so that a link behaves the same whether or not it is split
  between partitions (see `LegEngine::do_step`).

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

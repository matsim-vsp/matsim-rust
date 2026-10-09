# Java/Rust differential fixtures (PT)

This is the reference harness for the SwissRailRaptor and transit-execution port
([#69](https://github.com/titipakorn-th/matsim-rust/issues/69)). It pins MATSim, records what the
pinned reference does on a small set of shared scenarios, and compares the Rust port against that
recording at the two boundaries where behavior is observable: `TripRouter` for itineraries, and the
simulation integration runner for execution events.

It is deliberately a *harness*, not a compatibility claim. A differential test can only detect a
difference that a fixture exercises, and the fixture corpus is still small and focused. Every
divergence it reports is a fact about those scenarios, not a measure of overall parity.
The external routing service boundary is covered through the same trip router. The one-to-all skim
uses one per-origin routing tree; its reachable and unreachable stop results, including a missed
departure boundary, are compared with MATSim's `calcTreesObservable`. The skim's explicit `pt`,
`walk`, and `no_path` outcomes and external result classifications are tested separately.

## The pinned reference

| | |
|---|---|
| Repository | <https://github.com/matsim-org/matsim> |
| Tag | `2026.0` |
| Commit | `c7a75ebeddc3ceb62959af046190064bf23770df` |
| License | GPL-2.0-or-later (see [Redistribution](#redistribution)) |

The reference is not published to Maven Central, so `java_reference/run_reference.sh` checks the
commit out, verifies the SHA, and builds it into a cache-local Maven repository before running the
harness. It refuses to run against a checkout at any other commit, so a recorded reference always
names the source it came from.

Requirements: `git`, Maven 3.9+, and a **JDK 25**. MATSim 2026.0 sets
`maven.compiler.release=25`; the launcher checks the major version and stops rather than producing a
confusing compiler error.

```shell
JAVA_HOME=/path/to/jdk25 ./java_reference/run_reference.sh              # every fixture
JAVA_HOME=/path/to/jdk25 ./java_reference/run_reference.sh supplied_plan  # one fixture
```

The first run downloads the reference, its dependencies and the JDK-independent build. Later runs
reuse the cache at `$REFERENCE_CACHE_DIR` (default `~/.cache/matsim-rust/java-reference`).

## Fixtures

A fixture is a directory under `matsim_rust/tests/resources/pt_reference/`:

| File | Role |
|---|---|
| `config.xml` | MATSim's own configuration. Input paths are relative to this file. |
| `requests.json` | Routing requests issued through `TripRouter` and one-to-all tree queries after the run. Optional. |
| `*.yml` | The Rust configuration for the same scenario, where one is needed. |
| shared inputs | Reused from `matsim_rust/assets/` rather than duplicated. |
| `../java/<fixture>.json` | The recorded reference. Regenerate; never hand-edit. |

Each recorded reference carries the configuration digest, the seed, the simulation clock and a
SHA-256 of every input, so a moved or edited input cannot pass unnoticed. The Rust side asserts the
schema version and the reference commit before comparing anything.

### `supplied_plan`

The PT tutorial scenario — network, schedule, plans and vehicle types from
`matsim_rust/assets/pt_tutorial/` — executed with the supplied plan. The Rust side runs the existing
`pt_tutorial_config.yml`, so the fixture adds no Rust input at all.

It claims only that Rust *executes* a supplied plan the way the reference does. It says nothing about
routing: the plan is given, so the router is never consulted.

### queue_execution

The same supplied plans, network and transit schedule, with transit vehicles driven on both sides.
Java reads the tutorial's MATSim transit-vehicle file; Rust uses the equivalent vehicle types with
the two scheduled vehicles added. The comparison follows each passenger's event sequence and each
vehicle's stop sequence, so simultaneous events from unrelated vehicles may appear in either order
while boarding, alighting and stop dependencies remain ordered.

The fixture gives two simultaneous passengers one seat. The first boards the 07:50 service and the
other boards the next eligible service; passenger waiting, boarding and alighting events match MATSim
exactly. Cross-stream assertions require vehicle arrival before boarding and departure, and vehicle
arrival before alighting. The comparison runs with one and two network partitions. Vehicle stop events
may be one clock step early in Rust because the link queue and node transition occur in different
phases; schedule delay must move by the same amount. Larger differences fail.
The vehicle type uses MATSim's default serial door mode. The separate `queue_doors_*` fixtures
compare serial and parallel doors while one passenger alights as another boards.

`queue_stranded` ends at 07:45, while both passengers are waiting for service. Rust and MATSim emit
the same waiting and `stuckAndAbort` events for each passenger, also with two network partitions.
The one-seat case covers a passenger missing a service because capacity is full and boarding the next
eligible departure.

### `queue_missed_connection`

A supplied plan rides the 08:00 train to `rb`, walks 151 seconds to `rb_platform`, and requests the
08:05:30 bus. It reaches the second platform at 08:05:56, misses `bus_1`, and boards `bus_2` at
08:15. Passenger boarding, alighting and waiting events match the pinned reference within two
seconds; the outcome is checked with one and two network partitions.

### `queue_road_baseline` and `queue_road_congestion`

Both fixtures run the same supplied passenger plans and road bus. The congestion case adds one car
that shares road link `1213` with the bus; the baseline omits only that car. The pinned Java bus
arrives at the downstream stop 62 seconds later with the car. Rust measures a 64-second delay, within
two seconds of the reference, and checks passenger and vehicle event dependencies with one and two
network partitions. This demonstrates the shared-road effect for the pinned link capacity and
vehicle types, not general traffic-congestion parity.

### `queue_doors_serial` and `queue_doors_parallel`

Two supplied passengers share one run and meet at stop `2a`: one alights while the other boards.
The vehicle has two seats so capacity does not prevent overlapping door operations. The serial
reference completes alighting before boarding; the parallel reference records both in the same
second. Rust is compared against both references with one and two network partitions. Passenger
event order and timing relative to the shared stop are checked, as is dwell duration at each stop.
Absolute arrival times downstream can differ by a few link-phase seconds, so the door comparison
isolates stop dwell from route travel time.

### `routing_direct_vs_transfer`

A request from stop `ra` to stop `rc` at 08:00 where three candidates exist:

| Candidate | Result |
|---|---|
| `a_to_b` 08:00 → 08:10 at `rb` | |
| `b_to_c` 08:15 → 08:25 at `rc` | **Java and Rust choose this** |
| `direct` 08:00 → 08:50 at `rc` | |

The population is empty; the itinerary comes from the recorded request, so the assertion is about the
router alone. The direct service remains in the schedule so this fixture proves that routes compete
under the pinned default costs instead of a direct-service preference.
This slice uses those fixed costs and a 20-transfer search cap; configurable transfer limits and
non-default scoring remain outside its coverage.
The same fixture records `calcTreesObservable` from stop `ra` for the 08:00 departure and for the
window beginning one second later through 08:10. It compares arrivals at `rb` and `rc`; isolated
stop `rd` must remain absent from the transit tree. The Rust skim queries those stops at their exact
coordinates, so the comparison covers the shared transit tree without adding access or egress time.

The two transfer-penalty fixtures below vary only the penalty on this same schedule and request, so
together they show the penalty deciding between the same two itineraries.
This slice uses those fixed costs and a 20-transfer search cap; configurable transfer limits remain
outside its coverage.
The fixture records MATSim's `totalRouteCost` attribute in utility units. The Rust assertion converts
its time-equivalent cost using the pinned PT time weight before comparing the two.

### `routing_mapped_modes`

The same request and schedule with the `b_to_c` route changed to `bus`. Both configs enable
passenger mode mappings (`train` → `rail`, `bus` → `road`); rail and road have different travel
utilities, so the direct train service competes with the faster transfer by mapped passenger-mode
cost. The pinned Java and Rust routers choose the direct service, arrive at 08:50, and expose the
ride as a `rail` leg. This complements the preceding fixture, which checks route selection with
mapping disabled.

### `routing_distinct_platform_transfer`

Two requests transfer between `rb` and `rb_platform`, separate facilities 100 m apart. The 08:00
request selects bus–rail; the 08:50 request selects rail–rail. The 08:00:01 request misses the bus
by one second, and a fourth request confirms that platforms 201 m apart are not connected by the
default walking transfer. The reachable requests compete with direct service under the same Java
route-cost objective. Their itineraries include the transfer walk, its 130 m beeline-adjusted
distance, and the pinned five-second transfer-walk margin. A separate Rust runner config supplies
a person plan so the generated access, transfer and egress legs execute in QSim; the routing test
also round-trips that itinerary through XML and protobuf population files.

### `routing_intermodal_access_egress`

The endpoints are near one stop and no transit ride is useful. `avoid` returns the feeder-only
itinerary, selecting bike over walking by cost. This pins the no-PT policy and the walk/bike
alternative.

### `routing_intermodal_eligibility`

Three requests cover an eligible person at a bike-enabled stop, an ineligible person, and an
eligible person at a stop that disallows bike access. The enabled stop maps the bike feeder to a
different link, so the returned plan also includes MATSim's zero-time walk connectors. The Rust
fixture compares passenger modes, arrival times and the outer `pt` routing mode.

### `routing_intermodal_unavailable_feeder`

The bike mode is eligible, but its capped search radius contains no eligible stop. MATSim and Rust
skip that feeder candidate and return the available walking itinerary. A separate Rust unit test
covers a feeder router that returns `NoPath` for a candidate stop.

### `routing_transfer_penalty_shared_stop`

The same request and schedule as `routing_direct_vs_transfer`, with a transfer penalty of 6 utils
per transfer instead of the pinned default of one. The transfer saves 25 minutes, which at 12
utils per hour is worth 5 utils, so the penalty is enough to make both routers reject it and take
the 08:00 -> 08:50 direct service. `transferPenaltyMaxCost` equals the base cost, so the
per-travel-time-hour part is clipped away and the penalty is exactly 6 utils however long the
journey runs; that pins the clipping boundary. MATSim only reads `transferPenaltyBaseCost` once a
per-travel-time-hour cost is configured, because `RaptorUtils.createParameters` otherwise falls back
to `-utilityOfLineSwitch`, so both sides configure both.

### `routing_transfer_penalty_mode_to_mode`

The same schedule as `routing_distinct_platform_transfer`, with a 2 utils penalty on the `train` to
`bus` transport-mode pair. At 08:00 that turns the faster bus transfer at `rb_platform` into the
more expensive option and the slower rail transfer wins; the direct train service stays available as
a fallback. The penalty applies to the route's transport mode, not to the mapped passenger mode.
Configuring any mode pair selects MATSim's `ModeSpecificTransferCostCalculator`, which cannot also be
given a per-travel-time-hour cost; the Rust config rejects that combination rather than dropping one
of them silently.

This fixture records a **known deviation in the route cost**. The selected route, its rides and its
arrival time all match; the recorded `generalized_cost` does not: Java reports 9.0 utils where Rust
reports 6.0. `ModeSpecificTransferCostCalculator` ignores its `existingTransferCosts` argument and
returns the whole per-transfer cost, which `SwissRailRaptorCore` then re-adds as it walks the path,
so the reference's transfer cost depends on how many path elements it visits. The Rust router charges
one clipped cost per transfer instead, which is the behaviour the calculator's own contract describes.
Neither one cost per transfer nor a guessed re-adding rule reproduces 9.0, so the gap is pinned in
`java_reference.rs` rather than papered over; closing it means porting MATSim's incremental transfer
accounting. `routing_transfer_penalty_shared_stop` demonstrates the costs agreeing exactly where the
route has no transfers, which shows the utils conversion itself is sound and the deviation is specific
to mode-specific penalties.

### `routing_range_boundaries`

This fixture enables SwissRailRaptor range queries with a 60-second earlier and later window. A
request at 08:01 selects the unique transfer departing at the inclusive earlier boundary, 08:00; a
request at 08:09 selects the unique transfer departing at the inclusive later boundary, 08:10. A
request at 09:06 occurs after the final usable service and confirms that neither implementation
repeats the schedule on the next day. MATSim's top-level `TripRouter` falls back to a direct walk
when no PT route exists; the Rust PT routing module reports no path. The comparison pins the absence
of a PT service in both results while preserving that existing wrapper difference.

This fixture keeps its own copy of the transit schedule instead of reading
`../routing_direct_vs_transfer/transit_schedule.xml`. That schedule now carries stop `rd`, which the
one-to-all fixture added, and a reference is only comparable against the inputs it was recorded
with: an extra stop beside the access point changes the candidate stops and therefore the selected
departure.

### Transfer construction

`transit.transfer_construction` accepts `initial` (default), `adaptive`, and `online`. Initial builds
and retains candidates for every used stop at router creation. Adaptive builds candidates on first
use and retains them in a synchronized cache. Online rebuilds candidates on each query. The three
modes use the same candidate ordering and transfer rules, so repeated requests select the same
itinerary; the choice changes when candidate construction and retained memory occur. Initial trades
up-front work and memory for reuse, Adaptive spreads that work across encountered stops, and Online
avoids retaining candidate lists.

The pinned MATSim 2026.0 `RaptorTransferCalculation` exposes Initial and Adaptive. Online is a
Rust extension and has no direct mode-level reference comparison; its route choices are covered by
the same fixture assertions against the other two modes.

### `routing_person_specific_costs`

This fixture exercises the reference's per-subpopulation scoring: two passengers with different
mode utilities disagree on which service is cheapest. The schedule mirrors `routing_direct_vs_transfer`
(a 10-min `bus` and a 50-min `rail`), and `config.yml` declares two `mode_params` blocks that share
the `mode = "rail"` field while scoping the marginal utility to a single subpopulation. A "person"
passenger keeps the global `rail` utility, a "freight" passenger uses the subpopulation-specific
override, and the two requests therefore pick different services. The fixture exercises acceptance
criterion 2 of the parent (`Params… agree with scoring of the resulting passenger modes`) at the
subpopulation boundary rather than the global one. The Java differential reference for this fixture
has not yet been recorded against the reference runner; the routing assertion is Rust self-consistent
for now and will be promoted to a pinned comparison when the runner is updated.

## Comparison rules

The reference is recorded once and compared many times, so the rules are fixed and stated here
rather than discovered per run. They are per metric because the implementations do not agree on
everything, and a single global tolerance would hide exactly the differences worth knowing about.

| Metric | Rule | Rationale |
|---|---|---|
| Event order | Exact within each passenger and vehicle | Passenger boarding/alighting and each vehicle's stop sequence carry dependencies. Events from independent vehicles are compared in separate streams because their simultaneous order is not meaningful. |
| Agent, mode, activity type, leg mode, link | Exact | A difference is a different journey, never a rounding difference. |
| `distance` | Exact, full precision | Both implementations compute it from the same link lengths. A rounded form would hide a genuine difference. |
| Generalized route cost | Exact after converting Rust's time-equivalent cost to utility units | MATSim records RAPTOR's `totalRouteCost`; Rust uses the pinned 12 utils/hour PT time weight and 1 utility per transfer. |
| `boardingTime` | Exact | A schedule time, not a computed duration. |
| Service identity (line, route, board/alight stop) | Exact | A different service is a different journey. |
| supplied_plan event and leg times | Exact | The same-tick engine handoff now starts the next activity at the leg's arrival time. |
| queue_execution passenger event times | Exact | Boarding, alighting and activity transitions match the pinned run. |
| queue_execution vehicle stop times | Absolute difference no more than one clock step; delay changes by the same amount | The per-link queue and node phases differ by at most one tick at intermediate stops. This is bounded by the configured clock, not an event-count-dependent allowance. |
| queue_missed_connection passenger events | Absolute difference no more than two seconds | The supplied transfer misses the same service and boards the same later departure; stop and activity phases shift the walk handoff by one step. |
| queue_road_* bus delay from the no-car baseline | Absolute difference no more than two seconds | The baseline and congested queue phases each contribute at most a one-second shift to the compared delay. |
| queue_doors_* passenger events | Exact journey and event order; relative event time within one second of the stop arrival | Door handling is measured from the vehicle's stop arrival, independent of upstream link-phase differences. |
| queue_doors_* stop dwell | Absolute difference no more than one second at each stop | Dwell is departure time minus arrival time, isolating door operations from link travel time. |
| Arrival time, itineraries | Exact | The metric the routing fixture exists to compare. |

supplied_plan has no event-time tolerance: the activity and leg engines exchange completed agents
within the same tick. Queue stop events use one clock step because MATSim and Rust check a vehicle
at different phases of link/node processing; the same measured difference must appear in the stop's
schedule delay.

## Known divergences

supplied_plan's one-step-per-leg handoff lag was fixed in [#81](https://github.com/titipakorn-th/matsim-rust/issues/81).
The test now pins the worst lag to zero.

The routing fixture's former direct-service divergence was fixed by
[#72](https://github.com/titipakorn-th/matsim-rust/issues/72). It now compares the complete selected
itinerary, leg modes and arrival against the same pinned reference.

**2. PT no-path fallback** — `routing_range_boundaries`. After the last service, MATSim's outer
`TripRouter` emits a direct walk while the Rust PT module returns `NoPath`. The fixture verifies that
neither side invents a PT service; matching the outer fallback behavior is outside issue 77.

## Adding a fixture

1. Create `matsim_rust/tests/resources/pt_reference/<name>/` with a `config.xml`. Reuse inputs from
   `assets/`; do not copy a scenario that already exists.
2. Pin any value that decides between candidates on both sides — walk speed, clock, seed. The
   supplied-plan fixture pins the walk parameters for exactly this reason.
3. Add `requests.json` if the fixture makes routing claims. Give each request a descriptive `id`;
   the assertions name it.
4. Add a `*.yml` if the Rust side needs one, with the same inputs and pinned values.
5. Record the reference: `./java_reference/run_reference.sh <name>`.
6. Read `matsim_rust/tests/resources/pt_reference/java/<name>.json` and check that it says what you
   expect. A surprising recording is a finding, not a fixture to accept.
7. Add the assertions to `matsim_rust/tests/java_reference.rs`.
8. **Prove the assertions can fail.** Change the recorded reference in a way that should be caught
   and confirm the test fails, then restore it. A fixture never seen red is not evidence.

Every fixture added this way is a commitment: the recorded reference is a fact about the pinned
MATSim, and Rust is expected to converge on it. Do not record a reference and then assert the
current Rust behavior, even when that behavior is currently correct-looking — that converts a
divergence into a silent regression.

## Redistribution

The recorded references are **outputs of a run**, not MATSim source: they are this repository's own
data and carry this repository's license. Committing them is fine.

The MATSim jar is **not** committed. It is built from source into a cache-local Maven repository
outside the working tree. Anyone reproducing these fixtures builds it themselves from the pinned
commit.

If MATSim source or headers are ever copied into this repository — a transliterated
`SwissRailRaptor` method, a copied test — that is a derivative work of GPL-2.0-or-later code. It
requires preserving the upstream copyright and warranty notices, stating what was changed, and
keeping the whole combined work under the GPL. This repository is already GPL-3.0, so a direct
translation is permitted; reimplementing documented behavior without copying code is not a
derivative work and carries no such obligation. Note that this conclusion depends on the GPL: a
permissive license here would forbid transliterating RAPTOR code and would leave only
clean-room reimplementation. Check `matsim/COPYING`, `matsim/LICENSE` and `matsim/WARRANTY` in the
pinned checkout before relying on this.

## Inventory

Recorded from the pinned checkout, for scoping the port. Sources are under
`matsim/src/main/java/ch/sbb/matsim/routing/pt/raptor/`, tests under
`matsim/src/test/java/ch/sbb/matsim/routing/pt/raptor/`.

### Routing

| Feature | Production | Test |
|---|---|---|
| Least-cost search, one-to-one | `SwissRailRaptorCore` | `SwissRailRaptorTest`, `SwissRailRaptorTreeTest` |
| One-to-all trees, observed departures | `SwissRailRaptor.calcTreesObservable` | `SwissRailRaptorChainedDepartureTest` |
| Range queries / departure windows | `SwissRailRaptor.performRangeQuery` | `SwissRailRaptorTest.testRangeQuery` |
| Route selection, least cost and configurable | `LeastCostRaptorRouteSelector`, `ConfigurableRaptorRouteSelector` | `SwissRailRaptorTest`, `SwissRailRaptorConfigGroupTest` |
| Transfer construction, initial/adaptive/online | `SwissRailRaptorData` | `SwissRailRaptorDataTest.testTransfersFromSchedule` |
| Transfer cost, default and per mode pair | `DefaultRaptorTransferCostCalculator`, `ModeSpecificTransferCostCalculator` | `SwissRailRaptorTest.testTravelTimeDependentTransferCosts`, `testTransferWeights` |
| Transfer margins, minimum transfer time | `RaptorStaticConfig`, `RaptorUtils.convertRouteToLegs` | `SwissRailRaptorTest.testLongTransferTime_withTransitRouterWrapper` |
| Walking transfers between distinct stops | `SwissRailRaptorCore.createRaptorRoute` | `SwissRailRaptorTest.testLineChange`, `testFasterAlternative` |
| Stop finder, search radius, filters | `DefaultRaptorStopFinder` | `RaptorStopFinderTest` (13 cases) |
| Intermodal access and egress | `DefaultRaptorIntermodalAccessEgress` | `SwissRailRaptorIntermodalTest` |
| In-vehicle cost, capacity dependent | `DefaultRaptorInVehicleCostCalculator`, `CapacityDependentInVehicleCostCalculator` | `SwissRailRaptorInVehicleCostTest`, `CapacityDependentScoringTest` |
| Occupancy feedback | `OccupancyTracker`, `OccupancyData` | `OccupancyTrackerTest`, `SwissRailRaptorCapacitiesTest` |
| Person-specific parameters | `RaptorParametersForPerson`, `IndividualRaptorParametersForPerson` | `SwissRailRaptorModuleTest.testRaptorParametersForPerson` |
| Passenger mode mappings | `RaptorStaticConfig.addModeMappingForPassengers` | `SwissRailRaptorTest.testModeMapping`, `testModeMappingCosts` |
| Chained departures | `SwissRailRaptorData`, `RaptorRoute` | `SwissRailRaptorChainedDepartureTest` |
| Restricted boarding and alighting | `SwissRailRaptorCore.exploreRoute` | `SwissRailRaptorRestrictedBoardingAlightingTest` |
| No schedule repetition after 24 h | `SwissRailRaptor.calcRoute` | `SwissRailRaptorTest.testAfterMidnight` |
| Determinism | — | `RaptorDeterminismTest` |

Not covered by an upstream test in the pinned tree, so not evidence of intended behavior:
`RaptorTransferCalculation.{Adaptive,Online}`, `maxTransfers`, `exactDeparturesOnly`,
`useTransportModeUtilities`, the `LeastCostRaptorRouteSelector` tie-break, and
`ModeSpecificTransferCostCalculator`'s clamping. Several production classes carry no license header;
they remain under the package-level grant in `matsim/LICENSE`.

The Rust router's range-query settings are configured under `transit.range_query_settings` and
`transit.route_selector_settings`. It evaluates the requested departure, both window boundaries,
and access-adjusted scheduled departures in the window. Route scores use the configured travel-time,
departure-deviation, and transfer-count weights. Equal scores use `simulation::random::get_rng`
keyed by seed, person, and requested departure; this is deterministic but intentionally does not
reuse Java's random stream. Route selection leaves the previous activity's scheduled end time as
loaded, preserving the pinned timing limitation.

### Execution

The queue-based engine is in `org.matsim.core.mobsim.qsim.pt`: `TransitQSimEngine`,
`AbstractTransitDriverAgent`, `SimpleTransitStopHandler` / `ComplexTransitStopHandler`,
`PassengerAccessEgressImpl`, `TransitStopAgentTracker`, `TransitQVehicle`. Its dwell model is
4 s per boarding, 2 s per alighting, plus 15 s per stop visit
(`SimpleTransitStopHandler.handleTransitStop`); `TransitDriverTest.testHandleStop_EnterPassengers`
is what pins the 15 s.

The timetable-driven engine is in `contribs/sbb-extensions`: `SBBTransitQSimEngine`,
`SBBTransitDriverAgent`, `SBBPassengerAccessEgress`, `SBBTransitConfigGroup`. It emits a different
event set from the queue engine — `PersonEntersVehicle`, `PersonLeavesVehicle`,
`VehicleEntersTraffic`, `VehicleLeavesTraffic`, optionally link events — and
`SBBTransitQSimEngineTest.testEvents_withoutPassengers_withoutLinks` is a usable golden event
sequence.

`contribs/railsim` is a third, rail-specific engine and is out of scope.

The `timetable_mixed` fixture runs the supplied plans with `train` services on the SBB timetable
engine and `pt` services on the queue network engine. Rust selects SBB-style runs with
`transit.deterministic_service_modes`; those runs keep their event state on the single worker and
use the scheduled stop offsets while reusing the queue engine's passenger capacity and stop queues.
This path currently requires one partition. Cross-partition timetable service is tracked separately.
The fixture compares train and passenger event times within one second; the queue bus's final stop
allows two seconds for accumulated link/node phases, as the bus remains road-driven.

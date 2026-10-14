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

### `capacity_feedback`

Two passengers compete for one seat on the 08:00 direct service. A second direct departure leaves at 08:30, while separate riders use the 08:05–08:20 transfer. After two iterations, the request at 08:00:01 selects the transfer in both implementations. MATSim enables SwissRailRaptor's capacity constraint; Rust uses the occupancy and failed-boarding feedback collected during the first iteration. This compares the resulting itinerary, not the internal capacity algorithms, which differ as described in [the architecture notes](architecture.md).

### `routing_mapped_modes`

The same request and schedule with the `b_to_c` route changed to `bus`. Both configs enable
passenger mode mappings (`train` → `rail`, `bus` → `road`); rail and road have different travel
utilities, so the direct train service competes with the faster transfer by mapped passenger-mode
cost. The pinned Java and Rust routers choose the direct service, arrive at 08:50, and expose the
ride as a `rail` leg. This complements the preceding fixture, which checks route selection with
mapping disabled.

### `routing_combined_features`

This combines passenger mode mappings, subpopulation-specific mode utilities, a walk between
separate transfer platforms, and the bounded transfer penalty. The default subpopulation takes the
direct rail service; freight takes the faster rail–bus transfer, including the 130 m walk and
five-second boarding margin. Mode utility applies to ride time, while transfer walking and waiting
remain at the baseline PT cost.

### `routing_restricted_boarding_alighting`

Two faster transfer options are independently blocked: one route cannot alight at `rb`, and another
cannot board there. If either stop restriction is ignored, its faster transfer beats the valid
alternative. MATSim and Rust choose the slower transfer whose boarding and alighting are both
allowed, so the selected service sequence checks both flags rather than merely checking that a route
exists.

### `routing_chained_departure`

The 08:00 service from `ra` reaches `rb`, then continues as the linked 08:15 service to `rc` and the
linked 08:25 service to `rd`. Both continuation routes forbid boarding at their first stop, so a
normal transfer cannot use them. The 08:15 departure also links to a later route ending at `re`; a
request to `rd` must follow the matching branch. MATSim returns three route sections; Rust reports
one through ride, arriving at 08:35 without counting either linked dwell as a transfer.

### `routing_distinct_platform_transfer`

Two requests transfer between `rb` and `rb_platform`, separate facilities 100 m apart. The 08:00
request selects bus–rail; the 08:50 request selects rail–rail. The 08:00:01 request misses the bus
by one second, and a fourth request confirms that platforms 201 m apart are not connected by the
default walking transfer. The reachable requests compete with direct service under the same Java
route-cost objective. Their itineraries include the transfer walk, its 130 m beeline-adjusted
distance, and the pinned five-second transfer-walk margin. A separate Rust runner config supplies
a person plan so the generated access, transfer and egress legs execute in QSim; the routing test
also round-trips that itinerary through XML and protobuf population files. It rebuilds the routing
scenario from protobuf inputs and repeats the same-seed request, then compares the full itinerary
with the XML result. The execution test runs XML and protobuf input bundles with one and two
partitions and compares normalized passenger events and the final population.

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
arrival time all match; the recorded `generalized_cost` does not: Java reports 9.0 utils where Rust's
route costs 6.0.

The whole gap is 3.0 utils. Instrumenting `calcTransferCost` during a run of this fixture
decomposes both totals exactly:

| component | Rust | MATSim |
|---|---|---|
| in-vehicle time, transfer walk and waiting time | 5.0 | 5.0 |
| the one real `train` → `train` transfer | 1.0 | 1.0 |
| **a stale `train` → `bus` charge** | — | 3.0 |
| total | **6.0** | **9.0** |

Both sides price the real transfer identically: `transferPenaltyFixCostPerTransfer` is 1.0, which
`RaptorUtils.createParameters` derives from `utilityOfLineSwitch`, and no `train` → `train` mode pair is
configured, so the mode-specific offset contributes nothing. Rust's `TransitTransferPenalty::base_cost`
applies the same fallback. The 5.0 of time is the 600 s on `a_to_b`, the 156 s walk to `rb_platform`,
144 s of waiting and the 600 s on `b_to_c`, each at the pinned 12 utils/h minus 6 utils/h performing,
which is 1500 s at the module's 300 s per utility.

The 3.0 MATSim adds on top is an upstream defect, not a different accounting convention.
`CachingTransferProvider` (`SwissRailRaptorData`) holds a single mutable `raptorTransfer` field. Only
`handleTransfers` ever updates it, via `reset(transfer)` at `SwissRailRaptorCore:925`. `exploreRoute`
reads that same provider at `SwissRailRaptorCore:741` to price the arrival at every route stop past the
first boarding, and never resets it first. So the arrival at `rc` on a `train` → `train` journey is
priced by whatever transfer `handleTransfers` happened to leave behind — here the `train` → `bus`
transfer, worth 1.0 plus the configured 2.0.
`ModeSpecificTransferCostCalculator` ignores its `existingTransferCosts` argument and returns the whole
per-transfer cost, which makes that stale value additive; the `DefaultRaptorTransferCostCalculator`
subtracts `existingTransferCosts`, so the same stale read cancels and the shared-stop fixture is
unaffected.

The consequence is that the reference's transfer cost is a property of MATSim's round ordering, not of
the itinerary: the same journey priced from a different search order would report a different number.
Rust therefore cannot reproduce 9.0 without mirroring MATSim's internal search order, which is outside
the comparison boundary this harness sets. Rust charges one clipped cost per transfer, which is what
`ModeSpecificTransferCostCalculator`'s contract describes.

Both numbers are pinned in `java_reference.rs` so the gap stays visible and cannot drift silently.
`routing_transfer_penalty_shared_stop` demonstrates the costs agreeing exactly under the default
calculator, which shows the conversion and the time component are sound and isolates the deviation to
the stale mode-specific charge.

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

This fixture exercises per-subpopulation scoring: two passengers with different mode utilities
disagree on which service is cheapest. The schedule mirrors `routing_direct_vs_transfer` (a 10-min
`bus` and a 50-min `rail`), and the Java and Rust configs declare global and freight-specific mode
utilities. The Java config selects SwissRailRaptor's `Individual` scoring parameters; its default
router deliberately uses one parameter set for every passenger. `population.xml` gives the routing
requests actual persons with their respective subpopulation attributes; a request without that
population entry would silently route as a personless query and miss the feature under test. The
default `person` passenger chooses rail, while the `freight` passenger chooses the bus transfer. The
test compares both complete itineraries and arrival times against the pinned Java reference, then
repeats the requests and changes Rust partition count to check stable choices.

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

### Rust comparison status

Tags describe the strength of the Rust evidence: **Java differential** means the named fixture compares
Rust output with the pinned MATSim recording; **Rust-only** means the behavior has tests but no
Java comparison; **partial** means only the named cases are covered; **deviation** records known
behavioral differences. This is a fixture inventory, not a claim that every MATSim option is ported.

| Feature group | Tag | Rust evidence or remaining limit |
|---|---|---|
| Least-cost one-to-one and passenger mode mapping | Java differential | `routing_direct_vs_transfer`, `routing_mapped_modes`, `routing_combined_features` |
| One-to-all trees and observable departures | Java differential | `routing_direct_vs_transfer`; reachable, unreachable and isolated stops |
| Departure windows and no service after the final departure | Partial | `routing_range_boundaries`; direct-walk wrapper fallback remains a known deviation |
| Configurable route selectors and equal-cost tie-breaks | Partial | Default route choices are compared; Java differentials for non-default selector weights, `maxTransfers`, `exactDeparturesOnly`, `useTransportModeUtilities`, and Java RNG stream equivalence are not established |
| Transfer construction modes | Partial | Initial is the reference mode; Adaptive and Online are compared Rust-side; Online is a Rust extension |
| Default transfer cost and transfer margins | Java differential | `routing_transfer_penalty_shared_stop`, `queue_missed_connection`, `routing_distinct_platform_transfer` |
| Mode-pair transfer costs | Deviation | `routing_transfer_penalty_mode_to_mode` matches selected itinerary and arrival but generalized cost differs; incremental Java accounting and `ModeSpecificTransferCostCalculator` clamping are not ported or differentially verified |
| Distinct-stop walking transfers | Java differential | `routing_distinct_platform_transfer`, `routing_combined_features` |
| Stop radius, eligibility and link filters | Partial | `routing_intermodal_eligibility`, `routing_intermodal_unavailable_feeder`; a feeder returning `NoPath` is Rust-unit-tested only |
| Intermodal access and egress | Java differential | `routing_intermodal_access_egress`, `routing_intermodal_eligibility`, `routing_intermodal_unavailable_feeder` |
| Person-specific mode utilities | Java differential | `routing_person_specific_costs`, `routing_combined_features` |
| Capacity-aware route choice | Deviation | `capacity_feedback` compares the resulting choice; Rust occupancy/headway costs differ from Java's failed-boarding window exclusion |
| Chained departures | Java differential, partial | `routing_chained_departure` verifies two successive linked service sections and a two-way branch to distinct destinations, including no-boarding stops; XML and protobuf preserve references. Larger chained graphs and split/merge execution are not covered |
| Restricted boarding and alighting | Java differential | `routing_restricted_boarding_alighting`; each forbidden route would otherwise beat the valid transfer |
| Next-day schedule repetition | Rust-only | A routing unit test checks no service at 24:00 and a real service at 25:00; no pinned Java fixture covers it |
| Same-seed repeatability and partition stability | Java differential / Rust parity | Requests repeat with the recorded seed; `routing_person_specific_costs` and `routing_combined_features` check routing choices across partitions; `routing_distinct_platform_transfer` compares XML/protobuf decisions and same-seed repeats, plus execution final state across one/two partitions |

### Execution

| Feature group | Tag | Rust evidence or remaining limit |
|---|---|---|
| Supplied-plan activity and leg execution | Java differential | `supplied_plan`; event identities and times are exact |
| Queue transit boarding, alighting and capacity | Java differential | `queue_execution`, `queue_stranded`; one-seat denial and abort outcomes are covered across partitions |
| Missed transfer connections | Java differential | `queue_missed_connection`; event differences are bounded to two seconds |
| Road traffic competing with transit | Partial | `queue_road_baseline`, `queue_road_congestion`; bus delay agrees within two seconds for the pinned link and vehicle types |
| Serial and parallel vehicle doors | Java differential | `queue_doors_serial`, `queue_doors_parallel`; passenger order and stop dwell are compared |
| Mixed timetable and queue engines | Java differential, one/two partitions | `timetable_mixed`; the two-partition test forces one train link transition across the partition boundary |
| Timetable link and traffic events | Java differential | `timetable_link_events`; event identities, links and times are compared within the documented clock tolerance |

These tags summarize the pinned fixture corpus and leave small-fixture and untested-option limits
visible.

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
sequence. Rust's `transit.create_link_events_interval` mirrors SBB's
`createLinkEventsInterval`: 0 disables synthetic link/traffic events; positive values enable them
when `iteration % interval == 0`. Link transitions are distributed by downstream link length over
the scheduled interval between stops and are emitted on the first simulation tick at or after each
scheduled transition. The `timetable_link_events` fixture compares their vehicle identities, links,
and event times against the pinned SBB engine.

`contribs/railsim` is a third, rail-specific engine and is out of scope.

The `timetable_mixed` fixture runs the supplied plans with `train` services on the SBB timetable
engine and `pt` services on the queue network engine. Rust selects SBB-style runs with
`transit.deterministic_service_modes`; those runs use the scheduled stop offsets while reusing the
queue engine's passenger capacity and stop queues. The reference suite runs the fixture with one
partition and a targeted two-partition variant that forces one train link transition across a
partition boundary. That covers this handoff case, not arbitrary partition layouts or routes.
The fixture compares train and passenger event times within one second; the queue bus's final stop
allows two seconds for accumulated link/node phases, as the bus remains road-driven.

## User Story Coverage

Summary of all 38 user stories from [#69](https://github.com/titipakorn-th/matsim-rust/issues/69):

| # | User Story | Status | Implementation & Evidence |
|---|---|---|---|
| 1 | Transit itineraries match pinned Java reference | Implemented | `tests/java_reference.rs` (`a_faster_shared_stop_transfer_beats_a_direct_service`, `:1166`, `:1548`, `:2287`, `:2323`, `:2412`) |
| 2 | Direct vs transfer compete on routing cost | Implemented | `tests/java_reference.rs:1166`, `routing/mod.rs:1723-1786` |
| 3 | Bus-to-rail transfers | Implemented | `tests/java_reference.rs:1548`, `routing/mod.rs:3207` |
| 4 | Rail-to-rail transfers | Implemented | `tests/java_reference.rs:1548` (`b_to_c` at `rb_platform`) |
| 5 | Walking transfers between separate stop facilities | Implemented | `routing/mod.rs:1641-1714` (`nearby_transfer_stops`), `tests/java_reference.rs:1618` |
| 6 | Java-compatible transfer timing and margins | Implemented | `routing/mod.rs:686-687` (`RAPTOR_MIN_TRANSFER_TIME=60s`, `margin=5s`), `tests/java_reference.rs:1616-1639`, `:3403`, `:3568` |
| 7 | Walking access and egress | Implemented | `routing/mod.rs:1191-1203`, `:1252-1258`, `:1460-1468` |
| 8 | Configured intermodal access and egress | Implemented | `config.rs:579-585`, `routing/mod.rs:858-1027`, `tests/java_reference.rs:1267`, `:1512` |
| 9 | Person and stop filters for intermodal access | Implemented | `routing/mod.rs:893-913`, `:1577-1610`, `tests/java_reference.rs:1410` |
| 10 | Different costs per transit passenger mode | Implemented | `routing/mod.rs:1756-1785`, `tests/java_reference.rs:2287` (`routing_mapped_modes`) |
| 11 | Person-specific routing costs | Implemented | `routing/mod.rs:494-505`, `:638-682`, `tests/java_reference.rs:2323` (`routing_person_specific_costs`) |
| 12 | Configurable transfer costs | Implemented | `config.rs:611-636`, `:869`, `:894`, `routing/mod.rs:1859-1905`, `tests/java_reference.rs:1347` |
| 13 | Mode-to-mode transfer penalties | Implemented | `config.rs:623-636`, `routing/mod.rs:1875-1887`, `tests/java_reference.rs:1367`; route selection matches Java; 3.0 cost gap proved to be Java `CachingTransferProvider` stale read bug and documented above |
| 14 | Departure-window searches and route selection | Implemented | `routing/mod.rs:1907-2006`, `config.rs:729-747`, `tests/java_reference.rs:2412` |
| 15 | Subpopulation-specific range settings | Implemented | `routing/mod.rs:612-629`, `config.rs:699`, `:735`, `routing/mod.rs:4526` |
| 16 | Reference transfer-construction modes | Implemented | `config.rs:650-660`, `routing/mod.rs:1641-1661`, `:3513`; Online is a documented Rust extension |
| 17 | One-to-all routing + observable routing results | Implemented | `routing/mod.rs:1419-1477`, `tests/java_reference.rs:2193-2271` (`calcTreesObservable`) |
| 18 | Capacity-aware feedback from completed iterations | Implemented | `pt/feedback.rs:33-50`, `routing/mod.rs:1748-1845`, `controller/controller.rs:723`, `tests/java_reference.rs:1233`; documented Rust extension |
| 19 | Actual boarding capacity enforcement | Implemented | `pt/driver.rs:281-300`, `:396`, `tests/java_reference.rs:138` |
| 20 | Boarding, alighting and dwell Java-compatible | Implemented | `pt/doors.rs:74-138`, `pt/driver.rs:301-352`, `tests/java_reference.rs:860-880` |
| 21 | Queue-based buses interact with road traffic | Implemented | `tests/java_reference.rs:1064`, `docs/architecture.md:80-82` |
| 22 | Timetable-driven services selected by schedule mode | Implemented | `config.rs:563`, `pt/runs.rs:196-201`, `:265-294`, `engines/timetable_transit_engine.rs:94-146`, `tests/java_reference.rs:148` |
| 23 | Configured events for timetable-driven vehicles | Implemented | `config.rs:566`, `engines/timetable_transit_engine.rs:99-102`, `:186-247`, `tests/java_reference.rs:415` |
| 24 | Mixed execution journeys across partitions | Implemented | `tests/java_reference.rs:148-155`, `engines/timetable_transit_engine.rs:216-247` |
| 25 | Missing services and stranded passengers handled explicitly | Implemented | `pt/runs.rs:168-173`, `:236-240`, `:307-320`, `:346-356`, `tests/simulation/pt.rs:39`, `tests/java_reference.rs:731` |
| 26 | Departures beyond 24 h + final scheduled departure | Implemented | `routing/mod.rs:3739-3803`, `tests/java_reference.rs:2440-2462` |
| 27 | Car fallback restricted to declared car owners | Implemented | `routing/mod.rs:451` (`OWNS_CAR`), `:788-806`, `:1099-1120`, `:4335`, `:4493` |
| 28 | Missing ownership prohibits car fallback | Implemented | `routing/mod.rs:1110`, `:4351`, `:4366`, `:4383` |
| 29 | Personless PT queries return PT/walk/no-path | Implemented | `routing/mod.rs:572-588` (`TransitSkimOutcome`), `silo_routing.rs:229-245`, `:366`, `:391`, `:428` |
| 30 | SILO legacy car fallback explicit and separate | Implemented | `config.rs:577`, `routing/mod.rs:1032-1035`, `tests/silo_routing.rs:469` |
| 31 | Boardings and alightings by train line and stop | Implemented | `analysis/transit.rs:801`, `:894-909`, `tests/java_reference.rs:334-380` |
| 32 | Departure-segment occupancy incl. through passengers | Implemented | `analysis/transit.rs:802`, `:885-890`, `:1076-1112` |
| 33 | Journey access, egress, waiting and transfer measures | Implemented | `analysis/transit.rs:803`, `:1116-1213`, `tests/java_reference.rs:1692-1704` |
| 34 | Stop-level counts support station aggregation + sample expansion | Implemented | `analysis/transit.rs:822`, `:894-909`, `:1298-1314`, `:1405`, `tests/java_reference.rs:1722-1744` |
| 35 | Repeatable stochastic choices across partitions | Implemented | `routing/mod.rs:876-880`, `:2000-2005`, `:4654`, `tests/java_reference.rs:2380-2405` |
| 36 | XML and protobuf inputs produce equivalent decisions and state | Implemented | `routing/mod.rs:3703-3711`, `:3748-3780`, `:4164-4220` |
| 37 | Runnable differential fixtures at public boundaries | Implemented | `java_reference/run_reference.sh`, 21 recorded references, `tests/java_reference.rs` |
| 38 | Staged delivery with explicit feature coverage | Implemented | Detailed inventory and user story coverage matrix in `docs/pt_java_reference.md` |

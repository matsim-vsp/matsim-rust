# Java/Rust differential fixtures (PT)

This is the reference harness for the SwissRailRaptor and transit-execution port
([#69](https://github.com/titipakorn-th/matsim-rust/issues/69)). It pins MATSim, records what the
pinned reference does on a small set of shared scenarios, and compares the Rust port against that
recording at the two boundaries where behavior is observable: `TripRouter` for itineraries, and the
simulation integration runner for execution events.

It is deliberately a *harness*, not a compatibility claim. A differential test can only detect a
difference that a fixture exercises, and the fixture corpus is currently two scenarios wide. Every
divergence it reports is a fact about those scenarios, not a measure of overall parity.
The skims and external routing service entry points remain unchanged and are not covered here; their
reference semantics belong to ticket 11.

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
| `requests.json` | Routing requests issued through `TripRouter` after the run. Optional. |
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

## Comparison rules

The reference is recorded once and compared many times, so the rules are fixed and stated here
rather than discovered per run. They are per metric because the implementations do not agree on
everything, and a single global tolerance would hide exactly the differences worth knowing about.

| Metric | Rule | Rationale |
|---|---|---|
| Event order | Exact, positional | Both streams are in simulation order. The order carries meaning: boarding, alighting and service identity depend on it. Only genuinely independent events could be reordered, and reordering them would break the alignment. |
| Agent, mode, activity type, leg mode, link | Exact | A difference is a different journey, never a rounding difference. |
| `distance` | Exact, full precision | Both implementations compute it from the same link lengths. A rounded form would hide a genuine difference. |
| `boardingTime` | Exact | A schedule time, not a computed duration. |
| Service identity (line, route, board/alight stop) | Exact | A different service is a different journey. |
| Event and leg times | `0 ≤ rust − reference ≤ legs_completed × 1 s` | See below. A tolerance would let a real regression hide inside it. |
| Arrival time, itineraries | Exact | The metric the routing fixture exists to compare. |

Times are compared against a **derived bound**, not an arbitrary tolerance. MATSim hands an agent
from the leg engine to the activity engine within the same time step; this port does it one step
later (`Simulation::do_sim_step` documents the handoff). Each completed leg can therefore add one
second of lag, and the bound follows from the number of legs the agent has completed. The tutorial
agent completes 7 legs and the worst observed lag is 3 s, which is *within* the 7 s bound — the lag
does not accumulate on every leg, and the test pins the observed worst case at 3 s so the bound
cannot be widened silently to accommodate a new one.

## Known divergences

Both are recorded facts, not accepted outcomes. Each names the ticket that owns the fix.

**1. One-step-per-leg handoff lag** — `supplied_plan`. Rust's activity following a leg starts up to
one clock step later than the reference, giving the tutorial agent's later events a 1–3 s lag.
Owner: [#81](https://github.com/titipakorn-th/matsim-rust/issues/81) (queue-based passenger
execution). When it is fixed, the `worst_lag` assertion in `tests/java_reference.rs` will fail and
should be updated to `0.0` rather than removed.

The routing fixture's former direct-service divergence was fixed by
[#72](https://github.com/titipakorn-th/matsim-rust/issues/72). It now compares the complete selected
itinerary, leg modes and arrival against the same pinned reference.

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

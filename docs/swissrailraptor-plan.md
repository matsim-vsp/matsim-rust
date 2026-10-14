# SwissRailRaptor implementation plan

This proposal ports the public transport behavior of MATSim 2026.0, commit
`c7a75ebeddc3ceb62959af046190064bf23770df`. It follows
[the compatibility decision](adr/0001-java-transit-compatibility-target.md).
Implementation has not started.

## Reference and boundaries

The reference source is the tagged
[SwissRailRaptor package](https://github.com/matsim-org/matsim-libs/tree/2026.0/matsim/src/main/java/ch/sbb/matsim/routing/pt/raptor).
Configuration follows the tagged
[SwissRailRaptorConfigGroup](https://github.com/matsim-org/matsim-libs/blob/2026.0/matsim/src/main/java/ch/sbb/matsim/config/SwissRailRaptorConfigGroup.java).
Historical documentation helps explain intent but does not override tagged behavior.

Vehicle execution covers ordinary TransitQSim behavior and the
[SBB timetable-driven engine](https://github.com/matsim-org/matsim-libs/tree/2026.0/contribs/sbb-extensions).
The separate `railsim` extension and arbitrary user-written Java extensions are outside this work.
Supported upstream customization points need Rust equivalents where the ported features require them.

## Existing integration points

`simulation/replanning/routing/mod.rs` owns the current transit router, trip assembly and skims.
`simulation/controller/controller.rs` registers routing modules.
Plan preparation and replanning call those modules for agents.
`external_services/silo_routing.rs` also accepts personless requests.

`simulation/engines/transit_engine.rs` starts drivers and queues passengers.
`simulation/pt/` owns runs, stops, drivers and doors.
The network engine moves vehicles and their passengers between partitions.
Analysis rebuilds passenger rides from events and writes station, service and journey outputs.

The replacement retains `RoutingModule` and `TripRouter` entry points.
It uses the existing schedule, population attributes, garage, time types and deterministic RNG.
Immutable routing data belongs with the shared scenario lifecycle.
Iteration feedback is published as immutable snapshots after workers finish.
Partition-local passenger and vehicle state stays local to the executing worker.

## Delivery stages

1. Establish the Java comparison harness and feature inventory. Run a small Java fixture at the pinned commit and export a normalized itinerary. Record configuration, departure context and seed with expected results. Inventory tagged source and tests before claiming feature completeness.
2. Port fixed-departure RAPTOR routing, data preparation, access and egress, direct walking, and transfer construction. Compare direct and transfer connections together. Generate walking legs between distinct stop facilities and preserve Java transfer-time boundaries. Route selection uses Java costs rather than arrival time alone.
3. Integrate the router into preparation, replanning, trip assembly, skims and SILO. Remove the superseded internal search after migrating callers. Apply `ownsCar` only to passenger fallback and keep any SILO compatibility policy explicit. Validate that permitted car fallback has a routable vehicle resource.
4. Port advanced routing features. Cover passenger-mode mapping, person-specific scoring, range queries, intermodal access and egress, transfer margins, mode-specific transfer costs, all tagged transfer-construction modes, and one-to-all routing. Preserve tagged behavior for previous-activity timing and departures beyond 24 hours.
5. Port capacity-aware routing feedback. Collect occupancy and failed boarding from the completed iteration. Keep this prediction separate from the execution engine's actual boarding capacity. Publish feedback deterministically for the following iteration.
6. Validate queue-based transit execution against Java and fix observed compatibility gaps. Add timetable-driven execution selected by service mode. Support a mixed simulation with timetable-driven trains and buses on the queue network, including cross-partition transfers and the reference engine's configured link-event behavior.
7. Validate analysis and complete release checks. Compare station boardings, alightings, departure-segment occupancy and complete passenger journeys. Update architecture, configuration, testing and analysis documentation as implemented contracts change. Measure performance after correctness passes.

Each stage ends with runnable behavior and focused regression checks.
An incomplete stage does not establish full SwissRailRaptor compatibility.
Dependencies are added only when needed for the reference harness or implementation.

## Acceptance fixtures

The shared fixtures cover the following behavior:

- A direct service and a faster transfer service compete for the same journey.
- Bus-to-rail and rail-to-rail transfers use both shared and distinct stop facilities.
- A transfer exactly meets or misses its permitted timing boundary.
- The last departure, departures beyond 24 hours, walk-only routes and absent connections preserve Java behavior.
- Equal costs produce repeatable Rust choices from stable semantic RNG streams. These choices need not match Java's random stream.
- Range queries, person costs, passenger-mode mappings and intermodal filters change the itinerary as in the reference.
- Capacity feedback changes a later iteration's route choice independently of boarding enforcement.
- Full vehicles leave passengers waiting. Later services and end-of-simulation stuck outcomes are recorded correctly.
- Queue-based and timetable-driven services share passengers in a mixed-mode journey.
- XML and protobuf inputs produce equivalent decisions and final state.
- Single- and multi-partition executions preserve outcomes and meaningful event dependencies.
- Owners, non-owners, missing ownership attributes and personless requests exercise distinct fallback policies.

Identifiers, selected services, transfer counts and outcomes compare exactly for unique optima.
Time comparisons account for the configured simulation clock and Java's rounding rules.
Numerical tolerances are stated with each metric and cannot mask a different chosen service.
Normalization permits reordering only for independent simultaneous events.
Aggregate station counts supplement individual journey checks rather than replacing them.

## Validation and remaining evidence

Rust tests that create IDs use `#[deterministic_id_test]`.
Java fixtures run in a reproducible launcher rather than relying on an external developer service.
Validation starts with focused tests, then runs formatting, diff checks and the repository's release checks.
Berlin tests remain excluded unless explicitly needed, and then run only in release mode.

The pinned Java harness has not run yet. Numerical tolerances and execution gaps require evidence from shared fixtures.
No performance comparison has been measured. Licensing and redistribution requirements for reference material are checked before incorporating upstream code or generated fixtures.

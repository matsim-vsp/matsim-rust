# Applying katgpt-rs techniques to MATSim Rust

The strongest initial opportunities are shared route caching and speculative route proposals with exact-search fallback. Both can keep all 10 million people represented and leave the traffic engine responsible for queues, capacity, spillback, and vehicle events. Their benefit depends on routing's share of total runtime.

This document records the katgpt patterns implemented in this repository and their limits. Exact routing optimizations are enabled when their assumptions hold. Adaptive rerouting and proposal preparation are opt-in because they can change strategy behavior or add work.

## Inspected versions

- MATSim Rust: `64d80b178eff60c90bb098c26d4872199a19f31e`, cloned at `/home/varameth/matsim-rust`.
- katgpt-rs: `2865de70807046578c86d60e68668166c04bdf8b`, shallow clone at `/home/varameth/katgpt-rs`.

Links below name the inspected implementations. Local files can change after this assessment. The commits above fix the evidence.

## What the current implementation already does

The controller runs preparation, partitioned mobility simulation, scoring, and replanning. It shares immutable scenario data and keeps partition-local runtime state. Replanning uses Rayon. [Architecture](architecture.md), [controller](../rust_qsim/src/simulation/controller/controller.rs), [replanning](../rust_qsim/src/simulation/replanning/mod.rs).

The controller builds ALT routers, which use landmark-based lower bounds to accelerate A*. A route request includes departure time and optional person and vehicle references. Routing cost can vary by time and traveler characteristics. [Router construction](../rust_qsim/src/simulation/controller/controller.rs), [A*](../rust_qsim/src/simulation/replanning/routing/a_star.rs), [request and calculator interface](../rust_qsim/src/simulation/replanning/routing/least_cost_path_calculator.rs), [cost functions](../rust_qsim/src/simulation/replanning/routing/cost.rs).

Workers collect observed travel times and publish a complete immutable snapshot after all partitions submit. Routers read it through `ArcSwap`; they do not take the submission lock. [Travel-time calculator](../rust_qsim/src/simulation/replanning/routing/travel_time_calculator.rs).

The network already tracks active links and nodes. Each simulation tick visits active network objects, while the outer loop still advances through every tick. Skipping inactive links would duplicate existing behavior. [Network processing](../rust_qsim/src/simulation/network/sim_network.rs), [simulation loop](../rust_qsim/src/simulation/simulation.rs).

Current main supports multithreading. Its README marks MPI support as deprecated and directs MPI users to version 0.2.0 or earlier. [README](../README.md).

## Ranked opportunities

| Priority | Technique | MATSim insertion point | Expected source of savings | Semantic status |
|---|---|---|---|---|
| 1 | Reuse search buffers and initialize only discovered nodes | `a_star_core`, `RoutingAStarActions` | Less allocation and full-network setup for each route | Preserve costs and define tie behavior before claiming identical routes |
| 2 | Shared, versioned route cache | `LeastCostPathCalculator`, travel-time publication | Avoid repeated searches for equivalent requests | Exact only with a complete request key and immutable cost snapshot |
| 3 | Propose a route, validate it, and bound exact search | `LeastCostPathCalculator`, `a_star_core` | A good candidate can reduce search expansions | Exact only after a valid optimality check under the router's assumptions |
| 4 | Adaptive replanning budget | `StrategyManager`, `ReRouteModule` | Route fewer plans when conditions are stable | Changes strategy behavior; explicit experimental option |
| 5 | Batch route proposals | Replanning batch | Reuse prior routes as candidate proposals across requests | Candidate paths are validated by exact routing; model inference is not included |
| 6 | Shared destination guidance | Mode-specific routing graph | Share work across travelers with common destinations | Static homogeneous costs can be exact; grouped dynamic costs need approximation checks |

The ranking is an engineering judgment based on implementation cost and semantic risk. Profiling can change the order.

All six techniques now have an implementation in the routing or replanning path. The model-based proposal backend remains unimplemented because this repository has no route model or inference runtime. The opt-in proposal path batches existing routes as candidates and sends them through exact routing validation.

## 1. Remove route-search setup work first

`get_initial_queue` pushes every graph node into a keyed priority queue, including unreachable nodes with infinite priority. Each routing request also allocates full-node arrays for parents and arrival times. [Search core](../rust_qsim/src/simulation/replanning/routing/a_star_core.rs).

This gives every route full-network setup work even when the useful search is local. A proposal model would still pay that overhead unless the search implementation changes.

The search now uses a discovered-node frontier, explicit settled state, and generation-stamped per-thread scratch arrays for dense distance and settled data. Landmark searches materialize the dense distances that landmark generation needs. One-to-one route searches do not materialize that vector. Parent links and arrival times use sparse maps, so routing metadata is stored only for discovered nodes. A reentrant call gets fallback scratch rather than borrowing the same thread-local buffer twice.

This borrows katgpt-rs's practice of reusing hot-path buffers. Its flow-field cache, for example, owns reusable FFT and potential buffers. It does not require a Transformer dependency. [katgpt cache implementation](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-core/src/flow/cache.rs).

The priority ordering remains deterministic. The search retains equal-bound states so a candidate upper bound cannot change the existing tie selection. Tests cover the sparse search, candidate pruning, and cached results.

## 2. Cache equivalent route requests against one snapshot

Wrap `LeastCostPathCalculator` with a bounded cache. The controller already supplies this interface to `NetworkRoutingModule`; callers need not know how caching works. [Calculator interface](../rust_qsim/src/simulation/replanning/routing/least_cost_path_calculator.rs), [network routing](../rust_qsim/src/simulation/replanning/routing/network_routing.rs).

Borrow the shared-goal cache and invalidation idea from `FlowFieldCache`. Its cache tracks topology versions and dirty state. [katgpt flow-field cache](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-core/src/flow/cache.rs).

A traffic key needs origin, destination, mode, departure time, topology version, cost-snapshot version, and every relevant person and vehicle cost parameter. Disable exact caching for cost implementations whose dependencies cannot be identified. Never use borrowed addresses as key identities.

Travel-time snapshots expose a monotonically increasing epoch. Cache keys include request endpoints, exact departure nanoseconds, travel-time and disutility epochs, and complete built-in cost profiles. The cache is bounded by entry count and estimated route payload. Custom cost providers bypass the cache unless they expose a stable epoch and profile. [Snapshot publication](../rust_qsim/src/simulation/replanning/routing/travel_time_calculator.rs).

Rounding departure times to bins is not automatically exact. A route can enter later links in different bins, and interpolation can vary within a bin. Start with exact request equivalence. Treat time grouping as a separate approximation.

The cache currently uses FIFO eviction. Measure duplicate-request frequency, memory, and hit rate before assuming it helps at ten-million-person scale.

## 3. Draft a candidate and verify its value

katgpt-rs exposes `SpeculativeGenerator`, typed `GenerativeConstraintPruner`, and token-oriented `ConstraintPruner` and `ScreeningPruner` interfaces. These separate proposal generation, hard validity, and soft relevance. [Shared traits](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-core/src/traits/mod.rs).

For traffic, the router drafts from a previous route or reverse destination guidance. The router checks endpoints, link continuity, mode-graph membership, and finite nonnegative costs. It recomputes candidate cost while advancing arrival time along the route. Invalid candidates fall through to ordinary A*.

MATSim already has route-connectivity and mode validation in plan preparation. Factor the relevant checks into a reusable module rather than duplicating them. Its full network route includes departure and arrival links, while the least-cost calculator returns intermediate links. [Plan preparation](../rust_qsim/src/simulation/scenario/prepare_for_sim.rs), [path extraction](../rust_qsim/src/simulation/replanning/routing/a_star.rs).

A valid candidate supplies an upper bound, not proof that it is the least-cost route. The search prunes only when a certified static lower bound is strictly greater than the candidate cost. Dynamic costs and providers without static-bound guarantees use the candidate only as a route hint or fall back to ordinary A*. Equal bounds remain searchable so the baseline tie rule is preserved. ALT remains the lower bound.

The current search settles nodes without reopening them. Check heuristic consistency and time-dependent routing assumptions before adding stronger pruning or claiming optimality. The existing nonnegative-cost contract is necessary but does not by itself establish all conditions for general time-dependent disutility. [A* loop](../rust_qsim/src/simulation/replanning/routing/a_star_core.rs), [heuristic contract](../rust_qsim/src/simulation/replanning/routing/a_star.rs), [cost contract](../rust_qsim/src/simulation/replanning/routing/cost.rs).

LLM speculative decoding's distribution guarantees do not automatically transfer to route search. Route legality and route optimality need their own checks. Keep ordinary ALT as fallback when proposals fail or verification is not cheaper.

## 4. Spend replanning effort selectively

`StrategyManager` supports an optional reroute probability and periodic full-route interval. The default probability is unset, so the gate is disabled. When enabled, a stable RNG stream keyed by iteration and person makes the decision. The first reroute and each configured periodic check always run. [Strategies and rerouting](../rust_qsim/src/simulation/replanning/mod.rs), [replanning configuration](../rust_qsim/src/simulation/config.rs).

Borrow the budget-and-fallback pattern from katgpt-rs's `ExplorationBudget`, rather than its syntax-specific verification tiers. That implementation tracks remaining verification counts; it does not perform domain verification itself. [Exploration budget](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-pruners/src/exploration_budget.rs).

Skipping a configured reroute changes behavior even if the old route is still legal. Keep a nonzero exploration probability and periodic full checks so stable travelers can discover better routes. This gate changes behavior and has no score-convergence guarantee. Evaluate outcomes across independent seeds and equal compute budgets. All agents still enter mobility simulation.

Do not copy density-based game caching unchanged. A dense traffic queue can be where small capacity changes cause the largest network effects. Density alone is insufficient evidence that a computation can be skipped. [katgpt density policy](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-core/src/zone_density.rs).

## 5. Batch proposals without making execution timing a decision

When `replanning.batch_previous_route_proposals` is enabled, replanning collects prior network routes before parallel strategy execution. It sorts and bounds proposals deterministically, processes batches of 256, and publishes an immutable lookup table. Request-time exact routing validates each candidate. The built-in backend copies previous routes; it does not perform model inference. The existing external-service adapters remain separate and are not called by this path. [Proposal preparation](../rust_qsim/src/simulation/replanning/routing/mod.rs), [replanning pool](../rust_qsim/src/simulation/controller/mod.rs), [configuration](../rust_qsim/src/simulation/config.rs).

Start with deterministic batches between iterations. Live asynchronous inference introduces ordering concerns: apply responses by semantic simulation time and stable request identity, rather than whichever request finishes first. Use MATSim's seeded RNG contract if proposals are stochastic. The generic katgpt generator takes `fastrand::Rng`, which is not a direct substitute for MATSim's reproducibility contract. [Randomness contract](../AGENTS.md).

Measure proposal preparation, validation, fallback, and memory together. A model backend needs an explicit deterministic interface, timeout policy, and failure fallback before it can replace the built-in route-copy backend.

## 6. Adapt shared-goal guidance to roads

katgpt-rs's `FlowField` stores 2D direction vectors and uses FFT-smoothed potentials for crowd steering. Road routing uses directed links with mode restrictions and changing costs. Importing a grid direction into a road graph would not establish a legal or least-cost route. [Flow implementation](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-core/src/flow/mod.rs).

The A* router stores bounded reverse shortest-path trees for repeated destinations when its cost provider declares static route bounds. These trees provide candidates, then exact A* validates and bounds the request search. The tree uses the provider's global minimum disutility, so it can guide requests with different static traveler profiles without asserting that its path is optimal for them. Dynamic or unprofiled costs do not use reverse guidance. Destination endpoints are never grouped or changed.

## Prerequisites for a meaningful experiment

The current controller's `run_scoring_phase` sets all stored plan scores to `Some(1.0)`. Scoring-based link disutility exists, but full behavioral plan scoring is not implemented in that phase. Do not interpret score-based convergence or adaptive plan-quality learning as production MATSim behavior until that scoring path is implemented or supplied. Routing and fixed-plan mobility experiments remain useful. [Controller scoring](../rust_qsim/src/simulation/controller/controller.rs), [routing disutility](../rust_qsim/src/simulation/replanning/routing/cost.rs).

katgpt-rs pins Rust 1.98.1, while MATSim Rust pins 1.94.0. Importing its current crates requires a deliberate toolchain review. `katgpt-pruners` also depends on several other workspace crates. Begin with traffic-specific implementations of selected patterns, then evaluate a narrow, pinned dependency if direct reuse earns its cost. [katgpt manifest](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/Cargo.toml), [pruner dependencies](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-pruners/Cargo.toml), [MATSim toolchain](../rust-toolchain.toml).

## Recommended first experiment

Routing profiles record route-search counts, graph node counts, expanded nodes, cache hits, valid candidate bounds, candidate validation time, and searches without a valid bound in CSV and Parquet output. A one-person rerouting run confirmed the search counters appear in controller-side profiles. This verifies observability only; a valid performance comparison still needs a scenario that actually reroutes. The earlier fixed-plan comparison below did not call A*. [Routing profiling](../rust_qsim/src/simulation/profiling/routing.rs).

For the next comparisons, keep the same inputs, seed, worker count, output settings, and convergence criteria. Record total wall time, CPU time, peak memory, route-search count, nodes expanded, cache-hit fraction, candidate validation time, and full-search fallback fraction.

For exact changes, compare costs, routes, determinism, and vehicle events. For experimental adaptive replanning, also compare link volumes, travel-time distributions, stuck agents, and convergence across seeds. Profile smaller cases before allocating a ten-million-person run, then validate scale effects with a controlled population ladder.

## Initial runtime baseline

Built the unchanged release binary with `cargo build --release --bin local_qsim` after confirming the apt-installed CMake is available. The build completed successfully (about 73 seconds).

Ran the bundled Berlin v6.4 input, which contains 5,332 people and 119,174 nodes / 283,885 links. With its configured 0.001 QSim sample size and one ReRoute phase across two iterations, the complete command took 34.73 seconds wall time, 45.78 CPU seconds, and peaked at 2,526,804 KiB RSS. This is a small-scenario baseline, not a ten-million-person estimate. QSim spans accounted for 9.27 seconds in the measured run; the rest includes startup, preparation, replanning/scoring, and output. The route profile remained header-only, so route-search time and counts are not yet measurable.

A separate run with `qsim.sample_size=1.0` took 15.04 seconds wall time and peaked at 2,195,840 KiB RSS, but that override changes capacity scaling and should be treated only as a stress datapoint, not a behavior-comparable result. Neither run establishes a speedup or scaling curve.

## Routing experiment status

The five-run-per-build comparison on the bundled Berlin input used its default replanning configuration, `KeepLastSelected`. It therefore did not invoke A* and cannot measure the sparse-frontier change. Its wall-time, CPU, and RSS numbers are omitted as evidence for that change. The matched outputs only establish that those fixed-plan runs completed; they say nothing about route selection.

A full Berlin rerouting attempt stopped at a public-transport trip because this setup has no `pt` routing module. A controlled profile run with one car/walk-only person did reroute successfully on the 119,174-node network and emitted seven controller-side A* rows. Each row reported `node_count=119174`; the searches expanded 93, 7, 40, 1403, 967, 10, and 405 nodes. This confirms that the counters reach the profile output, but the single-person run is not a performance measurement.

Next, compare baseline and sparse-frontier builds on the same route-active input, with identical seed, output settings, and rerouting requests. Record route count and node expansions alongside runtime, CPU, and peak memory. Kernel profiling remains unavailable in this environment (`perf_event_paranoid=4`).

Checks completed so far: `cargo check -p rust_qsim --lib` passed, all 88 replanning tests passed, and both CSV and Parquet routing profile tests passed. The one-person rerouting profile contained seven controller-side A* rows. No matched runtime comparison has measured the new search setup costs yet.

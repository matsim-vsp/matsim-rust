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
| 3 | Propose a route, validate it, and bound exact search | `LeastCostPathCalculator`, `a_star_core` | A good candidate can reduce search expansions | Exact only after a valid optimality check; measured bound use stays `false`, so no expansion savings are established |
| 4 | Adaptive replanning budget | `StrategyManager`, `ReRouteModule` | Route fewer plans when conditions are stable | Changes strategy behavior; explicit experimental option |
| 5 | Batch route proposals | Replanning batch | Rank stored routes by empirical support and reuse common candidates across requests | A bounded deterministic frequency model proposes paths; exact routing validates them; no neural inference is included |
| 6 | Shared destination guidance | Mode-specific routing graph | Share work across travelers with common destinations | Static homogeneous costs can be exact; grouped dynamic costs need approximation checks. Limited to heuristics with a stated consistency property |

The ranking is an engineering judgment based on implementation cost and semantic risk. Profiling can change the order.

All six techniques now have an implementation in the routing or replanning path. The proposal path uses a deterministic empirical model that ranks stored plan routes by how often each path appears for a mode and OD pair. The model runs on bounded batches, and exact routing validates each proposed path. The repository has no neural route model or compatible neural inference runtime.

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

The cache currently uses FIFO eviction with limits of 4,096 entries and 4 MiB of estimated route payload per `AStar` instance. Mode-specific routers each own a cache, so total retained memory grows with the number of routers. Measure duplicate-request frequency, memory, and hit rate before assuming it helps at ten-million-person scale.

## 3. Draft a candidate and verify its value

katgpt-rs exposes `SpeculativeGenerator`, typed `GenerativeConstraintPruner`, and token-oriented `ConstraintPruner` and `ScreeningPruner` interfaces. These separate proposal generation, hard validity, and soft relevance. [Shared traits](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-core/src/traits/mod.rs).

For traffic, the router drafts from a previous route or reverse destination guidance. The router checks endpoints, each consecutive link connection, mode-graph membership, and finite nonnegative costs while accumulating candidate cost and travel time. Invalid candidates fall through to ordinary A*. Regression coverage includes a disconnected candidate whose endpoints match and whose cost would otherwise be low enough to prune the valid route, a legal but suboptimal candidate that must not be returned instead of the cheaper route, and a sweep comparing assisted and unassisted routers over every link pair on the triangle network.

Plan preparation validates full network routes, including departure and arrival links, modes, and connectivity. The least-cost calculator receives only intermediate links, so its candidate verifier checks continuity between the request endpoints and each intermediate link in the mode-specific graph. Keep this representation difference explicit. [Plan preparation](../rust_qsim/src/simulation/scenario/prepare_for_sim.rs), [A* path validation and extraction](../rust_qsim/src/simulation/replanning/routing/a_star.rs).

A valid candidate supplies an upper bound, not proof that it is the least-cost route. The search accepts that bound only when the popped priority is strictly greater than the candidate cost, so equal bounds stay searchable and the baseline tie rule is preserved. Dynamic costs and providers without static-bound guarantees use the candidate only as a route hint or fall back to ordinary A*. ALT remains the lower bound.

That test rarely decides a result, and the profile now says so. A validated candidate is a feasible route, so its cost is an upper bound on the optimum, and a goal-terminated A\* pops no state above the optimum before it settles the to-node. The bound therefore normally never fires, and `candidate_bound_used` stays `false` even when `candidate_valid` is `true`. The comparison is a safeguard for searches that cannot settle the to-node, not a search reduction, and the profile fields distinguish the two.

The current search settles nodes without reopening them. Check heuristic consistency and time-dependent routing assumptions before adding stronger pruning or claiming optimality. The existing nonnegative-cost contract is necessary but does not by itself establish all conditions for general time-dependent disutility.

Heuristics opt into bounds through `supports_consistent_static_bounds`. `ZeroHeuristic` states the property, since a zero estimate is consistent for nonnegative costs. `AltHeuristic` declines it: the landmark bound is admissible by the triangle inequality, but this implementation does not establish that it is consistent, and a search that settles without reopening relies on consistency to stay exact. Candidate bounds and shared destination guidance are therefore limited to the zero heuristic, and the production ALT routers do not use them. Establishing the property for landmarks would mean reweighting the search and revisiting this decision. [A* loop](../rust_qsim/src/simulation/replanning/routing/a_star_core.rs), [heuristic contract](../rust_qsim/src/simulation/replanning/routing/a_star.rs), [cost contract](../rust_qsim/src/simulation/replanning/routing/cost.rs).

LLM speculative decoding's distribution guarantees do not automatically transfer to route search. Route legality and route optimality need their own checks. Keep ordinary ALT as fallback when proposals fail or verification is not cheaper.

## 4. Spend replanning effort selectively

`StrategyManager` supports an optional reroute probability and periodic full-route interval. The default probability is unset, so the gate is disabled. When enabled, a stable RNG stream keyed by iteration and person makes the decision. The first reroute and each configured periodic check always run. [Strategies and rerouting](../rust_qsim/src/simulation/replanning/mod.rs), [replanning configuration](../rust_qsim/src/simulation/config.rs).

Borrow the budget-and-fallback pattern from katgpt-rs's `ExplorationBudget`, rather than its syntax-specific verification tiers. That implementation tracks remaining verification counts; it does not perform domain verification itself. [Exploration budget](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-pruners/src/exploration_budget.rs).

Skipping a configured reroute changes behavior even if the old route is still legal. Keep a nonzero exploration probability and periodic full checks so stable travelers can discover better routes. This gate changes behavior and has no score-convergence guarantee. Evaluate outcomes across independent seeds and equal compute budgets. All agents still enter mobility simulation.

Do not copy density-based game caching unchanged. A dense traffic queue can be where small capacity changes cause the largest network effects. Density alone is insufficient evidence that a computation can be skipped. [katgpt density policy](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-core/src/zone_density.rs).

## 5. Batch proposals without making execution timing a decision

When `replanning.batch_previous_route_proposals` is enabled, replanning counts stored network routes by mode, origin, destination, and path before parallel strategy execution. It caps the model at 4,096 unique candidates and 4 MiB of route payload, processes candidates in batches of 256, and publishes an immutable lookup table. For each request, the model proposes the most common stored path. Ties use path order. Request-time exact routing validates every candidate. The external routing-service adapters remain separate and are not called by this path. [Proposal preparation](../rust_qsim/src/simulation/replanning/routing/mod.rs), [replanning pool](../rust_qsim/src/simulation/controller/mod.rs), [configuration](../rust_qsim/src/simulation/config.rs).

Start with deterministic batches between iterations. Live asynchronous inference introduces ordering concerns: apply responses by semantic simulation time and stable request identity, rather than whichever request finishes first. Use MATSim's seeded RNG contract if proposals are stochastic. The generic katgpt generator takes `fastrand::Rng`, which is not a direct substitute for MATSim's reproducibility contract. [Randomness contract](../AGENTS.md).

This empirical model ranks stored plan alternatives. It does not learn from realized travel times, and duplicate plans increase a path's support. Measure proposal preparation, validation, fallback, and memory together. A neural backend would need an explicit deterministic interface, timeout policy, and failure fallback.

## 6. Adapt shared-goal guidance to roads

katgpt-rs's `FlowField` stores 2D direction vectors and uses FFT-smoothed potentials for crowd steering. Road routing uses directed links with mode restrictions and changing costs. Importing a grid direction into a road graph would not establish a legal or least-cost route. [Flow implementation](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-core/src/flow/mod.rs).

The A* router stores bounded reverse shortest-path trees for repeated destinations when its cost provider declares static route bounds and its heuristic states a consistent bound. These trees provide candidates, then exact A* validates them. The tree uses the provider's global minimum disutility, so it can guide requests with different static traveler profiles without asserting that its path is optimal for them. Dynamic or unprofiled costs do not use reverse guidance, and neither do the production ALT routers, which decline the consistency property. Destination endpoints are never grouped or changed. The 8 MiB tree budget applies per `AStar` instance, so mode-specific routers can multiply the total retained memory.

A tree is built on the second request for a destination, so which requests carry a candidate depends on the order requests arrive, and replanning runs in parallel. That state is shared mutable data guarded by a mutex, and it affects only whether a candidate is offered, never the returned route. A regression test warms two routers with opposite request orders and compares every result. If guidance is ever changed to affect the returned path, request order would become a determinism problem, and the repository's rule against collection-order-dependent outcomes would apply.

## Why the routing code does not use more katgpt-rs algorithms

`ExplorationBudget` limits software verification attempts by tier. A shared simulation-wide route budget would make decisions depend on parallel person-processing order. Per-person quotas would introduce a new strategy rule without evidence that they improve convergence. The current per-person seeded gate and periodic reroute check keep decisions independent of worker scheduling.

`ScreeningPruner` and `SpeculativeGenerator` expect model relevance scores or a generator that produces candidates. This repository has no trained road-route model. The bounded route-frequency proposal model uses stored plans as its data and leaves feasibility and cost decisions to exact routing.

The implementation uses these domain patterns without adding katgpt-rs as a dependency. katgpt-rs pins Rust 1.98.1 and its generic generator uses `fastrand`; this workspace pins Rust 1.94.0 and uses its own seeded RNG contract. [katgpt generator trait](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-core/src/traits/mod.rs), [exploration budget](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-pruners/src/exploration_budget.rs).

## Prerequisites for a meaningful experiment

The current controller's `run_scoring_phase` sets all stored plan scores to `Some(1.0)`. Scoring-based link disutility exists, but full behavioral plan scoring is not implemented in that phase. Do not interpret score-based convergence or adaptive plan-quality learning as production MATSim behavior until that scoring path is implemented or supplied. Routing and fixed-plan mobility experiments remain useful. [Controller scoring](../rust_qsim/src/simulation/controller/controller.rs), [routing disutility](../rust_qsim/src/simulation/replanning/routing/cost.rs).

katgpt-rs pins Rust 1.98.1, while MATSim Rust pins 1.94.0. Importing its current crates would require a toolchain review, and `katgpt-pruners` brings several other workspace crates. The current routing changes use local types and do not need that dependency. [katgpt manifest](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/Cargo.toml), [pruner dependencies](https://github.com/katopz/katgpt-rs/blob/2865de70807046578c86d60e68668166c04bdf8b/crates/katgpt-pruners/Cargo.toml), [MATSim toolchain](../rust-toolchain.toml).

## Recommended first experiment

Routing profiles record route-search counts, graph node counts, expanded nodes, cache hits, candidate validity, candidate-bound use, candidate validation time, and searches without a valid bound in CSV and Parquet output. `candidate_valid` and `candidate_bound_used` are separate measurements: the first says a stored path passed validation, the second says the search returned that path instead of its own result. Expect the second to stay `false` for the reason given in section 3. The matched route-active comparison below confirms the counters capture search work. The earlier fixed-plan comparison did not call A*. [Routing profiling](../rust_qsim/src/simulation/profiling/routing.rs).

For larger comparisons, keep the same inputs, seed, worker count, output settings, and convergence criteria. Record total wall time, CPU time, peak memory, route-search count, nodes expanded, cache-hit fraction, candidate validation time, and full-search fallback fraction.

For exact changes, compare costs, routes, determinism, and vehicle events. For experimental adaptive replanning, also compare link volumes, travel-time distributions, stuck agents, and convergence across seeds. Profile smaller cases before allocating a ten-million-person run, then validate scale effects with a controlled population ladder.

### Repeatable local runs

`scripts/routing_experiment.py` runs a release `local_qsim` binary repeatedly with a warm-up, stable seed, worker counts, separate output directories, and elapsed-time / RSS ceilings. It writes `experiment.json` plus one captured log per attempt. Use the same scenario, capacity scaling, output settings, seed, and worker counts for baseline and optimized binaries; only the binary/revision should differ in an exact comparison. For adaptive comparisons, use independent seeds and report transport outcomes as well as resource use. Use separate configs and output directories for fixed-plan and route-active workloads. A route-active run is interpretable only when routing profile output records nonzero searches.

Example on Linux after building the binary:

```sh
python3 scripts/routing_experiment.py \\
  --config rust_qsim/assets/berlin-v6.4/config.yml \\
  --input rust_qsim/assets/berlin-v6.4 \\
  --output-dir /tmp/matsim-fixed-plan-current \\
  --workload fixed-plan --population-size 5332 \\
  --network-nodes 119174 --network-links 283885 \\
  --sample-size 0.001 --simulated-duration-seconds 129600 --iterations 2 \\
  --demand-provenance 'Bundled Berlin v6.4 filtered 0.1% population' \\
  --seed 4711 --qsim-workers 2 --replanning-workers 2 \\
  --build-profile release --build-settings 'cargo build --release; RUSTFLAGS=unset' \\
  --warmups 1 --runs 3 --max-seconds 600 --max-rss-kib 8388608 \\
  --set controller.last_iteration=1
```

The `--input` paths must cover the full input bundle; files and directory contents are hashed. The worker option maps to `partitioning.num_parts` (QSim workers); replanning threads are set separately. RSS and CPU values are sampled from Linux `/proc` at 100 ms intervals and may miss brief peaks or the final CPU fraction. For a formal result, also retain kernel-level process accounting and the generated routing profile outputs. The driver records config/input/binary hashes, source revision/dirty state, Rust toolchain, declared build settings, hardware, command lines, run logs, declared workload dimensions, and resource ceilings. It does not generate population sizes or verify declared build/workload metadata: provide a matching input/config per ladder rung and verify declarations. Set elapsed/RSS ceilings according to the machine allocation; the script does not prescribe a feasibility threshold.

## Initial runtime baseline

Built the unchanged release binary with `cargo build --release --bin local_qsim` after confirming the apt-installed CMake is available. The build completed successfully (about 73 seconds).

Ran the bundled Berlin v6.4 input, which contains 5,332 people and 119,174 nodes / 283,885 links. With its configured 0.001 QSim sample size and one ReRoute phase across two iterations, the complete command took 34.73 seconds wall time, 45.78 CPU seconds, and peaked at 2,526,804 KiB RSS. This is a small-scenario baseline, not a ten-million-person estimate. QSim spans accounted for 9.27 seconds in the measured run; the rest includes startup, preparation, replanning/scoring, and output. The route profile remained header-only, so route-search time and counts are not yet measurable.

A separate run with `qsim.sample_size=1.0` took 15.04 seconds wall time and peaked at 2,195,840 KiB RSS, but that override changes capacity scaling and should be treated only as a stress datapoint, not a behavior-comparable result. Neither run establishes a speedup or scaling curve.

## Routing experiment status

The five-run-per-build comparison on the bundled Berlin input used its default replanning configuration, `KeepLastSelected`. It therefore did not invoke A* and cannot measure the sparse-frontier change. Its wall-time, CPU, and RSS numbers are omitted as evidence for that change. The matched outputs only establish that those fixed-plan runs completed; they say nothing about route selection.

The earlier full Berlin rerouting attempt stopped at a public-transport trip because this setup has no `pt` routing module. For a valid route-active workload, I selected 100 people from the Berlin 0.1% population whose legs use car or walk and whose explicit `routingMode` attributes are either absent or `car`. This avoids plans that look car-only by leg mode but still request PT routing.

For each population size, runs used the same input, the 119,174-node / 283,885-link Berlin network, seed (`4711`), worker settings, ReRoute strategy, QSim sample size (`0.001`), and two-iteration config; paired configs differed only in output directory. This isolates route-active replanning, not full-population mobility simulation. The baseline was built from archived commit `789ff1c`; the optimized binary came from commit `7307024`. The baseline commit predates the routing work, so the comparison is tied to exact source revisions.

| Population | Metric | Baseline (`789ff1c`) | Optimized (`7307024`) |
|---:|---|---:|---:|
| 100 (3 runs) | Wall time | 11.42 s (11.31–12.53) | 12.94 s (9.95–17.05) |
|  | User CPU time | 32.58 s (31.34–33.47) | 18.15 s (15.34–23.87) |
|  | Peak RSS | 2,137,036 KiB (2,135,888–2,137,268) | 1,682,776 KiB (1,678,804–1,687,832) |
| 200 (3 runs) | Wall time | 16.11 s (12.62–22.21) | 17.81 s (11.28–26.40) |
|  | User CPU time | 47.57 s (46.36–60.01) | 25.71 s (19.09–29.51) |
|  | Peak RSS | 2,203,528 KiB (2,203,428–2,205,416) | 1,756,836 KiB (1,754,124–1,756,988) |

The optimized runs consistently recorded 380 searches and 838,406 node expansions for 100 people, and 740 searches and 1,796,027 expansions for 200. The baseline predates those routing profile counters, so its per-search counts are unavailable. The first baseline and optimized runs produced identical serialized route records (2,286 for 100 people and 4,482 for 200) and byte-identical compressed vehicle-event files at each population size. Median user CPU time fell by 44% and 46%, and median peak RSS by 21% and 20%, respectively. Wall-time ranges overlap at both sizes and the optimized medians are higher. These runs support lower CPU and memory use on these cases, but do not establish a wall-time improvement or predict ten-million-person behavior. Route-cache and reverse-tree limits apply per router, and search scratch is retained per worker thread. Kernel profiling remains unavailable in this environment (`perf_event_paranoid=4`).

Checks recorded for the historical comparison: `cargo check -p rust_qsim --lib` passed, all 91 replanning tests passed, and both CSV and Parquet routing profile tests passed. The runtime comparison used release builds from the archived baseline and optimized commits. Subsequent correctness changes in the current source were not included in those measurements.

The historical measurements above predate the experiment driver and are not a completed population ladder. This checkout has no recorded 10k/100k/500k/1M full-population runs or matched adaptive-policy outcome study. The existing adaptive option remains disabled by default, makes per-person seeded decisions with mandatory first and periodic checks, and still requires independent-seed measurements of link volumes, travel times, and stuck agents before its behavioral/resource tradeoff can be assessed. The full million-person feasibility claim remains unproven; do not extrapolate it from the 100/200-person routing subset.

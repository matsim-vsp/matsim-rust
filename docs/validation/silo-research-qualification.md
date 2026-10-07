# SILO matsim-rs research qualification

Date: 2026-10-06

## Decision

**Not qualified as a behaviorally equivalent Java MATSim replacement.** The paired 1% QSim comparison still has material differences in car boardings, completed car trips, stuck events, and link events after matching the sample scale and 60-second stuck threshold. A corrected end-to-end 1% SILO run passed integration operation. An exact-input Java/Rust QSim pair has since completed; it still shows material behavioral differences, so it does not establish equivalence or empirical validity.

Rust may be used as a separately identified exploratory engine while its results are checked against study-specific outcomes. A confirmatory study should wait for the gates below to pass or explicitly justify the engine differences and demonstrate robust conclusions.

## Evidence so far

### Configuration

The SILO Rust config writer previously omitted Java QSim capacity factors and stuck time. The Rust run therefore used its defaults (`sample_size=1.0`, `stuck_threshold=10`) even though the Java 1% config used factors `0.01` and `stuckTime=60` seconds.

The writer now passes Java flow capacity as Rust `sample_size`, Java stuck time as `stuck_threshold`, Java `removeStuckVehicles` as `remove_stuck_vehicles`, and Java's global random seed as Rust `computational_setup.random_seed`. It rejects unequal Java flow/storage factors because Rust currently has one shared scale. The focused `MatsimRsRunnerConfigTest` passes and covers the mappings and rejection.

### Routing fallback defect

The incomplete 1% run's PT `no_path` messages came from a link-mode mismatch at the SILO/Rust boundary. SILO selected endpoint links from its PT-only filtered network, while matsim-rs handles PT from coordinates and its no-transit-path fallback invokes the car network router. In the captured log this appears as PT requests from bus/rail links failing with `mode car`. SILO now anchors all matsim-rs requests on its car-filtered network; the Rust transit router still uses the request coordinates and schedule for PT. Rust no longer returns zero solely because an origin and destination share a link; the selected mode router handles that case.

Regression checks pass: `MatsimTravelTimesRustRoutingTest` (3 tests) confirms PT requests use car-routable anchors, and `cargo test -p rust_qsim --test silo_routing -- --test-threads=1` passes all 6 route-service tests, including same-link mode dispatch. The end-to-end run predates this fix. A fixed 1% rerun was deferred because a separate 2% SILO run was already using the shared machine; no Java simulation started in the deferred attempt. The earlier partial output is preserved in `BKK_CASE/scenOutput/VMV_BAU_1pct_matsimrs_routing_incomplete_prior`.

The corrected end-to-end 1% SILO run uses SILO seed 123. Its MATSim global seed is left at the default 4711, which is also the Rust default; the effective sample size is 0.01 and the stuck threshold is 60 seconds. My first run was killed during QSim with exit code 137, before writing events or starting the route service. The cgroup recorded no OOM kill, and kernel logs were unavailable, so the cause is unknown. A separate corrected 1% SILO run wrote the full 24-hour Rust QSim events file (about 744 MB) and started the Rust route service, but predates the endpoint-link fix. Its artifacts are under `BKK_CASE/scenOutput/VMV_BAU_1pct_matsimrs_routing` in the SILO workspace.

The post-fix 1% rerun used `/tmp/silo-rs-1pct-matsimrs-routing-fixed.log` and completed successfully (`Finished SILO`, wrapper exit 0) at 20:44 on 2026-10-06. It ran for about 1 hour 12 minutes, including a long modal-share phase that requests routes per employed person. Rust QSim completed; SILO reported 151,921 persons, 54,027 households, and 57,400 dwellings. Modal shares were 0.4181 car and 0.5819 PT.

The route service handled 132,947 car requests (132,766 routed; 181 beeline fallbacks) and 145,721 PT requests (145,698 routed; 23 beeline fallbacks). The fallback totals are 0.136% of car requests and 0.016% of PT requests. Logged examples say `No route found ... with mode car`, including PT requests whose car-compatible anchors are disconnected in the car network. The release binary (`cefe399cd4f3`) predates the `failure_category` response mapping, so all 204 fallbacks were labeled `service_error`; the run does not distinguish no-path from transport/service failures in its final summary. Thus integration operation passed for this run, while fallback classification needs confirmation with a rebuilt binary.

### Paired QSim behavior

A Java MATSim QSim baseline and a matsim-rs QSim run used the same preserved 1% export: network, plans, transit schedule, and vehicles. Both used random seed 4711, capacity/sample factor 0.01, stuck threshold 60 seconds, and `removeStuckVehicles=true`. The Java baseline completed. The Rust QSim completed through scoring after fixing stuck-agent retention; it also started the route service.

Input SHA-256 values were identical across the pair:

| Input | SHA-256 |
|---|---|
| `network.xml` | `867b366b2f55945ef6b204ea55000fc12d5e40c4d16f3236bcf4b69569e3b572` |
| `plans.xml` | `0a27828338db33e1871ce08f8a8de0c0740d40d75e1519f985f3a8462c4a3fce` |
| `transitSchedule.xml` | `9c8cdf2a2635997171effd9e9e58a1a366851ed92412d9eeec0956bfe81b8ee3` |
| `vehicles.xml` | `f5aa4263ffb907efa2173e585d16402dedef38920f3fe6a8feb58224209120df` |

The matsim-rs release binary used for this run has SHA-256 `a0cd56081cbd6e92887fbefa3ec76a7c20bb4b54621548b9d032f96256fea9e0`.

| Metric | matsim-rs | Java MATSim | Rust difference vs Java |
|---|---:|---:|---:|
| Car departures | 128,937 | 126,760 | +1.7% |
| Car boardings | 128,937 | 126,760 | +1.7% |
| Car completions | 57,049 | 59,373 | -3.9% |
| Stuck before 24:00 | 71,453 | 83,435 | -14.4% |
| Stuck at 24:00 | 754 | 5,441 | — |
| Private-car link entries | 2,059,957 | 1,952,335 | +5.5% |

Rust and Java encode transit link movement differently, so total link-entry counts are not comparable; the car-only Java count excludes transit vehicle IDs. Departure and boarding counts agree within 1.7%, but the completion, stuck, and car-link counts still differ materially. This pair does not establish behavioral equivalence.

The first stuck-removal run exposed a second defect: aborted agents were omitted from the population returned for scoring, causing a population-size assertion after QSim. The fix retains aborted agents as terminal `STUCK` agents, preserving their partial plans for scoring while avoiding a duplicate end-of-day stuck event. The corrected standalone 1% QSim completed scoring and wrote events/plans under `BKK_CASE/scenOutput/VMV_BAU_1pct_matsimrs_routing_stuck_abort_retained`. The earlier failed output is preserved under `..._stuck_abort`.

## Qualification gates

1. **Integration operation — passed for the fixed 1% run:** SILO reached `Finished SILO`; Rust QSim completed; its route service remained available through reporting; routing success and fallback counts were recorded.
2. **Behavioral comparison:** compare identical inputs, effective settings, and multiple paired seeds. Before examining the comparison, define tolerances for the study's primary outcomes (at least car/mode shares, completed trips, travel time, congestion, and the actual research estimands). Treat differences outside those tolerances as a failure, not as parity.
3. **Reproducibility:** repeat matsim-rs with identical inputs and seed and verify that the primary outcomes are stable at the precision the study reports.
4. **Empirical validity:** validate the chosen engine against independent observed data relevant to the study. Java agreement alone is not empirical validation.
5. **Performance claim:** if Rust's runtime advantage is part of the justification, benchmark both engines with production settings and repeated paired runs; report median and range, failures, and end-to-end elapsed time.

## Still open

- The matched paired QSim run fails the behavioral-equivalence gate based on the differences above.
- The modal-share report's per-person routing is a major runtime cost; the fixed 1% run took about 1 hour 12 minutes.
- The run's release binary predates `failure_category` serialization, so its 204 `service_error` fallbacks are not classified precisely. Rebuild and rerun if separate no-path and genuine service-failure counts are required.
- No study-specific outcome tolerances or estimands have been recorded, and no empirical validation or repeated-seed reproducibility check has been done.
- No repeated runtime benchmark has been done; no speedup claim is supported.
- Java configurations with different flow and storage capacity factors are not representable by the current Rust QSim config and are rejected by SILO.

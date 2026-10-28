# Testing

By default, every test in Rust is executed in parallel. This may cause problems with some tests that have global state (e.g. a global ID
store or a global logger).

You might use the `[serial]` attribute to mark a test as serial. But note that this only makes sure that all such marked
tests are executed sequentially. Any other test not being marked as serial will still be executed in parallel and there
is no guarantee that `serial` test will be the only one to run.

This is why all tests requiring an ID store should be marked as `[deterministic_id_test]`. This will (1) ensure
that the ID store is empty before the test starts and (2) run the test in serial.

If you need a test to be run exclusively in serial, it should be an integration test. This is also the case for tests
where the logger is set during the test. As a convention, each integration test should be an `[deterministic_id_test]`.

## METIS tests

The tests in `simulation::network::metis_partitioning` call the native METIS library. They used to be flaky and
occasionally aborted with `SIGSEGV` (e.g. `test_andorra_with_inlinkcount_and_capacity`). The cause was an invalid input
graph: METIS does not check its input, and the graph contained loops, duplicate edges and edges without a reverse edge.
Since the graph is built as described in [architecture.md](architecture.md#general-scenario-handling), these tests are
expected to pass reliably. A crash there is a bug, not flakiness.

`assert_valid_metis_graph` in these tests checks the input requirements of METIS, similar to METIS' own `graphchk`
program. Use it when changing how the graph is built. On macOS, Guard Malloc makes invalid memory accesses inside METIS
reproducible; run the test binary with `DYLD_INSERT_LIBRARIES=/usr/lib/libgmalloc.dylib`.

## Traffic dynamics

Focused tests in `network::storage_cap`, `network::link::kinematic_tests`, `network::sim_network`
and `messaging::sim_communication::message_broker` cover wave arrival boundaries, short links,
mixed PCE, both release paths, inflow exhaustion, lazy updates after large time jumps, sleeping
and empty links, sampling, tick length, identical local/split admission ticks, and transmitting
several future releases exactly once. Tests creating IDs use `deterministic_id_test`.

Run formatting, focused tests and the remaining suite before the expensive Berlin comparisons:

```shell
cargo fmt --all -- --check
mkdir -p matsim_rust/test_output
cargo test -p matsim-rust --release --lib simulation::network:: -- --test-threads=1
cargo test -p matsim-rust --release --lib simulation::messaging:: -- --test-threads=1
cargo test --workspace --release --features http -- --test-threads=1 --skip berlin::
cargo test -p matsim-rust --release --test simulation berlin::test_berlin_1 -- --exact --test-threads=1
cargo test -p matsim-rust --release --test simulation berlin::test_berlin_2 -- --exact --test-threads=1
git diff --check
```

The two Berlin tests retain their Queue event-file references. Those files are not a reference
for Kinematic Waves.

## Berlin performance measurement

The initial [Kinematic Waves comparison](performance/kinematic-waves.md) records the six measured
combinations, raw runtimes, result counts, source revisions, machine, compiler and input hashes.

The ignored `controller::controller::berlin_performance::berlin_performance` library test uses
the existing Berlin-v6.4 0.1% scenario and one iteration. It refuses debug builds. For each of
one and two partitions it loads fresh simulation state for one warmup and three measurements.
The timer encloses `MobsimWorkerPool::run_mobsim`, including communication, waiting for all
workers and final synchronization. Loading, METIS, plan preparation, partition construction,
result checks and worker shutdown are outside it. The first worker command also initializes
the partition's simulation, as in a regular controller run.

All performance runs set `output.write_events: None`. Input files, seed 4711, simulation times
0–129600 s, sampling 0.001, one tick/s and the 30 s stuck threshold remain those of the Berlin
config. Counters record completed legs (`PersonArrival`, all modes) and link transitions
(`LinkEnter`), using local counters with a single merge per partition. Each row reports the
three runtimes' median, minimum, maximum and range; repeated runs must reproduce the counts.

```shell
cargo test -p matsim-rust --release --lib berlin_performance -- --ignored --nocapture --test-threads=1
MATSIM_BENCH_DYNAMICS=KinematicWaves cargo test -p matsim-rust --release --lib berlin_performance -- --ignored --nocapture --test-threads=1
```

The manually dispatched `.github/workflows/kinematic-waves.yml` executes the unchanged Queue physics from `b943ec9`
(the benchmark-only commit based on `7fc3125`) and the current source on the same runner.
It copies the current benchmark harness to the baseline so harness fixes are identical on both
sides; the exact harness diff is retained. The workflow then runs all correctness checks and the
Queue/Kinematic Waves after measurements on that same machine. Its artifact records raw runs,
source revisions, Rust version, CPU details, input SHA-256 hashes and config alongside a summary.
`scripts/summarize_berlin_performance.py` checks Queue count equivalence and flags slowdowns over
5% when the observed ranges do not overlap. Such a flag needs investigation and another complete
same-machine comparison to distinguish a repeatable regression from runner noise.

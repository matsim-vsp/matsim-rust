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

## Agent phases and network performance

The phase API includes compile-fail doctests for read-only capabilities, activity-only operations and
the consumption of mutable projections during a phase transition. Run them with `cargo test -p
matsim-rust --doc`. Agent tests that create IDs use `#[deterministic_id_test]` as usual.

The three-link simulation tests record an ordered event trace in addition to the normal event-file
comparisons. The latter deliberately ignore ordering among simultaneous events and therefore cannot
detect a changed activity/departure or link-leave/link-enter sequence. The ordered trace also covers
zero-duration interaction activities; a separate test stops exactly at the final arrival to preserve
the distinction between arriving and starting the final activity in the next tick.

`agent_network_benchmark` measures actual node/link movement of 1,024 vehicles along 64 links. Scenario
construction, agent/ID creation and output are outside the timed interval; no event files are written.
It uses three warmup samples and ten measurements by default:

```shell
cargo test -p matsim-rust --release --lib agent_network_benchmark -- --ignored --nocapture --test-threads=1
```

Set `MATSIM_AGENT_BENCH_SAMPLES=1` for one measured sample after the three warmups. For before/after
comparisons, build and preserve the baseline test executable outside `target/` before modifying the
implementation, then build the new executable with the same settings. Alternate both executables for
ten measurement pairs on the same otherwise idle machine. Compare paired timings and their scatter;
a repeatable slowdown outside baseline variation is a regression. Do not compare debug timings or
run this benchmark concurrently with compilation or other benchmarks.

For the phase/role migration, the final ten alternating pairs on Apple Silicon with Rust 1.94.0
(release, default features, 2026-10-27) gave median sample times of 5.093 ms before and 5.115 ms after.
The median paired change was +0.56%; the mean paired change was +0.14%, with an approximate 95%
Student-t interval of [-2.33%, +2.61%]. This measurement did not resolve a slowdown. Construction and
the increased fixed storage for the private leg runtime are outside this hot-path measurement.

## METIS tests

The tests in `simulation::network::metis_partitioning` call the native METIS library. They used to be flaky and
occasionally aborted with `SIGSEGV` (e.g. `test_andorra_with_inlinkcount_and_capacity`). The cause was an invalid input
graph: METIS does not check its input, and the graph contained loops, duplicate edges and edges without a reverse edge.
Since the graph is built as described in [architecture.md](architecture.md#general-scenario-handling), these tests are
expected to pass reliably. A crash there is a bug, not flakiness.

`assert_valid_metis_graph` in these tests checks the input requirements of METIS, similar to METIS' own `graphchk`
program. Use it when changing how the graph is built. On macOS, Guard Malloc makes invalid memory accesses inside METIS
reproducible; run the test binary with `DYLD_INSERT_LIBRARIES=/usr/lib/libgmalloc.dylib`.

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

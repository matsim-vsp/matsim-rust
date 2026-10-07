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

## Known flaky tests

The tests in `simulation::network::metis_partitioning` call the native METIS library. They are known to be flaky: on some
builds and machines, one of them (we have seen `test_andorra_with_inlinkcount_and_capacity`) occasionally aborts with
`SIGSEGV`. A failure there does not indicate a regression in unrelated code, and rerunning usually passes.

Because a segmentation fault terminates the whole test process, it also hides all tests that would have run afterward. If
you want to validate other code, skip these tests:

```shell
cargo test --workspace -- --test-threads=1 --skip berlin:: --skip metis_partitioning
```

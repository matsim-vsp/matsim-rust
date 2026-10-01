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

Scoring test configurations must list a finite, explicit `typical_duration_s` for every main activity type that the
test executes. There is deliberately no runtime fallback for missing typical durations. Scoring coefficients in the
configuration retain MATSim's public units (`util/h` for time coefficients); the scorer converts them to `util/s`
once when it is constructed.

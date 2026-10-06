# Repository guidance for coding agents

This file applies to the entire repository. Follow a more deeply nested
`AGENTS.md` or `AGENTS.override.md` as well if one is added later.

## Project purpose

This repository contains a parallel Rust implementation of MATSim's QSim. The
main goal is not merely to produce a plausible traffic simulation: behavior,
edge cases, and event semantics should stay as close as practical to MATSim's
Java implementation while taking advantage of Rust's ownership and concurrency
model.

The Cargo workspace has two members:

- `rust_qsim`: the simulation library, command-line tools, and almost all tests.
- `macros`: procedural macros used by the simulation and its tests.

Read these files before making broad changes:

- `README.md` for dependencies, build commands, and runnable examples.
- `docs/architecture.md` for scenario ownership and worker lifecycles.
- `docs/tests.md` for the rules around the global ID store and logging.
- `docs/analysis.md` for final-iteration report conventions.

## Repository map

- `rust_qsim/src/simulation/`: core simulation code.
- `rust_qsim/src/simulation/controller/`: iteration orchestration and scenario
  transitions.
- `rust_qsim/src/simulation/engines/`: activity, leg, network, transit, and
  teleportation engines.
- `rust_qsim/src/simulation/pt/`: transit runs, drivers, stops and boarding doors.
- `rust_qsim/src/simulation/network/`: the runtime network, capacities,
  partitioning, and link dynamics.
- `rust_qsim/src/simulation/replanning/`: selectors, strategies, routing, and
  travel-time handling.
- `rust_qsim/src/simulation/scenario/`: input models and preparation of plans,
  routes, vehicles, and transit data.
- `rust_qsim/src/simulation/io/`: XML and protobuf adapters.
- `rust_qsim/src/simulation/id/`: external-to-internal ID mapping and its global
  store.
- `rust_qsim/src/simulation/random.rs`: the reproducible RNG contract.
- `rust_qsim/src/bin/`: conversion, merge, partitioning, and simulation entry
  points.
- `rust_qsim/src/experiments/`: exploratory code; do not treat it as the
  production architecture without checking its callers.
- `rust_qsim/tests/resources/` and `rust_qsim/assets/`: test and example inputs.

`Scenario` owns data while inputs are read. The controller converts it to a
`ControllerScenario`, whose immutable core is shared through `Arc` while the
population remains mutable. Each mobsim iteration creates partition-local
runtime state and population shards. Preserve these ownership boundaries;
avoid solving borrowing problems by cloning an entire scenario or population.

## Toolchain and native dependencies

- Use the Rust version pinned in `rust-toolchain.toml` and Rust 2024 idioms.
- The build requires the native METIS library and libclang. A compile or link
  failure mentioning METIS, clang, or bindgen may be an environment problem,
  not a source-code regression.
- `.cargo/config.toml` contains Apple Silicon Homebrew search paths. Do not
  generalize or replace them incidentally while working on unrelated code.
- `rust_qsim/build.rs` compiles the `.proto` sources with the bundled
  `protobuf-src` compiler. Edit the `.proto` definitions or handwritten adapter
  code, never generated files in Cargo's `OUT_DIR`.
- The optional `http` feature pulls in remote-download support and may require
  additional TLS/OpenSSL support on some systems.

Do not add or upgrade dependencies unless they are needed for the requested
change. Prefer the standard library and dependencies already in the workspace.

## Core behavioral invariants

### MATSim compatibility

- Treat MATSim Java behavior as the reference when porting an algorithm or when
  the task supplies Java source. Preserve its boundary cases, but account for
  differences in representation instead of transliterating blindly.
- Document intentional semantic deviations and cover them with tests.
- Route representation is one known difference: this Rust code's network-route
  vector contains the departure and arrival links, whereas MATSim Java APIs may
  expose only intermediate link IDs. Check the local type before porting route
  calculations.
- Main/network-mode legs may legitimately lack travel times because QSim derives
  them. For other legs, plan preparation can synchronize a travel time when it
  is present on the leg or its route. Keep mutation/adaptation separate from the
  subsequent read-only validity check.

### Determinism and IDs

Reproducibility is part of the simulation contract, not just a testing aid.

- The same complete input bundle, configuration, and seed should produce the
  same behavior across runs.
- Never let `IntMap`, `IntSet`, `HashMap`, or `HashSet` iteration order decide a
  simulation outcome. When processing order is observable, establish a stable
  order explicitly or use dense data indexed by internal ID.
- Internal IDs are dense and assigned through the global ID store. XML loading
  canonicalizes person IDs before assigning them; protobuf loading restores the
  stored mapping. Preserve the association between protobuf data and its ID
  store.
- XML and protobuf are different serializations of the same scenario model.
  Avoid introducing new format-dependent behavior. If a change targets
  cross-format equivalence, compare final simulation state separately from
  harmless ordering differences among simultaneous events.
- Use `simulation::random::get_rng(base_seed, purpose, stream_id)` for
  simulation randomness. Give each stochastic operation a stable, distinct
  purpose string and derive stream IDs from semantic context such as iteration
  and person ID.
- Do not use `thread_rng`, `SmallRng`, randomized default hashers, process-local
  addresses, wall-clock time, or collection iteration order as seed material.
- Changes to RNG seed encoding or stream naming alter reproducibility. Make such
  changes deliberate, explain them, and update golden-vector tests.

### Numerical behavior

- Validate invalid, missing, NaN, and infinite inputs explicitly where they can
  enter scoring or routing logic.
- Use numerically stable formulations for probabilities and accumulated costs.
  For example, normalize logit utilities before exponentiation rather than
  relying on values not to overflow.
- Add short comments explaining non-obvious mathematical transformations and
  why they preserve the intended result.
- Use tolerances for floating-point assertions when exact representation is not
  the contract; use exact comparisons when testing deterministic streams or
  serialized values.

## Editing expectations

Before editing:

1. Inspect `git status --short` and the relevant diff.
2. Read the complete local code path, its tests, and the nearest documentation.
3. Treat every existing modification as user work. Work around it and preserve
   it unless the request explicitly says otherwise.

While editing:

- Keep the change focused and prefer small, idiomatic Rust patches over broad
  rewrites.
- Preserve all existing code comments. If a comment becomes inaccurate, update
  it in place instead of deleting it.
- Retain established public APIs, names, error behavior, and ownership
  boundaries unless changing them is part of the task.
- Do not silence warnings broadly. Fix the cause or use the narrowest possible
  allowance with a reason.
- Avoid unnecessary cloning in hot simulation paths. Prefer clear borrowing,
  staged mutation followed by validation, or copy-on-write where the code
  already uses it.
- Do not hand-edit generated build output, files below `target/`, or runtime
  output below `out/` and `rust_qsim/test_output/`.
- Update `docs/architecture.md`, `docs/tests.md`, or `README.md` when their stated
  contract changes.

Do not run destructive Git commands, discard changes, amend commits, create
commits, or change branches unless the user explicitly asks.

## Tests and validation

Tests that touch the global ID store or initialize logging need exclusive,
repeatable setup.

- Use `#[deterministic_id_test]` for tests that create or resolve IDs, depend on
  a clean ID store, or need exclusive logger setup. The macro resets the store,
  initializes thread-local logging, and serializes participating tests.
- A `#[serial]` attribute alone does not exclude ordinary parallel tests. Put
  tests that require process-wide exclusivity in an integration-test binary and
  follow `docs/tests.md`.
- Do not make tests pass by weakening determinism assertions or by depending on
  execution order.

Start with the narrowest relevant check, then widen validation in proportion to
the change. During implementation work, skip the Berlin integration tests
(`berlin::test_berlin_1` and `berlin::test_berlin_2`) by default because they take
very long in debug mode. If Berlin validation is needed, run it only with
`--release`; never run these tests in debug mode. Typical commands from the
repository root are:

```shell
cargo test -p rust_qsim --lib path::to::test -- --test-threads=1
cargo test --workspace -- --test-threads=1 --skip berlin::
cargo test -p rust_qsim --release --test simulation berlin:: -- --test-threads=1
cargo fmt --all -- --check
```

The CI-equivalent build and test path is stricter and uses release mode, the
`http` feature, warnings as errors, and single-threaded tests:

```shell
RUSTFLAGS="-D warnings" cargo build --release
RUSTFLAGS="-D warnings" cargo test --release --verbose --features http -- --test-threads=1
```

Run `cargo fmt --all` after changing Rust code. Do not require `cargo clippy` as
a substitute for the checks above; it is not currently part of CI. Full tests
can take several minutes, so focused tests should fail fast before the full
suite is started.

For behavioral changes, add regression tests that cover the nominal case and
the meaningful edge cases. In particular, consider:

- same seed and semantic context producing the same choice;
- different purposes or contexts producing independent RNG streams;
- empty, missing, non-finite, and boundary values;
- start link equal to end link in route calculations;
- XML/protobuf ordering when serialization may affect simulation order;
- multi-partition behavior when touching controller or messaging code.

## Completion standard

Before reporting completion:

- Review the final diff and run `git diff --check`.
- Confirm that unrelated user changes remain untouched.
- State which checks passed and which were not run, including the reason.
- Call out compatibility, determinism, performance, or input-format consequences
  that a reviewer should know about.

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues; use the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the default labels `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: use root `CONTEXT.md` and `docs/adr/` when present. See `docs/agents/domain.md`.

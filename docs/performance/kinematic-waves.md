# Berlin v6.4, 0.1%: same-runner performance comparison

One warmup and three fresh measured runs per row; release builds, no event files. Completed legs count PersonArrival events of all modes; link entries count LinkEnter events.

| Source / dynamics | Partitions | Median (s) | Min (s) | Max (s) | Range (s) | Completed legs | Link entries |
|---|---:|---:|---:|---:|---:|---:|---:|
| Queue before | 1 | 6.370133 | 6.295674 | 6.615746 | 0.320072 | 50854 | 410325 |
| Queue before | 2 | 3.106473 | 3.105579 | 3.165845 | 0.060266 | 50854 | 410325 |
| Queue after | 1 | 6.240430 | 5.956887 | 6.244755 | 0.287868 | 50854 | 410325 |
| Queue after | 2 | 3.210720 | 3.138015 | 3.234807 | 0.096792 | 50854 | 410325 |
| Kinematic Waves after | 1 | 7.138555 | 7.101232 | 7.227459 | 0.126227 | 50828 | 409738 |
| Kinematic Waves after | 2 | 3.676088 | 3.666246 | 3.718454 | 0.052208 | 50828 | 409738 |

- Queue before/after, 1 partitions: -2.04%; counts unchanged. No regression above 5% beyond the observed spread.
- Kinematic Waves / Queue after, 1 partitions: +14.39%. Traffic physics and therefore result counts may differ.
- Queue before/after, 2 partitions: +3.36%; counts unchanged. No regression above 5% beyond the observed spread.
- Kinematic Waves / Queue after, 2 partitions: +14.49%. Traffic physics and therefore result counts may differ.
- Queue before, two/one partitions: 0.488 times the runtime.
- Queue after, two/one partitions: 0.515 times the runtime.
- Kinematic Waves after, two/one partitions: 0.515 times the runtime.


## Source, machine and settings

Measured on 2026-10-28 in [workflow run 40742461021](https://github.com/matsim-vsp/matsim-rust/actions/runs/40742461021).
All six rows ran sequentially on the same Ubuntu 24.04 VM: AMD EPYC 7763, four virtual CPUs
(two cores, two threads per core), x86-64. Rust was 1.94.0, LLVM 21.1.8, release profile,
`--features http`, `RUSTFLAGS="-D warnings"`.

- Before: `b943ec9f7b75756a29e2b978961c611aec77e246`, the benchmark-only commit based on
  `7fc3125910e4015bb49d313b63678bf4634ea927`. The identical corrected benchmark harness was
  copied onto this revision; traffic-physics files were unchanged.
- After: branch commit `6b1377e6de969477d038b1304b783746f09b4977`, compiled as GitHub's PR merge
  commit `541d1157a527323cca84b769d6c07ce4ed360bb5` against the same base.
- Seed 4711; one iteration (0); simulation 0–129600 s; sample 0.001; one tick/s; stuck threshold
  30 s; original Berlin input files and METIS settings. Only the partition count and dynamics vary
  by row. Every performance run disables event-file writing. Logging, profiling and the normal
  worker observers retain their configured behavior.
- Each row has one warmup and three measured runs, each with fresh scenario/partition runtime,
  capacities, population and workers. Timing encloses `MobsimWorkerPool::run_mobsim`, including
  communications and final synchronization. Reading, partitioning, plan preparation, partition
  construction and result checks stay outside the timer.

[Raw runtimes, counts, source revisions, input SHA-256 hashes and complete base configuration](kinematic-waves.measurements.json)
are committed alongside the [compiler and machine details](kinematic-waves.environment.txt).
The JSON records all 24 runs, including warmups and effective performance overrides. The linked
workflow artifact additionally contains the raw logs and baseline harness diff.

## Interpretation

Queue preserves 50,854 completed legs and 410,325 link entries with both partition counts,
matching the baseline on every warmup and measured run. The median changes are -2.04% and
+3.36%. The two-partition timing ranges overlap. No Queue slowdown above the roughly 5%
investigation threshold was observed.

Kinematic Waves takes 14.39% (one partition) and 14.49% (two partitions) longer than the current
Queue implementation. It completes 26 fewer legs and records 587 fewer link entries by the same
end time. Its counts also agree exactly between one and two partitions and repeat across all
runs. These results include the changed traffic physics; the runtime difference therefore
covers a different set of completed transitions as well as the additional capacity work.

Two partitions take 48.8% of the one-partition baseline runtime and about 51.5% for each current
mode. Communication and synchronization are included. The nearly equal current-mode ratios
show no disproportionate loss of parallel scaling for Kinematic Waves in this scenario. This
comparison does not isolate the cost of communication from the remaining simulation work.

## Validation

Formatting and warnings-as-errors release builds passed. The dedicated workflow then passed
82 focused network tests, the messaging and configuration tests, and the complete remaining
suite without Berlin. Both existing Queue Berlin tests passed separately in release mode with
their unchanged event-file reference. `git diff --check` passed. The normal Rust workflow also
passed the full release suite with `http` and single-threaded tests (628 library tests, all
integration tests and doctests). The 16 added behavioral regressions are included in those
checks; the Berlin performance test remains ignored by default. Queue event files were not
used as a Kinematic Waves reference.

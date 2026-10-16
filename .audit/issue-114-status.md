# Issue #114 status, 2026-10-16

Issue #114 remains open. The raw routing timeout and raw day/ReRoute blockers now have passing
local evidence. Full scoped compatibility is still incomplete.

## Completed in the 32-partition continuation

- `computational_setup.global_sync` now drains incoming messages after the exchange, so cross-partition
  transfers, including zero-duration walking legs between transit stops, arrive in the same tick. The
  default remains `false`, so unsynchronized runs are unchanged.
- Follow-on teleported legs admitted to the next exchange keep their original event time; the receiving
  queue deadline is the later of the nominal arrival tick and the actual send tick.
- Transit vehicle activities now carry the midpoint coordinate of their link instead of `None`.
- Consecutive duplicate links in a transit route are traversed once, and the stop set is preserved.
- The backpacking collector keeps a person-to-vehicle index and drops the mapping when a leg is
  abandoned, so a stuck person is not reported as still occupying the vehicle.
- A stuck event mid-leg now closes that leg as aborted instead of silently discarding it and letting the
  abandoned leg swallow the resumption events. Aborted legs are not scored, because they have no arrival.

Measured the raw 1% Bangkok bundle at 32 partitions with `global_sync` enabled: the run completes with no
errors in 418.0 s, of which 328.3 s is population preparation from raw plans and 66.8 s is QSim. The
prepared-plan run spent 65.2 s in QSim, so the post-exchange barrier costs about 2.5% of that phase.

## Completed in the first continuation

- Indexed configured transfer relations by origin stop, preserving directed first-match behavior.
- Pruned dominated simple-cost round labels and redundant boardings during route scans.
- Skipped all segment work when the capacity snapshot is empty.
- Measured eleven raw travelers through public `TripRouter::calc_route`, five calls each.
  All 55 calls pass the agreed 1 second budget; worst 0.122114851 seconds.
- Completed the unchanged raw 0.1% sample through iterations 0 and 1, including production ReRoute.
  1,310 people acquired second plans; 261 selected new plans contain PT.
- Recorded 25,833 completed journeys out of 26,024 planned journeys, or 99.266%.
  There are 171 stuck legs, all at 24:00. Walking-only completion is 94.1%.
- Verified that the empty-feedback shortcut preserves both complete event files and final plans
  byte for byte against the preceding build.
- Reanalyzed the final run and matched all 13 checked journey, leg, and transit CSV files exactly.
- Passed the warnings-as-errors release build and non-Berlin `http` suite: 945 tests passed,
  15 ignored, 2 filtered. This includes 793 library and 30 pinned Java differential tests.
  Formatting and whitespace checks pass.

## Evidence

- `issue-114-raw-sampled-final.csv` and `.tsv`: per-call timings and test build hash.
- `issue-114-check-raw.py`: rerun the selected raw travelers with a supplied release test executable.
- `issue-114-bangkok-run-v7.tsv` and `.log`: exact command, inputs, build hash, phases, RSS, outcomes.
- `issue-114-bangkok-run-v8.tsv`: the 32-partition `global_sync` command, phase timings, and barrier cost.
- `issue-114-completion-v7.json`: per-mode departure/arrival/stuck accounting for both iterations.
- `issue-114-count-completion.py`: event-pair accounting with an abort/late-arrival self-check.
- `issue-114-reanalysis-v7.json`: exact export comparison after standalone reanalysis.
- `issue-114-validation-summary.txt`: CI-equivalent command and result summary. Full logs remain local.

The final run is at
`/mnt/Data/bangkoksilo/scenOutput/baseline_0p1pct_20261016_issue114_rust_v7`.
Its PT execution is teleported, as configured in the original raw input bundle.

## Remaining acceptance work

- Complete the general optional-feature search and prove dominance with mode costs, signed/hourly
  penalties, chained continuation, range selection, and capacity feedback. The general heap search
  remains; the simple-cost optimization cannot establish this gate.
- Resolve the unapproved mode-pair generalized-cost and reference capacity differences. Complete
  non-default selector, intermodal, chained/mixed execution, and analysis coverage gaps in the
  evidence matrix in `docs/pt_java_reference.md`.
- Run controlled, matched, interleaved Java/Rust measurements. No speedup claim is established.
- Run same-build repeated-seed and raw XML/protobuf checks. The actual multiple-partition check is now
  covered at 32 partitions on the raw 1% bundle; the v6/v7 output comparison still uses two builds and is
  not a substitute for the remaining gates.
- Extend isolated latency coverage beyond the selected travelers' first PT trips. The sampled
  result does not prove every raw query is below 1 second.

This PR addresses the raw routing and run blockers. It does not close #114.

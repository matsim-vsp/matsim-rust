# Run the partition assignment experiment

Use `partition_network --coarse-regions` to compare the corrected weighted-METIS baseline with an assignment of more
coarse network regions to the requested QSim workers. The mode is opt-in. Without `--coarse-regions`, the command
keeps its existing partitioning behavior.

Prepare a network with `cmp_weight` values first. The binary conversion path writes route-occurrence weights from the
population into the network. Then run the partition command in release mode:

```shell
cargo run --release -p matsim-rust --bin partition_network -- \
  --net-path path/to/network.binpb \
  --num-parts 8 \
  --coarse-regions 32 \
  --seed 4711 \
  --demand-windows path/to/run.workload_windows.json
```

The command writes the selected network to `network.8.binpb` and its comparison metadata to
`network.8.experiment.json` and writes the corrected weighted-METIS layout as a sibling named
`network.8.baseline.binpb`. The metadata records the input and source revisions, baseline and candidate layout IDs,
solver revision and limits, seed, workload definitions, preparation durations, each worker's estimated loads and
neighbor counts, effective METIS contiguity, structural checks, and rejection reasons. If the input network is
disconnected, METIS contiguity is disabled for preparation because METIS cannot partition a disconnected graph in
contiguous mode; final candidate connectivity is still checked and a rejected candidate selects the baseline. A
coarse-partition error is recorded and also selects the baseline.

`convert_to_binary --workload-sidecar` writes an hourly workload sidecar from selected-plan routes and departure times. The solver
balances total precomputed route-link occurrences, memory estimated as 64 bytes per node, and one route-link
occurrence count per node for each supplied hour. Legs without departure times are counted in the sidecar and omitted
from hourly dimensions. The memory value is a fixed estimate, not a measurement. Hourly counts are input-based proxies,
not observed per-tick QSim work.

The command accepts a candidate only when its maximum estimated total work improves, every estimated load including
hourly work stays within baseline maxima, its weighted edge cut and maximum neighbor count do not increase, and each
worker's share of every original network component stays connected when contiguity is required. Disconnected inputs
disable METIS's global contiguity option, then validate component-wise connectivity for both the weighted baseline
and candidate. Use `local_qsim --partition-diagnostics path.csv` to record per-worker
work duration, synchronization wait, active vehicle-link steps, link transitions, migrations, and received
inter-partition messages in hourly simulated-time windows. The command's preparation durations do not measure QSim runtime. Diagnostics do not measure
actual memory use or establish simulation compatibility; compare saved events, final population state, and routing
responses for matched runs before making a compatibility claim.

Run matched repeated release sweeps with `scripts/partition_pair_experiment.py`. Supply one baseline and candidate
network for each worker count, the same scenario config and complete input bundle, and choose at least one warm-up and
three measured repetitions. The runner alternates baseline/candidate order, records elapsed/CPU/peak RSS and host
provenance, retains failed runs, compares event summaries and canonicalized final-plan state, and repeats each pair
with diagnostics disabled and enabled to expose instrumentation cost. Enabled runs also record controller phase
timings. These measurements describe only the supplied workload; they do not establish population-scale behavior.
Event summaries separately report stranded agents, abandoned legs, and agents that resume at the scheduled start of
their next activity. The runner alternates both layout order and diagnostics-on/off order across repetitions.

```sh
python3 scripts/partition_pair_experiment.py \
  --config path/to/config.yml --binary target/release/local_qsim \
  --baseline-network 2=path/to/network.2.baseline.binpb \
  --candidate-network 2=path/to/network.2.binpb \
  --partition-metadata 2=path/to/network.2.experiment.json \
  --workers 2 --input path/to/network.bundle --input path/to/plans.bundle \
  --output-dir out/partition-pairs --seed 4711 --replanning-workers 1 \
  --binary-source-revision <revision> --build-settings "cargo build --release --locked" \
  --build-toolchain "rustc --version --verbose output" \
  --workload "scenario name" --demand-provenance "plans plus hourly route sidecar" \
  --population-size 12345 --simulated-duration-seconds 86400 \
  --warmups 1 --runs 3 --max-seconds 600
```

These paths are the outputs of `partition_network --coarse-regions`. If the experiment rejects its candidate, the
selected `network.2.binpb` contains the same layout as `network.2.baseline.binpb`; the pair report marks
`compares_same_layout` as `true` for that fallback.

## Sample release run

The implementation was exercised on `assets/equil` (100 people, 15 nodes, 23 links, 24 simulated hours), with seed
4711, one replanning thread, one warm-up and three measured pairs at one and two QSim workers. The host was Linux on an
AMD Ryzen Threadripper PRO 3955WX with 32 logical CPUs; the binary used Rust 1.94.0 in release mode.

| Workers | Coarse regions | Selection | Preparation | Solver | Process wall | Mobsim phase | Process CPU | Peak RSS |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 4 | Baseline: no estimated-work improvement | 0.278 ms | 0.008 ms | 101 ms | 42 ms | 92 ms | 25,560 KiB |
| 2 | 6 | Baseline: weighted edge cut worsened | 0.455 ms | 0.052 ms | 602 ms | 562 ms | 628 ms | 25,560 KiB |

Preparation and solver values describe the rejected candidate attempt. Since both attempts selected the corrected
weighted-METIS baseline, this run does not measure reassignment speedup. All 16 measured worker/layout/instrumentation
pairs completed; final plan state matched and the event summary reported zero link-volume delta. At two workers,
diagnostics counted 400 migrations and 172,802 received inter-partition messages across 24 hours. Summed worker wait
time can exceed wall time because it adds concurrent workers. Diagnostics-on CPU varied from 0.99 to 1.14 times the
off runs; elapsed time was effectively unchanged at the process timer's resolution. These fixture results do not support
population-scale claims.

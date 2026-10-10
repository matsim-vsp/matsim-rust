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
`network.8.experiment.json`. The metadata records the input and source revisions, baseline and candidate layout IDs,
solver revision and limits, seed, workload definitions, preparation durations, each worker's estimated loads and
neighbor counts, structural checks, and rejection reasons. A rejected candidate selects the baseline.

`convert_to_binary` writes an hourly workload sidecar from selected-plan routes and departure times. The solver
balances total precomputed route-link occurrences, memory estimated as 64 bytes per node, and one route-link
occurrence count per node for each supplied hour. Legs without departure times are counted in the sidecar and omitted
from hourly dimensions. The memory value is a fixed estimate, not a measurement. Hourly counts are input-based proxies,
not observed per-tick QSim work.

The command accepts a candidate only when its maximum estimated total work improves, every estimated load including
hourly work stays within baseline maxima, its weighted edge cut and maximum neighbor count do not increase, and every
required contiguous partition remains connected. Use `local_qsim --partition-diagnostics path.csv` to record per-worker
work duration, synchronization wait, link work/transitions, migrations, and received inter-partition messages in hourly
simulated-time windows. The command's preparation durations do not measure QSim runtime. Diagnostics do not measure
actual memory use or establish simulation compatibility; compare saved events, final population state, and routing
responses for matched runs before making a compatibility claim.

Run matched repeated release sweeps with `scripts/partition_pair_experiment.py`. Supply one baseline and candidate
network for each worker count, the same scenario config and complete input bundle, and choose at least one warm-up and
three measured repetitions. The runner alternates baseline/candidate order, records sampled elapsed/CPU/RSS and host
provenance, retains failed runs, compares event summaries and canonicalized final-plan state, and repeats each pair
with diagnostics disabled and enabled to expose instrumentation cost. Enabled runs also record controller phase
timings. These measurements describe only the supplied workload; they do not establish population-scale behavior.

```sh
python3 scripts/partition_pair_experiment.py \
  --config path/to/config.yml --binary target/release/local_qsim \
  --baseline-network 2=path/to/network.2.binpb \
  --candidate-network 2=path/to/network.2.candidate.binpb \
  --partition-metadata 2=path/to/network.2.experiment.json \
  --workers 2 --input path/to/network.bundle --input path/to/plans.bundle \
  --output-dir out/partition-pairs --seed 4711 --replanning-workers 1 \
  --binary-source-revision <revision> --build-settings "cargo build --release --locked" \
  --build-toolchain "rustc --version --verbose output" \
  --workload "scenario name" --demand-provenance "plans plus hourly route sidecar" \
  --population-size 12345 --simulated-duration-seconds 86400 \
  --warmups 1 --runs 3 --max-seconds 600
```

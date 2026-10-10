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
  --seed 4711
```

The command writes the selected network to `network.8.binpb` and its comparison metadata to
`network.8.experiment.json`. The metadata records the input and source revisions, baseline and candidate layout IDs,
solver revision and limits, seed, workload definitions, preparation durations, each worker's estimated loads and
neighbor counts, structural checks, and rejection reasons. A rejected candidate selects the baseline.

The solver balances two estimates. Work is the sum of each node's precomputed route-link occurrence count. Memory is
estimated as 64 bytes per node. The memory value is a fixed estimate, not a measurement. The network file has no
simulated-time demand windows, so this path does not balance time-of-day demand.

The command accepts a candidate only when its maximum estimated work improves, its worker loads stay within the
baseline maxima, its weighted edge cut and maximum neighbor count do not increase, and every required contiguous
partition remains connected. These checks validate layout structure. The recorded durations cover preparation and
assignment only. The command does not measure QSim runtime, worker wait, messages, actual memory use, final population
state, events, or routing-service behavior. Do not use this command alone to claim a speedup or simulation
compatibility.

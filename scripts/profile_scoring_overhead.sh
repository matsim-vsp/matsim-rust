#!/usr/bin/env bash
# Profiles the overhead of scoring (backpacking) inside the mobsim. Runs the same scenario with
# `scoring.mode=enabled` and `scoring.mode=disabled` for every given partition count and stores
# the profiles side by side, so that both modes can be compared.
#
# Usage:
#   scripts/profile_scoring_overhead.sh <tool> <config.yml> <partitions...> [-- <extra local_qsim args>]
#
# Example:
#   scripts/profile_scoring_overhead.sh hotpath-alloc matsim_rust/assets/berlin-v6.4/config.yml 1 4
#   REPEAT=3 scripts/profile_scoring_overhead.sh perf-stat berlin-10pct.yml 1 64 192 -- --set qsim.end_time=86400
#
# Tools:
#   time           plain release build, records wall times only (baseline without profiler)
#   hotpath        time per #[hotpath::measure] function (feature `hotpath`)
#   hotpath-alloc  allocations per #[hotpath::measure] function (features `hotpath,hotpath-alloc`)
#   samply         sampling profile, Linux and macOS (https://github.com/mstange/samply)
#   perf           sampling profile with `perf record`, Linux only
#   perf-stat      hardware counters (cache and TLB misses, ...) with `perf stat`, Linux only
#   c2c            cache line contention with `perf c2c`, Linux only (AMD requires IBS support)
#
# Only `hotpath` and `hotpath-alloc` build the instrumented binary. All other tools use an
# uninstrumented release build, so their timings are not distorted by the instrumentation.
#
# Environment variables:
#   OUT_DIR         result directory (default: matsim_rust/output/profiling/<timestamp>-<tool>)
#   REPEAT          repetitions per partition count and scoring mode (default: 1)
#   SCORING_MODES   scoring modes to run (default: "enabled disabled")
#   PERF_FREQ       sampling frequency for samply/perf/c2c (default: 499)
#   PERF_CALLGRAPH  call graph mode for `perf record`, e.g. "dwarf,16384" (default: none, flat profile)
#   PERF_EVENTS     events for `perf stat` (default: see below)
#   ALLOC_METRIC    `count` or `bytes` for hotpath-alloc (default: count)
#   BIN             use this local_qsim binary instead of building one
#
# Every run uses one iteration and writes no events, unless the extra arguments override this.
# Later `--set` arguments win, so extra arguments can override all defaults below.
# The summary file `summary.tsv` contains the exit code, the wall time, the time of the mobsim
# phase, and the longest simulation loop of all partitions (both taken from the log).

set -euo pipefail

usage() {
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
}

[ $# -ge 3 ] || usage

TOOL=$1
CONFIG=$2
shift 2

PARTITIONS=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do
    PARTITIONS+=("$1")
    shift
done
[ $# -gt 0 ] && shift
EXTRA_ARGS=("$@")
[ ${#PARTITIONS[@]} -gt 0 ] || usage

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG="$(cd "$(dirname "$CONFIG")" && pwd)/$(basename "$CONFIG")"
OUT_DIR="${OUT_DIR:-$ROOT/matsim_rust/output/profiling/$(date +%Y%m%d-%H%M%S)-$TOOL}"
REPEAT="${REPEAT:-1}"
SCORING_MODES="${SCORING_MODES:-enabled disabled}"
PERF_FREQ="${PERF_FREQ:-499}"
PERF_CALLGRAPH="${PERF_CALLGRAPH:-}"
PERF_EVENTS="${PERF_EVENTS:-task-clock,cycles,instructions,branch-misses,cache-references,cache-misses,L1-dcache-load-misses,dTLB-load-misses}"
ALLOC_METRIC="${ALLOC_METRIC:-count}"

case "$TOOL" in
    time | samply | perf | perf-stat | c2c) FEATURES="" ;;
    hotpath) FEATURES="hotpath" ;;
    hotpath-alloc) FEATURES="hotpath,hotpath-alloc" ;;
    *) echo "Unknown tool: $TOOL" >&2; usage ;;
esac

require() {
    command -v "$1" > /dev/null || { echo "$1 is required for tool $TOOL but was not found." >&2; exit 1; }
}
case "$TOOL" in
    samply) require samply ;;
    perf | perf-stat | c2c) require perf ;;
esac

mkdir -p "$OUT_DIR"

# Copy the binary into the result directory. Later builds of other variants then can't replace it
# during a sweep, and perf/samply can still resolve symbols when the results are analyzed later.
if [ -n "${BIN:-}" ]; then
    cp "$BIN" "$OUT_DIR/local_qsim"
else
    (
        cd "$ROOT"
        cargo build --release -p matsim-rust --bin local_qsim ${FEATURES:+--features "$FEATURES"}
    )
    cp "${CARGO_TARGET_DIR:-$ROOT/target}/release/local_qsim" "$OUT_DIR/local_qsim"
fi
BIN="$OUT_DIR/local_qsim"

{
    echo "tool: $TOOL"
    echo "config: $CONFIG"
    echo "partitions: ${PARTITIONS[*]}"
    echo "scoring modes: $SCORING_MODES"
    echo "repeat: $REPEAT"
    echo "extra args: ${EXTRA_ARGS[*]:-}"
    echo "features: ${FEATURES:-none}"
    echo "git: $(git -C "$ROOT" rev-parse HEAD 2> /dev/null || echo unknown)$(git -C "$ROOT" diff --quiet 2> /dev/null || echo ' (dirty)')"
    echo "host: $(uname -a)"
    echo "date: $(date)"
} > "$OUT_DIR/meta.txt"

now() {
    perl -MTime::HiRes=time -e 'printf "%.3f\n", time'
}

# Prints the busy time of the last closed span matching the pattern in seconds. If several spans
# match (one per partition), the longest one is printed.
span_seconds() {
    SPAN_PATTERN="$2" perl -ne '
        BEGIN { %unit = (ns => 1e-9, us => 1e-6, ms => 1e-3, s => 1) }
        s/\e\[[0-9;]*m//g;
        next unless /$ENV{SPAN_PATTERN}/ && /close time\.busy=([0-9.]+)(\S+)/;
        # tracing prints microseconds as "µs"; every unit that is not listed above is one.
        $v = $1 * ($unit{$2} // 1e-6);
        $max = $v if !defined $max || $v > $max;
        END { print defined $max ? sprintf("%.3f\n", $max) : "NA\n" }
    ' "$1"
}

qsim_comms() {
    local p=$1 comms="" i
    for ((i = 0; i < p; i++)); do
        comms="${comms:+$comms,}qsim-$i"
    done
    echo "$comms"
}

printf "tool\tpartitions\tscoring\trepetition\texit\twall_s\tmobsim_phase_s\tmax_sim_loop_s\n" > "$OUT_DIR/summary.tsv"

for parts in "${PARTITIONS[@]}"; do
    for mode in $SCORING_MODES; do
        for ((rep = 1; rep <= REPEAT; rep++)); do
            name="p${parts}-scoring-${mode}-r${rep}"
            run_dir="$OUT_DIR/$name"
            mkdir -p "$run_dir"
            echo "Running $name"

            sim=("$BIN" --config "$CONFIG"
                --set "partitioning.num_parts=$parts"
                --set "scoring.mode=$mode"
                --set "output.output_dir=$run_dir/output"
                --set "output.write_events=none"
                --set "controller.first_iteration=0"
                --set "controller.last_iteration=0"
                --set "scoring.write_experienced_plans=false"
                ${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"})

            case "$TOOL" in
                time) cmd=("${sim[@]}") ;;
                hotpath)
                    export HOTPATH_METRICS_SERVER_OFF=true HOTPATH_REPORT=functions-timing \
                        HOTPATH_FUNCTIONS_LIMIT=100 HOTPATH_OUTPUT_PATH="$run_dir/hotpath.txt"
                    cmd=("${sim[@]}")
                    ;;
                hotpath-alloc)
                    export HOTPATH_METRICS_SERVER_OFF=true HOTPATH_REPORT=functions-alloc \
                        HOTPATH_ALLOC_METRIC="$ALLOC_METRIC" HOTPATH_FUNCTIONS_LIMIT=100 \
                        HOTPATH_OUTPUT_PATH="$run_dir/hotpath-alloc-$ALLOC_METRIC.txt"
                    cmd=("${sim[@]}")
                    ;;
                samply) cmd=(samply record --save-only --rate "$PERF_FREQ" --output "$run_dir/profile.json.gz" -- "${sim[@]}") ;;
                perf) cmd=(perf record -F "$PERF_FREQ" ${PERF_CALLGRAPH:+--call-graph "$PERF_CALLGRAPH"} -o "$run_dir/perf.data" -- "${sim[@]}") ;;
                perf-stat) cmd=(perf stat -x, -o "$run_dir/perf-stat.csv" -e "$PERF_EVENTS" -- "${sim[@]}") ;;
                c2c) cmd=(perf c2c record -F "$PERF_FREQ" -o "$run_dir/c2c.data" -- "${sim[@]}") ;;
            esac

            start=$(now)
            set +e
            "${cmd[@]}" > "$run_dir/log.txt" 2>&1
            status=$?
            set -e
            wall=$(perl -e "printf '%.3f', $(now) - $start")
            if [ $status -ne 0 ]; then
                echo "  exited with $status, see $run_dir/log.txt" >&2
            fi

            case "$TOOL" in
                perf)
                    # Self time per symbol, once restricted to the mobsim worker threads (qsim-<rank>).
                    perf report -i "$run_dir/perf.data" --no-children --sort dso,sym --stdio \
                        --comms "$(qsim_comms "$parts")" --percentage relative \
                        > "$run_dir/perf-report-mobsim.txt" 2> /dev/null || true
                    perf report -i "$run_dir/perf.data" --no-children --sort comm,dso,sym --stdio \
                        > "$run_dir/perf-report-all.txt" 2> /dev/null || true
                    ;;
                c2c)
                    perf c2c report -i "$run_dir/c2c.data" --stdio > "$run_dir/c2c-report.txt" 2> /dev/null || true
                    ;;
            esac

            printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$TOOL" "$parts" "$mode" "$rep" "$status" "$wall" \
                "$(span_seconds "$run_dir/log.txt" 'run_mobsim_phase\{')" \
                "$(span_seconds "$run_dir/log.txt" ' run\{rank=')" >> "$OUT_DIR/summary.tsv"
        done
    done
done

echo "Results in $OUT_DIR"
column -t -s $'\t' "$OUT_DIR/summary.tsv" 2> /dev/null || cat "$OUT_DIR/summary.tsv"

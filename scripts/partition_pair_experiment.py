#!/usr/bin/env python3
"""Run paired release QSim sweeps for baseline and candidate partition networks."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import statistics
import subprocess
import sys
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from pathlib import Path

from routing_experiment import hash_input, run_one


def parse_networks(values: list[str]) -> dict[int, Path]:
    networks: dict[int, Path] = {}
    for value in values:
        workers_text, separator, path_text = value.partition("=")
        if not separator or not path_text:
            raise ValueError(f"expected WORKERS=PATH, got {value!r}")
        workers = int(workers_text)
        if workers <= 0 or workers in networks:
            raise ValueError(f"worker count must be positive and unique: {workers}")
        networks[workers] = Path(path_text).resolve()
    return networks


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as content:
        for chunk in iter(lambda: content.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def plan_state_sha256(path: Path | None) -> str | None:
    if path is None:
        return None
    xml = subprocess.run(["zstdcat", str(path)], capture_output=True, check=True).stdout.decode()
    canonical = ET.canonicalize(xml_data=xml, strip_text=True)
    return hashlib.sha256(canonical.encode()).hexdigest()


def summarize_runs(runs: list[dict]) -> dict:
    groups: dict[tuple[int, str, bool], list[dict]] = {}
    for run in runs:
        if run["warmup"] or run["exit_code"] != 0 or run["timed_out"] or run["rss_limit_reached"]:
            continue
        key = (run["worker_count"], run["variant"], run["diagnostics_enabled"])
        groups.setdefault(key, []).append(run)
    summary = {}
    for (workers, variant, diagnostics), samples in sorted(groups.items()):
        summary[f"workers-{workers}/{variant}/diagnostics-{str(diagnostics).lower()}"] = {
            "completed_runs": len(samples),
            "median_elapsed_seconds": statistics.median(run["elapsed_seconds"] for run in samples),
            "median_sampled_cpu_seconds": statistics.median(run["cpu_seconds_sampled"] for run in samples),
            "median_sampled_peak_rss_kib": statistics.median(run["peak_rss_kib_sampled"] for run in samples),
        }
    return summary


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--baseline-network", action="append", required=True, metavar="WORKERS=PATH")
    parser.add_argument("--candidate-network", action="append", required=True, metavar="WORKERS=PATH")
    parser.add_argument("--workers", required=True, nargs="+", type=int)
    parser.add_argument("--input", required=True, action="append", type=Path)
    parser.add_argument("--set", action="append", default=[], metavar="KEY=VALUE")
    parser.add_argument("--partition-metadata", required=True, action="append", metavar="WORKERS=PATH")
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--seed", required=True, type=int)
    parser.add_argument("--replanning-workers", type=int, default=1)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--max-seconds", type=float, required=True)
    parser.add_argument("--max-rss-kib", type=int)
    parser.add_argument("--binary-source-revision", required=True)
    parser.add_argument("--build-settings", required=True, help="release build command, RUSTFLAGS, and features")
    parser.add_argument("--build-toolchain", required=True, help="Rust toolchain used to build the binary")
    parser.add_argument("--workload", required=True, help="scenario or demand description")
    parser.add_argument("--demand-provenance", required=True, help="input population and demand source")
    parser.add_argument("--population-size", required=True, type=int)
    parser.add_argument("--simulated-duration-seconds", required=True, type=int)
    parser.add_argument("--sample-size", type=float)
    args = parser.parse_args()

    try:
        baseline = parse_networks(args.baseline_network)
        candidate = parse_networks(args.candidate_network)
        partition_metadata = parse_networks(args.partition_metadata)
    except (ValueError, OSError) as error:
        parser.error(str(error))
    if args.runs < 1 or args.warmups < 0 or args.max_seconds <= 0:
        parser.error("runs must be positive, warmups non-negative, and max seconds positive")
    if args.replanning_workers <= 0 or args.seed < 0 or args.population_size <= 0 or args.simulated_duration_seconds <= 0:
        parser.error("worker counts and population/duration must be positive, and seed non-negative")
    if args.max_rss_kib is not None and args.max_rss_kib <= 0:
        parser.error("max RSS must be positive")
    if len(set(args.workers)) != len(args.workers) or any(worker <= 0 for worker in args.workers):
        parser.error("worker counts must be positive and unique")
    if set(args.workers) - baseline.keys() or set(args.workers) - candidate.keys() or set(args.workers) - partition_metadata.keys():
        parser.error("provide baseline, candidate, and experiment metadata for every worker count")
    config = args.config.resolve()
    binary = args.binary.resolve()
    if not config.is_file() or not binary.is_file():
        parser.error("config and binary must exist")
    for path in [*baseline.values(), *candidate.values(), *partition_metadata.values()]:
        if not path.is_file():
            parser.error(f"partition network does not exist: {path}")
    if not Path("/proc/self/status").exists():
        parser.error("resource sampling requires Linux /proc")

    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    source_root = Path(__file__).resolve().parent.parent
    compare_script = Path(__file__).with_name("compare_experiment_events.py")
    input_hashes = [hash_input(path.resolve()) for path in args.input]
    run_records = []
    comparisons = []
    pair_index = 0
    for workers in args.workers:
        for repetition in range(-args.warmups, args.runs):
            warmup = repetition < 0
            pair_id = f"workers-{workers}-run-{repetition}"
            results = {}
            order = ["baseline", "candidate"]
            if pair_index % 2:
                order.reverse()
            pair_index += 1
            for diagnostic_mode in (False, True):
                mode_key = "diagnostics_on" if diagnostic_mode else "diagnostics_off"
                mode_results = {}
                for variant in order:
                    network = baseline[workers] if variant == "baseline" else candidate[workers]
                    run_dir = output / f"{pair_id}-{mode_key}-{variant}"
                    diagnostics_path = run_dir / "partition_diagnostics.csv"
                    overrides = [
                        f"network.path={network}",
                        f"partitioning.num_parts={workers}",
                        f"computational_setup.replanning_threads={args.replanning_workers}",
                        f"computational_setup.random_seed={args.seed}",
                        f"output.output_dir={run_dir}",
                        *args.set,
                    ]
                    command = [str(binary), "--config", str(config)]
                    for override in overrides:
                        command.extend(("--set", override))
                    if diagnostic_mode:
                        command.extend(("--partition-diagnostics", str(diagnostics_path)))
                    result = run_one(
                        command,
                        output / f"{pair_id}-{mode_key}-{variant}.log",
                        args.max_seconds,
                        args.max_rss_kib,
                    )
                    result.update(
                        {
                            "pair_id": pair_id,
                            "worker_count": workers,
                            "repetition": repetition,
                            "warmup": warmup,
                            "variant": variant,
                            "diagnostics_enabled": diagnostic_mode,
                            "network": str(network),
                            "network_sha256": sha256(network),
                            "output_dir": str(run_dir),
                            "command": command,
                            "diagnostics_csv": str(diagnostics_path) if diagnostic_mode else None,
                            "controller_metrics": (
                                json.loads(diagnostics_path.with_suffix(".controller.json").read_text(encoding="utf-8"))
                                if diagnostic_mode and diagnostics_path.with_suffix(".controller.json").is_file()
                                else None
                            ),
                        }
                    )
                    mode_results[variant] = result
                    run_records.append(result)
                results[mode_key] = mode_results
            for mode_key, mode_results in results.items():
                left, right = mode_results["baseline"], mode_results["candidate"]
                comparison = {
                    "pair_id": pair_id,
                    "instrumentation": mode_key,
                    "baseline_exit_code": left["exit_code"],
                    "candidate_exit_code": right["exit_code"],
                }
                if left["exit_code"] == 0 and right["exit_code"] == 0:
                    comparison["candidate_to_baseline_elapsed_ratio"] = (
                        round(right["elapsed_seconds"] / left["elapsed_seconds"], 6)
                        if left["elapsed_seconds"] else None
                    )
                    comparison["candidate_to_baseline_cpu_ratio"] = (
                        round(right["cpu_seconds_sampled"] / left["cpu_seconds_sampled"], 6)
                        if left["cpu_seconds_sampled"] else None
                    )
                    baseline_output = Path(left["output_dir"])
                    candidate_output = Path(right["output_dir"])
                    event_comparison = subprocess.run(
                        [sys.executable, str(compare_script), str(baseline_output), str(candidate_output)],
                        capture_output=True,
                        text=True,
                        check=False,
                    )
                    comparison["event_comparison_exit_code"] = event_comparison.returncode
                    comparison["event_comparison"] = (
                        json.loads(event_comparison.stdout) if event_comparison.returncode == 0 else None
                    )
                    comparison["event_comparison_error"] = event_comparison.stderr or None
                    plan_files = [
                        next(iter(sorted(path for path in directory.glob("output_plans.xml*") if path.is_file())), None)
                        for directory in (baseline_output, candidate_output)
                    ]
                    comparison["final_population_sha256"] = [plan_state_sha256(path) for path in plan_files]
                    comparison["final_population_match"] = (
                        comparison["final_population_sha256"][0] is not None
                        and comparison["final_population_sha256"][0]
                        == comparison["final_population_sha256"][1]
                    )
                comparisons.append(comparison)

    run_summary = summarize_runs(run_records)
    instrumentation_overhead = {}
    groups = {key.rsplit("/diagnostics-", 1)[0] for key in run_summary}
    for group in sorted(groups):
        enabled = run_summary.get(f"{group}/diagnostics-true")
        disabled = run_summary.get(f"{group}/diagnostics-false")
        instrumentation_overhead[group] = {
            "elapsed_ratio_on_vs_off": round(
                enabled["median_elapsed_seconds"] / disabled["median_elapsed_seconds"], 6
            ) if enabled and disabled and disabled["median_elapsed_seconds"] else None,
        }
    report = {
        "schema_version": 1,
        "started_at_utc": datetime.now(timezone.utc).isoformat(),
        "seed": args.seed,
        "workload": args.workload,
        "demand_provenance": args.demand_provenance,
        "population_size": args.population_size,
        "simulated_duration_seconds": args.simulated_duration_seconds,
        "sample_size": args.sample_size,
        "worker_counts": args.workers,
        "runs_per_variant": args.runs,
        "warmups_per_variant": args.warmups,
        "config": str(config),
        "config_sha256": sha256(config),
        "binary": str(binary),
        "binary_sha256": sha256(binary),
        "binary_source_revision": args.binary_source_revision,
        "build": {"settings": args.build_settings, "toolchain": args.build_toolchain},
        "inputs": input_hashes,
        "partition_networks": {
            str(workers): {
                "baseline": {"path": str(baseline[workers]), "sha256": sha256(baseline[workers])},
                "candidate": {"path": str(candidate[workers]), "sha256": sha256(candidate[workers])},
            }
            for workers in args.workers
        },
        "partition_metadata": {
            str(workers): {
                "path": str(partition_metadata[workers]),
                "sha256": sha256(partition_metadata[workers]),
                "contents": json.loads(partition_metadata[workers].read_text(encoding="utf-8")),
            }
            for workers in args.workers
        },
        "hardware": {
            "platform": platform.platform(),
            "processor": platform.processor() or None,
            "logical_cpus": os.cpu_count(),
            "hostname": platform.node(),
        },
        "resource_sampling": "Linux /proc sampled every 100ms; short peaks may be missed",
        "runs": run_records,
        "comparisons": comparisons,
        "summary": run_summary,
        "instrumentation_overhead": instrumentation_overhead,
        "runner_revision": subprocess.run(
            ["git", "-C", str(source_root), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=False,
        ).stdout.strip() or None,
    }
    report_path = output / "partition-pairs.json"
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(report_path)
    failed_run = any(record["exit_code"] != 0 for record in run_records if not record["warmup"])
    failed_comparison = any(
        comparison.get("event_comparison_exit_code") != 0
        or comparison.get("final_population_match") is False
        for comparison in comparisons
    )
    return 1 if failed_run or failed_comparison else 0


if __name__ == "__main__":
    sys.exit(main())

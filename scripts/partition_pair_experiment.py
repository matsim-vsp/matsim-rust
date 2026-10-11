#!/usr/bin/env python3
"""Run paired release QSim sweeps for baseline and candidate partition networks."""

from __future__ import annotations

import argparse
import csv
import hashlib
import math
import json
import os
import platform
import re
import statistics
import subprocess
import sys
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from pathlib import Path

from routing_experiment import hash_input, run_one
from compare_experiment_events import compare_summaries, summarize as summarize_events


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


def fnv1a64(path: Path) -> str:
    value = 0xCBF29CE484222325
    with path.open("rb") as content:
        for chunk in iter(lambda: content.read(1024 * 1024), b""):
            for byte in chunk:
                value = ((value ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{value:016x}"


def validate_partition_metadata(metadata: dict, workers: int, baseline: Path, candidate: Path) -> None:
    if metadata.get("worker_count") != workers:
        raise ValueError(f"partition metadata worker_count does not match {workers} workers")
    if (
        not isinstance(metadata.get("baseline_network_fnv64"), str)
        or fnv1a64(baseline) != metadata["baseline_network_fnv64"]
    ):
        raise ValueError(f"baseline network does not match corrected weighted-METIS metadata for {workers} workers")
    if (
        not isinstance(metadata.get("selected_network_fnv64"), str)
        or fnv1a64(candidate) != metadata["selected_network_fnv64"]
    ):
        raise ValueError(f"candidate network does not match selected layout metadata for {workers} workers")


def compares_same_layout(metadata: dict) -> bool:
    return metadata.get("baseline_layout_id") == metadata.get("selected_layout_id")


def diagnostics_order(pair_index: int) -> tuple[bool, bool]:
    return (True, False) if (pair_index // 2) % 2 else (False, True)


def plan_state_sha256(path: Path | None) -> str | None:
    if path is None:
        return None
    xml = subprocess.run(["zstdcat", str(path)], capture_output=True, check=True).stdout.decode()
    root = ET.fromstring(xml)
    people = sorted((person for person in root if person.tag == "person"), key=lambda person: person.get("id", ""))
    people_iter = iter(people)
    for index, child in enumerate(root):
        if child.tag == "person":
            root[index] = next(people_iter)
    xml = ET.tostring(root, encoding="unicode")
    canonical = ET.canonicalize(xml_data=xml, strip_text=True)
    return hashlib.sha256(canonical.encode()).hexdigest()


def run_outcome_signature(run: dict) -> tuple[dict, dict]:
    output = Path(run["output_dir"])
    plan = next(iter(sorted(path for path in output.glob("output_plans.xml*") if path.is_file())), None)
    event_summary = summarize_events(output / "events")
    signature = {
        "events": event_summary["canonical_events_sha256"],
        "final_population": plan_state_sha256(plan),
    }
    if run["diagnostics_enabled"]:
        with Path(run["diagnostics_csv"]).open(encoding="utf-8", newline="") as diagnostics_file:
            rows = csv.DictReader(diagnostics_file)
            counts = [
                [row[key] for key in (
                    "iteration",
                    "worker",
                    "time_window_start_seconds",
                    "active_network_work",
                    "vehicle_transitions",
                    "migrations",
                    "inter_partition_messages",
                )]
                for row in rows
            ]
        signature["diagnostic_counts"] = hashlib.sha256(
            json.dumps(counts, separators=(",", ":")).encode()
        ).hexdigest()
    return signature, event_summary


def compare_run_signatures(left: dict | None, right: dict | None, compare_counts: bool) -> dict:
    result = {
        "events_match": left is not None and right is not None and left["events"] == right["events"],
        "final_population_match": (
            left is not None
            and right is not None
            and left["final_population"] is not None
            and left["final_population"] == right["final_population"]
        ),
    }
    if compare_counts:
        result["diagnostic_counts_match"] = (
            left is not None
            and right is not None
            and left.get("diagnostic_counts") == right.get("diagnostic_counts")
        )
    result["matches"] = all(result.values())
    return result


def valid_timeout(value: float) -> bool:
    return math.isfinite(value) and value > 0


def require_no_metis_partitioning(config: Path, overrides: list[str]) -> None:
    text = config.read_text(encoding="utf-8")
    text = re.sub(
        r"(?<!\\)\\u([0-9a-fA-F]{4})|(?<!\\)\\U([0-9a-fA-F]{8})|(?<!\\)\\x([0-9a-fA-F]{2})",
        lambda match: chr(int(next(group for group in match.groups() if group), 16)),
        text,
    )
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if line.rstrip().endswith("\\") or line.lstrip().startswith("?"):
            raise ValueError("pair sweeps require simple block-style YAML keys")
        content = line.split("#", 1)[0].strip()
        if "partitioning" not in content:
            continue
        match = re.match(r"^(\s*)partitioning:\s*(?:#.*)?$", line)
        if not match:
            raise ValueError("pair sweeps require block-style partitioning.method: none configuration")
        indent = len(match.group(1))
        for nested in lines[index + 1 :]:
            nested_content = nested.split("#", 1)[0].strip()
            if not nested_content:
                continue
            if len(nested) - len(nested.lstrip()) <= indent:
                break
            method_line = nested[: len(nested) - len(nested.lstrip())] + nested_content
            method = re.match(r"\s+method:\s*(\S+)\s*$", method_line)
            if "method" in nested_content and not method:
                raise ValueError("pair sweeps require a plain block-style method: None setting")
            if method and method.group(1) != "None":
                raise ValueError("pair sweeps require partitioning.method: none; supplied layouts must remain unchanged")
    reserved = {
        "network.path",
        "population.path",
        "vehicles.path",
        "transit.schedule_path",
        "facilities.path",
        "ids.path",
        "partitioning.method",
        "partitioning.num_parts",
        "computational_setup.replanning_threads",
        "computational_setup.random_seed",
        "output.output_dir",
    }
    for override in overrides:
        key, separator, _value = override.partition("=")
        if not separator:
            continue
        if key.strip() in reserved:
            raise ValueError(f"--set cannot override runner-controlled setting {key.strip()}")


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
            "median_cpu_seconds": statistics.median(run["cpu_seconds"] for run in samples),
            "median_peak_rss_kib": statistics.median(run["peak_rss_kib"] for run in samples),
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
    if args.runs < 1 or args.warmups < 0 or not valid_timeout(args.max_seconds):
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
    try:
        require_no_metis_partitioning(config, args.set)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    for path in [*baseline.values(), *candidate.values(), *partition_metadata.values()]:
        if not path.is_file():
            parser.error(f"partition network does not exist: {path}")
    try:
        metadata_by_workers = {
            workers: json.loads(path.read_text(encoding="utf-8"))
            for workers, path in partition_metadata.items()
        }
    except (OSError, ValueError) as error:
        parser.error(f"invalid partition metadata: {error}")
    try:
        for metadata in metadata_by_workers.values():
            for key in ("baseline_preparation_nanos", "total_preparation_nanos"):
                if not isinstance(metadata[key], int) or metadata[key] < 0:
                    raise ValueError(f"{key} must be a non-negative integer")
        for workers in args.workers:
            validate_partition_metadata(
                metadata_by_workers[workers], workers, baseline[workers], candidate[workers]
            )
    except (KeyError, ValueError, TypeError) as error:
        parser.error(f"invalid partition preparation timing metadata: {error}")
    if not Path("/proc/self/status").exists():
        parser.error("resource sampling requires Linux /proc")

    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    source_root = Path(__file__).resolve().parent.parent
    input_hashes = [hash_input(path.resolve()) for path in args.input]
    run_records = []
    comparisons = []
    pair_index = 0
    for workers in args.workers:
        for repetition in range(-args.warmups, args.runs):
            warmup = repetition < 0
            pair_id = f"workers-{workers}-run-{repetition}"
            results = {}
            reverse_pair = pair_index % 2 == 1
            order = ["baseline", "candidate"]
            if reverse_pair:
                order.reverse()
            diagnostic_modes = diagnostics_order(pair_index)
            pair_index += 1
            for diagnostic_mode in diagnostic_modes:
                mode_key = "diagnostics_on" if diagnostic_mode else "diagnostics_off"
                mode_results = {}
                for variant in order:
                    network = baseline[workers] if variant == "baseline" else candidate[workers]
                    run_dir = output / f"{pair_id}-{mode_key}-{variant}"
                    diagnostics_path = run_dir / "partition_diagnostics.csv"
                    overrides = [
                        *args.set,
                        f"network.path={network}",
                        f"partitioning.num_parts={workers}",
                        f"computational_setup.replanning_threads={args.replanning_workers}",
                        f"computational_setup.random_seed={args.seed}",
                        f"output.output_dir={run_dir}",
                        "partitioning.method=None",
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
                    controller_metrics = None
                    diagnostics_parse_error = None
                    controller_metrics_path = diagnostics_path.with_suffix(".controller.json")
                    if diagnostic_mode and controller_metrics_path.is_file():
                        try:
                            controller_metrics = json.loads(controller_metrics_path.read_text(encoding="utf-8"))
                        except (OSError, ValueError) as error:
                            diagnostics_parse_error = str(error)
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
                            "controller_metrics": controller_metrics,
                            "diagnostics_parse_error": diagnostics_parse_error,
                        }
                    )
                    if result["exit_code"] == 0:
                        try:
                            result["outcome_signature"], result["_event_summary"] = run_outcome_signature(result)
                        except (OSError, RuntimeError, ValueError, ET.ParseError, subprocess.SubprocessError) as error:
                            result["outcome_signature_error"] = str(error)
                            result["outcome_signature"] = None
                    mode_results[variant] = result
                    run_records.append(result)
                results[mode_key] = mode_results
            for mode_key, mode_results in results.items():
                left, right = mode_results["baseline"], mode_results["candidate"]
                comparison = {
                    "pair_id": pair_id,
                    "instrumentation": mode_key,
                    "compares_same_layout": compares_same_layout(metadata_by_workers[workers]),
                    "baseline_exit_code": left["exit_code"],
                    "candidate_exit_code": right["exit_code"],
                }
                if left["exit_code"] == 0 and right["exit_code"] == 0:
                    comparison["candidate_to_baseline_elapsed_ratio"] = (
                        round(right["elapsed_seconds"] / left["elapsed_seconds"], 6)
                        if left["elapsed_seconds"] else None
                    )
                    comparison["candidate_to_baseline_cpu_ratio"] = (
                        round(right["cpu_seconds"] / left["cpu_seconds"], 6)
                        if left["cpu_seconds"] else None
                    )
                    metadata = metadata_by_workers[workers]
                    baseline_preparation = metadata["baseline_preparation_nanos"] / 1e9
                    candidate_preparation = metadata["total_preparation_nanos"] / 1e9
                    comparison["baseline_end_to_end_seconds"] = left["elapsed_seconds"] + baseline_preparation
                    comparison["candidate_end_to_end_seconds"] = right["elapsed_seconds"] + candidate_preparation
                    comparison["candidate_to_baseline_end_to_end_ratio"] = round(
                        comparison["candidate_end_to_end_seconds"] / comparison["baseline_end_to_end_seconds"], 6
                    ) if comparison["baseline_end_to_end_seconds"] else None
                    baseline_output = Path(left["output_dir"])
                    candidate_output = Path(right["output_dir"])
                    try:
                        comparison["event_comparison_exit_code"] = 0
                        comparison["event_comparison"] = compare_summaries(
                            left["_event_summary"], right["_event_summary"]
                        )
                        comparison["event_comparison_error"] = None
                        comparison["final_population_sha256"] = [
                            left["outcome_signature"]["final_population"],
                            right["outcome_signature"]["final_population"],
                        ]
                        comparison["final_population_match"] = (
                            comparison["final_population_sha256"][0] is not None
                            and comparison["final_population_sha256"][0]
                            == comparison["final_population_sha256"][1]
                        )
                        comparison["events_match"] = (
                            comparison["event_comparison"].get("events_match") is True
                        )
                    except (KeyError, OSError, RuntimeError, ValueError, ET.ParseError, subprocess.SubprocessError) as error:
                        comparison["comparison_error"] = str(error)
                        comparison["events_match"] = False
                        comparison["final_population_match"] = False
                comparisons.append(comparison)
                for run in mode_results.values():
                    run.pop("_event_summary", None)

    run_summary = summarize_runs(run_records)
    measured_runs = [run for run in run_records if not run["warmup"]]
    outcomes = {
        (run["worker_count"], run["repetition"], run["variant"], run["diagnostics_enabled"]): run
        for run in measured_runs
    }
    invariance_checks = []
    for worker in args.workers:
        for variant in ("baseline", "candidate"):
            repetitions = sorted(
                repetition for (count, repetition, name, _enabled) in outcomes
                if count == worker and name == variant
            )
            for repetition in repetitions:
                off = outcomes.get((worker, repetition, variant, False))
                on = outcomes.get((worker, repetition, variant, True))
                invariance_checks.append({
                    "worker_count": worker,
                    "variant": variant,
                    "comparison": f"diagnostics-off-vs-on/run-{repetition}",
                    **compare_run_signatures(
                        off.get("outcome_signature") if off else None,
                        on.get("outcome_signature") if on else None,
                        False,
                    ),
                })
            for enabled in (False, True):
                reference = outcomes.get((worker, repetitions[0], variant, enabled)) if repetitions else None
                for repetition in repetitions[1:]:
                    current = outcomes.get((worker, repetition, variant, enabled))
                    invariance_checks.append({
                        "worker_count": worker,
                        "variant": variant,
                        "comparison": f"repeatability/diagnostics-{str(enabled).lower()}/run-{repetition}",
                        **compare_run_signatures(
                            reference.get("outcome_signature") if reference else None,
                            current.get("outcome_signature") if current else None,
                            enabled,
                        ),
                    })
    instrumentation_overhead = {}
    groups = {key.rsplit("/diagnostics-", 1)[0] for key in run_summary}
    for group in sorted(groups):
        enabled = run_summary.get(f"{group}/diagnostics-true")
        disabled = run_summary.get(f"{group}/diagnostics-false")
        instrumentation_overhead[group] = {
            "elapsed_ratio_on_vs_off": round(
                enabled["median_elapsed_seconds"] / disabled["median_elapsed_seconds"], 6
            ) if enabled and disabled and disabled["median_elapsed_seconds"] else None,
            "cpu_ratio_on_vs_off": round(
                enabled["median_cpu_seconds"] / disabled["median_cpu_seconds"], 6
            ) if enabled and disabled and disabled["median_cpu_seconds"] else None,
            "peak_rss_ratio_on_vs_off": round(
                enabled["median_peak_rss_kib"] / disabled["median_peak_rss_kib"], 6
            ) if enabled and disabled and disabled["median_peak_rss_kib"] else None,
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
                "contents": metadata_by_workers[workers],
            }
            for workers in args.workers
        },
        "hardware": {
            "platform": platform.platform(),
            "processor": platform.processor() or None,
            "logical_cpus": os.cpu_count(),
            "hostname": platform.node(),
        },
        "resource_sampling": "elapsed time measured directly; final child CPU and peak RSS from wait4; /proc sampled every 100ms for live resource limits",
        "runs": run_records,
        "comparisons": comparisons,
        "summary": run_summary,
        "instrumentation_overhead": instrumentation_overhead,
        "invariance_checks": invariance_checks,
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
        comparison.get("events_match") is not True
        or comparison.get("final_population_match") is not True
        for comparison in comparisons
    )
    failed_invariance_check = any(not check["matches"] for check in invariance_checks)
    return 1 if failed_run or failed_comparison or failed_invariance_check else 0


if __name__ == "__main__":
    sys.exit(main())

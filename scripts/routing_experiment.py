#!/usr/bin/env python3
"""Run resource-bounded, repeatable local_qsim experiments on Linux."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import platform
import signal
import statistics
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path


def git_value(root: Path, *args: str) -> str | None:
    result = subprocess.run(
        ["git", "-C", str(root), *args], capture_output=True, text=True, check=False
    )
    return result.stdout.strip() if result.returncode == 0 else None


def peak_rss_kib(pid: int) -> int:
    try:
        with open(f"/proc/{pid}/status", encoding="utf-8") as status:
            for line in status:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1])
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except FileNotFoundError:
        pass
    return 0


def process_cpu_seconds(pid: int) -> float:
    try:
        with open(f"/proc/{pid}/stat", encoding="utf-8") as stat_file:
            fields = stat_file.read().rsplit(")", 1)[1].split()
        clock_ticks = os.sysconf(os.sysconf_names["SC_CLK_TCK"])
        return (int(fields[11]) + int(fields[12])) / clock_ticks
    except (FileNotFoundError, IndexError, ValueError):
        return 0.0


def hash_input(path: Path) -> dict[str, str]:
    digest = hashlib.sha256()
    if path.is_file():
        with path.open("rb") as content:
            for chunk in iter(lambda: content.read(1024 * 1024), b""):
                digest.update(chunk)
    elif path.is_dir():
        children = sorted(path.rglob("*"))
        for child in children:
            if child.is_symlink() and child.is_dir():
                raise ValueError(f"input directory contains unsupported symlink directory: {child}")
        for child in (item for item in children if item.is_file()):
            relative_path = str(child.relative_to(path)).encode()
            file_digest = hashlib.sha256()
            file_size = child.stat().st_size
            digest.update(len(relative_path).to_bytes(8, "big"))
            digest.update(relative_path)
            digest.update(file_size.to_bytes(8, "big"))
            with child.open("rb") as content:
                for chunk in iter(lambda: content.read(1024 * 1024), b""):
                    file_digest.update(chunk)
            digest.update(file_digest.digest())
    else:
        raise ValueError(f"input does not exist: {path}")
    return {"path": str(path), "sha256": digest.hexdigest()}


def routing_profile_summary(output_dir: Path) -> dict[str, int | bool | list[str]]:
    profile_paths = sorted((output_dir / "instrument").glob("routing_process_*.csv"))
    search_count = 0
    nodes_expanded = 0
    for profile_path in profile_paths:
        with profile_path.open(newline="", encoding="utf-8") as profile:
            for row in csv.DictReader(profile):
                search_count += 1
                nodes_expanded += int(row.get("nodes_expanded") or 0)
    return {
        "files": [str(path) for path in profile_paths],
        "search_count": search_count,
        "nodes_expanded": nodes_expanded,
    }


def run_one(command: list[str], log_path: Path, timeout: float, rss_limit_kib: int | None):
    start = time.monotonic()
    peak = 0
    cpu_seconds = 0.0
    timed_out = False
    rss_limited = False
    with log_path.open("wb") as log:
        process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
        try:
            while process.poll() is None:
                peak = max(peak, peak_rss_kib(process.pid))
                cpu_seconds = max(cpu_seconds, process_cpu_seconds(process.pid))
                elapsed = time.monotonic() - start
                if elapsed >= timeout:
                    timed_out = True
                    process.send_signal(signal.SIGTERM)
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                    break
                if rss_limit_kib is not None and peak >= rss_limit_kib:
                    rss_limited = True
                    process.send_signal(signal.SIGTERM)
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                    break
                time.sleep(0.1)
            return_code = process.wait()
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
    return {
        "elapsed_seconds": round(time.monotonic() - start, 3),
        "peak_rss_kib_sampled": peak,
        "cpu_seconds_sampled": round(cpu_seconds, 3),
        "exit_code": return_code,
        "timed_out": timed_out,
        "rss_limit_reached": rss_limited,
        "log": str(log_path),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--input", required=True, action="append", type=Path, help="input file or directory; repeat for the full input bundle")
    parser.add_argument("--binary", type=Path, default=Path("target/release/local_qsim"))
    parser.add_argument("--binary-source-revision", help="source revision used to build --binary; defaults to this checkout's HEAD")
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--workload", required=True, choices=["fixed-plan", "route-active", "adaptive"])
    parser.add_argument("--population-size", required=True, type=int)
    parser.add_argument("--network-nodes", type=int)
    parser.add_argument("--network-links", type=int)
    parser.add_argument("--sample-size", type=float)
    parser.add_argument("--simulated-duration-seconds", type=int)
    parser.add_argument("--iterations", type=int)
    parser.add_argument("--demand-provenance", default="unspecified")
    parser.add_argument("--seed", type=int, required=True)
    parser.add_argument("--qsim-workers", required=True, type=int)
    parser.add_argument("--replanning-workers", required=True, type=int)
    parser.add_argument("--build-profile", choices=["release", "debug", "custom"], default="release")
    parser.add_argument("--build-settings", default="unspecified", help="declared build command, RUSTFLAGS, and relevant feature flags")
    parser.add_argument("--build-toolchain", default="unspecified", help="declared Rust toolchain used to build --binary")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--max-seconds", type=float, required=True)
    parser.add_argument("--max-rss-kib", type=int)
    parser.add_argument(
        "--allow-missing-routing-profile",
        action="store_true",
        help="permit legacy route-active binaries that predate A* CSV profiling",
    )
    parser.add_argument("--set", action="append", default=[], metavar="KEY=VALUE")
    args = parser.parse_args()

    if args.runs < 1 or args.warmups < 0:
        parser.error("--runs must be positive and --warmups cannot be negative")
    if args.max_seconds <= 0 or (args.max_rss_kib is not None and args.max_rss_kib <= 0):
        parser.error("resource ceilings must be positive")
    if args.population_size <= 0 or args.qsim_workers <= 0 or args.replanning_workers <= 0:
        parser.error("population size and worker counts must be positive")

    root = Path(__file__).resolve().parent.parent
    config = args.config.resolve()
    binary = args.binary.resolve()
    if not config.is_file() or not binary.is_file():
        parser.error("--config and --binary must name existing files")
    if not Path("/proc/self/status").exists():
        parser.error("RSS sampling currently requires Linux /proc")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    config_sha = hashlib.sha256(config.read_bytes()).hexdigest()
    try:
        input_hashes = [hash_input(path.resolve()) for path in args.input]
    except ValueError as error:
        parser.error(str(error))
    runner_revision = git_value(root, "rev-parse", "HEAD")
    binary_revision = args.binary_source_revision or runner_revision
    config_overrides = list(args.set)
    config_overrides += [
        f"computational_setup.random_seed={args.seed}",
        f"partitioning.num_parts={args.qsim_workers}",
        f"computational_setup.replanning_threads={args.replanning_workers}",
        f"output.output_dir={args.output_dir.resolve()}/run-{{index}}",
    ]
    base = [str(binary), "--config", str(config)]
    runs = []
    started_at_utc = datetime.now(timezone.utc).isoformat()
    for index in range(-args.warmups, args.runs):
        run_overrides = [item.replace("{index}", str(index)) for item in config_overrides]
        command = base + [argument for override in run_overrides for argument in ("--set", override)]
        result = run_one(
            command,
            args.output_dir / f"run-{index}.log",
            args.max_seconds,
            args.max_rss_kib,
        )
        result.update({"index": index, "warmup": index < 0, "command": command})
        if args.workload in {"route-active", "adaptive"} and result["exit_code"] == 0:
            run_output_dir = args.output_dir.resolve() / f"run-{index}"
            route_profile = routing_profile_summary(run_output_dir)
            route_profile["verified"] = route_profile["search_count"] > 0
            result["routing_profile"] = route_profile
            if not route_profile["verified"] and args.allow_missing_routing_profile:
                route_profile["legacy_profile_unavailable"] = True
            elif not route_profile["verified"]:
                result["route_validation_error"] = (
                    "No A* search rows found. Enable CSV routing profiling and confirm the workload routes."
                )
        runs.append(result)
        if (
            result["timed_out"]
            or result["rss_limit_reached"]
            or result["exit_code"] != 0
            or result.get("route_validation_error")
        ):
            break

    measured = [run for run in runs if not run["warmup"]]
    completed = [run for run in measured if run["exit_code"] == 0 and not run["timed_out"] and not run["rss_limit_reached"]]
    report = {
        "schema_version": 1,
        "started_at_utc": started_at_utc,
        "workload": args.workload,
        "population_size": args.population_size,
        "network_size": {"nodes": args.network_nodes, "links": args.network_links},
        "sample_size": args.sample_size,
        "simulated_duration_seconds": args.simulated_duration_seconds,
        "iterations": args.iterations,
        "demand_provenance": args.demand_provenance,
        "seed": args.seed,
        "experiment_runner_revision": runner_revision,
        "binary_source_revision": binary_revision,
        "binary_source_revision_declared": args.binary_source_revision is not None,
        "source_dirty": bool(git_value(root, "status", "--short")),
        "binary": str(binary),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "config": str(config),
        "config_sha256": config_sha,
        "inputs": input_hashes,
        "build": {
            "declared_profile": args.build_profile,
            "declared_settings": args.build_settings,
            "declared_toolchain": args.build_toolchain,
            "runner_environment_rustc_version": subprocess.run(
                ["rustc", "--version", "--verbose"],
                capture_output=True,
                text=True,
                check=False,
            ).stdout.strip()
            or None,
        },
        "config_overrides": config_overrides,
        "qsim_workers": args.qsim_workers,
        "replanning_workers": args.replanning_workers,
        "output_settings": {"output_dir": str(args.output_dir.resolve())},
        "resource_limits": {
            "max_elapsed_seconds_per_run": args.max_seconds,
            "max_rss_kib": args.max_rss_kib,
        },
        "hardware": {
            "platform": platform.platform(),
            "processor": platform.processor() or None,
            "logical_cpus": os.cpu_count(),
            "hostname": platform.node(),
        },
        "runs": runs,
        "summary": {
            "completed_runs": len(completed),
            "median_elapsed_seconds": statistics.median([r["elapsed_seconds"] for r in completed]) if completed else None,
            "median_sampled_peak_rss_kib": statistics.median([r["peak_rss_kib_sampled"] for r in completed]) if completed else None,
            "median_sampled_cpu_seconds": statistics.median([r["cpu_seconds_sampled"] for r in completed]) if completed else None,
        },
        "limitations": [
            "RSS is sampled from /proc at 100 ms intervals and can miss short-lived peaks.",
            "CPU time is sampled from /proc at 100 ms intervals and can miss the final fraction of a second.",
            "Phase timings and routing counters must be read from simulation profiling outputs.",
            "Workload metadata is supplied by the operator and should be verified against inputs and config.",
        ],
    }
    report_path = args.output_dir / "experiment.json"
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(report_path)
    return 1 if any(
        r["exit_code"] != 0
        or r["timed_out"]
        or r["rss_limit_reached"]
        or r.get("route_validation_error")
        for r in runs
    ) else 0


if __name__ == "__main__":
    sys.exit(main())

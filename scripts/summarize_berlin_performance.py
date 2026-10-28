#!/usr/bin/env python3
"""Compare the ignored Berlin benchmark's summaries, without any simulation I/O."""
import pathlib
import re
import sys


def summaries(path):
    rows = {}
    for match in re.finditer(r"BERLIN_SUMMARY ([^\r\n]+)", path.read_text()):
        fields = dict(item.split("=", 1) for item in match[1].split())
        rows[int(fields["parts"])] = fields
    if set(rows) != {1, 2}:
        raise ValueError(f"Missing one- or two-partition summary in {path}")
    return rows


def main(directory):
    labels = [("Queue before", "queue-before.log"), ("Queue after", "queue-after.log"),
              ("Kinematic Waves after", "kinematic-after.log")]
    results = {label: summaries(directory / file) for label, file in labels}
    print("# Berlin v6.4, 0.1%: same-runner performance comparison\n")
    print("One warmup and three fresh measured runs per row; release builds, no event files. "
          "Completed trips count PersonArrival events of all modes; link entries count LinkEnter events.\n")
    print("| Source / dynamics | Partitions | Median (s) | Min (s) | Max (s) | Range (s) | Completed trips | Link entries |")
    print("|---|---:|---:|---:|---:|---:|---:|---:|")
    for label, _ in labels:
        for parts, row in sorted(results[label].items()):
            print(f"| {label} | {parts} | {row['median']} | {row['min']} | {row['max']} | "
                  f"{row['range']} | {row['completed_trips']} | {row['link_entries']} |")
    print()
    for parts in [1, 2]:
        before = results["Queue before"][parts]
        after = results["Queue after"][parts]
        for key in ["completed_trips", "link_entries"]:
            if before[key] != after[key]:
                raise ValueError(f"Queue {key} changed for {parts} partitions")
        change = float(after["median"]) / float(before["median"]) - 1
        separated = float(after["min"]) > float(before["max"])
        investigate = change > 0.05 and separated
        print(f"- Queue before/after, {parts} partitions: {change:+.2%}; counts unchanged. "
              f"{'INVESTIGATE: above 5% with disjoint timing ranges.' if investigate else 'No regression above 5% beyond the observed spread.'}")
        waves = results["Kinematic Waves after"][parts]
        cost = float(waves["median"]) / float(after["median"]) - 1
        print(f"- Kinematic Waves / Queue after, {parts} partitions: {cost:+.2%}. "
              "Traffic physics and therefore result counts may differ.")
    for label, _ in labels:
        ratio = float(results[label][2]["median"]) / float(results[label][1]["median"])
        print(f"- {label}, two/one partitions: {ratio:.3f} times the runtime.")
    print("\nMachine, compiler, exact source revisions, input hashes and settings are recorded "
          "in the accompanying environment.txt, baseline-source.txt, inputs.sha256 and config.yml. "
          "The raw logs retain all warmup and measured runtimes.")


if __name__ == "__main__":
    main(pathlib.Path(sys.argv[1]))

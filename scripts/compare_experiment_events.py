#!/usr/bin/env python3
"""Compare final-iteration event outcomes from two completed local_qsim runs."""

from __future__ import annotations

import argparse
import heapq
import itertools
import json
import statistics
import subprocess
import sys
import xml.etree.ElementTree as ET
from collections import Counter, defaultdict
from pathlib import Path


def read_events(path: Path):
    process = subprocess.Popen(["zstdcat", str(path)], stdout=subprocess.PIPE)
    if process.stdout is None:
        raise RuntimeError(f"could not read decompressed events from {path}")
    for _, element in ET.iterparse(process.stdout, events=("end",)):
        if element.tag == "event":
            yield float(element.get("time", "0")), dict(element.attrib)
        element.clear()
    return_code = process.wait()
    if return_code != 0:
        raise RuntimeError(f"zstdcat failed for {path} with exit code {return_code}")


def summarize(events_dir: Path) -> dict:
    paths = sorted(events_dir.glob("events.*.xml.zst"))
    if not paths:
        raise ValueError(f"no events.*.xml.zst files in {events_dir}")

    event_types: Counter[str] = Counter()
    entered_links: Counter[str] = Counter()
    travelled_events: Counter[str] = Counter()
    distances: Counter[str] = Counter()
    leg_times: dict[str, list[float]] = defaultdict(list)
    active_departures: dict[str, tuple[float, str]] = {}
    unmatched_arrivals = 0
    overlapping_departures = 0
    event_streams = [iter(read_events(path)) for path in paths]
    order = itertools.count()
    pending = []
    for stream_index, stream in enumerate(event_streams):
        event = next(stream, None)
        if event is not None:
            time, attributes = event
            heapq.heappush(pending, (time, next(order), stream_index, attributes))

    while pending:
        time, _, stream_index, attributes = heapq.heappop(pending)
        kind = attributes.get("type", "")
        event_types[kind] += 1
        person = attributes.get("person")
        if kind == "departure" and person is not None:
            if person in active_departures:
                overlapping_departures += 1
            active_departures[person] = (time, attributes.get("legMode", "unknown"))
        elif kind == "arrival" and person is not None:
            departure = active_departures.pop(person, None)
            if departure is None:
                unmatched_arrivals += 1
            else:
                start, mode = departure
                leg_times[mode].append(time - start)
        elif kind == "stuckAndAbort" and person is not None:
            active_departures.pop(person, None)
        elif kind == "entered link":
            entered_links[attributes.get("link", "unknown")] += 1
        elif kind == "travelled":
            mode = attributes.get("mode", "unknown")
            travelled_events[mode] += 1
            distances[mode] += float(attributes.get("distance", "0"))

        event = next(event_streams[stream_index], None)
        if event is not None:
            next_time, next_attributes = event
            heapq.heappush(
                pending,
                (next_time, next(order), stream_index, next_attributes),
            )

    return {
        "event_types": dict(sorted(event_types.items())),
        "entered_link_events": sum(entered_links.values()),
        "links_used": len(entered_links),
        "entered_link_counts": dict(sorted(entered_links.items())),
        "travelled_events_by_mode": dict(sorted(travelled_events.items())),
        "distance_by_mode": {mode: round(value, 1) for mode, value in sorted(distances.items())},
        "paired_legs": {mode: len(times) for mode, times in sorted(leg_times.items())},
        "leg_time_median_seconds": {
            mode: round(statistics.median(times), 2)
            for mode, times in sorted(leg_times.items())
            if times
        },
        "leg_time_mean_seconds": {
            mode: round(statistics.fmean(times), 2)
            for mode, times in sorted(leg_times.items())
            if times
        },
        "unmatched_arrivals": unmatched_arrivals,
        "unmatched_departures_at_end": len(active_departures),
        "overlapping_departures": overlapping_departures,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline_events_dir", type=Path)
    parser.add_argument("candidate_events_dir", type=Path)
    args = parser.parse_args()

    try:
        baseline = summarize(args.baseline_events_dir)
        candidate = summarize(args.candidate_events_dir)
    except (OSError, RuntimeError, ValueError, ET.ParseError) as error:
        parser.error(str(error))

    baseline_links = baseline.pop("entered_link_counts")
    candidate_links = candidate.pop("entered_link_counts")
    all_links = baseline_links.keys() | candidate_links.keys()
    baseline_volume = sum(baseline_links.values())
    absolute_delta = sum(
        abs(baseline_links.get(link, 0) - candidate_links.get(link, 0))
        for link in all_links
    )
    link_volume_delta_pct = (
        round(100 * absolute_delta / baseline_volume, 3) if baseline_volume else None
    )

    print(
        json.dumps(
            {
                "baseline": baseline,
                "candidate": candidate,
                "link_volume_comparison": {
                    "absolute_count_delta": absolute_delta,
                    "absolute_count_delta_pct_of_baseline": link_volume_delta_pct,
                    "links_with_changed_counts": sum(
                        baseline_links.get(link, 0) != candidate_links.get(link, 0)
                        for link in all_links
                    ),
                },
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())

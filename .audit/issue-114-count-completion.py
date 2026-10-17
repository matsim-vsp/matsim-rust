"""Count completed departure/arrival pairs without treating aborts as arrivals."""
import collections
import io
import json
import sys
import xml.etree.ElementTree as ET


def count_events(sources):
    """Count completed legs across one or more event sources merged by person."""
    modes = collections.defaultdict(collections.Counter)
    anomalies = collections.Counter()
    person_events = collections.defaultdict(list)

    for source in sources:
        for _, element in ET.iterparse(source, events=("end",)):
            if element.tag != "event":
                continue
            event = element.attrib
            kind = event.get("type")
            if kind in ("departure", "arrival", "stuckAndAbort"):
                person = event.get("person")
                time = float(event["time"])
                mode = event.get("legMode")
                # arrival and stuck close the prior leg, departure opens the next leg
                order = 0 if kind in ("arrival", "stuckAndAbort") else 1
                person_events[person].append((time, order, kind, mode))
            element.clear()

    for person, events in person_events.items():
        events.sort()
        active = None
        for time, _, kind, mode in events:
            if kind == "departure":
                if active is not None:
                    anomalies["overlapping_departure"] += 1
                mode = mode or "unknown"
                modes[mode]["departures"] += 1
                active = mode
            elif kind in ("arrival", "stuckAndAbort"):
                if active is None:
                    anomalies["unmatched_" + kind] += 1
                elif kind == "stuckAndAbort":
                    modes[active]["stuck"] += 1
                    active = None
                elif time <= 86400:
                    modes[active]["genuine_arrivals"] += 1
                    active = None
                else:
                    modes[active]["arrival_after_day"] += 1
                    active = None
        if active is not None:
            modes[active]["unfinished"] += 1

    totals = sum(modes.values(), collections.Counter())
    percent = (100 * totals["genuine_arrivals"] / totals["departures"]) if totals["departures"] else 0.0
    return dict(
        partitions_merged=len(sources),
        by_leg_mode=dict(modes),
        totals=totals,
        anomalies=anomalies,
        genuine_completion_pct=percent,
        passes_completion_gate=percent >= 95 and not anomalies,
    )


def count(source):
    return count_events([source])


def self_check():
    result = count(io.StringIO('''<events>
      <event time="0" type="departure" person="a" legMode="pt"/>
      <event time="10" type="arrival" person="a"/>
      <event time="0" type="departure" person="b" legMode="car"/>
      <event time="20" type="stuckAndAbort" person="b"/>
      <event time="0" type="departure" person="c" legMode="walk"/>
      <event time="0" type="departure" person="d" legMode="walk"/>
      <event time="86401" type="arrival" person="d"/>
    </events>'''))
    assert result["totals"] == dict(departures=4, genuine_arrivals=1, stuck=1,
                                    unfinished=1, arrival_after_day=1)
    assert not result["passes_completion_gate"]
    assert result["partitions_merged"] == 1

    # Cross-partition merge test: person a departs on partition 0, arrives on partition 1
    merged = count_events([
        io.StringIO('''<events>
          <event time="0" type="departure" person="a" legMode="pt"/>
        </events>'''),
        io.StringIO('''<events>
          <event time="10" type="arrival" person="a"/>
        </events>'''),
    ])
    assert merged["totals"] == dict(departures=1, genuine_arrivals=1)
    assert not merged["anomalies"]
    assert merged["passes_completion_gate"]
    assert merged["partitions_merged"] == 2


if __name__ == "__main__":
    self_check()
    sources = sys.argv[1:]
    if not sources:
        print("Usage: python issue-114-count-completion.py <event_files...>")
        sys.exit(0)
    output = count_events(sources)
    print(json.dumps(output, indent=2))

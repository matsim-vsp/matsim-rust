"""Count completed departure/arrival pairs without treating aborts as arrivals."""
import collections
import io
import json
import sys
import xml.etree.ElementTree as ET


def count(source):
    modes = collections.defaultdict(collections.Counter)
    active = {}
    anomalies = collections.Counter()
    for _, element in ET.iterparse(source, events=("end",)):
        if element.tag != "event":
            continue
        event = element.attrib
        kind, person = event.get("type"), event.get("person")
        time = float(event["time"])
        if kind == "departure":
            if person in active:
                anomalies["overlapping_departure"] += 1
            mode = event["legMode"]
            modes[mode]["departures"] += 1
            active[person] = mode
        elif kind in ("arrival", "stuckAndAbort"):
            mode = active.pop(person, None)
            if mode is None:
                anomalies["unmatched_" + kind] += 1
            elif kind == "stuckAndAbort":
                modes[mode]["stuck"] += 1
            elif time <= 86400:
                modes[mode]["genuine_arrivals"] += 1
            else:
                modes[mode]["arrival_after_day"] += 1
        element.clear()
    for mode in active.values():
        modes[mode]["unfinished"] += 1
    totals = sum(modes.values(), collections.Counter())
    percent = 100 * totals["genuine_arrivals"] / totals["departures"]
    return dict(by_leg_mode=dict(modes), totals=totals, anomalies=anomalies,
                genuine_completion_pct=percent,
                passes_completion_gate=percent >= 95 and not anomalies)


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


if __name__ == "__main__":
    self_check()
    output = {path: count(path) for path in sys.argv[1:]}
    print(json.dumps(output, indent=2))

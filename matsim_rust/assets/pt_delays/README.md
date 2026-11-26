# Scenario `pt_delays`

A small transit scenario for the development of exogenous transit delays and routing with real-time
information (see `docs/exogenous-pt-delays.md` in `parallel-qsim-berlin`). Unlike
`pt_tutorial`, which has a single line with one route per direction, it has alternatives between
routes and transfers. It will grow with the development.

```
                              village
                                 |  feeder
  suburb ----- midtown -------- hub -------------------- center
     |                 local                 local, trunk    |
     +---------------------- express ------------------------+
```

Each stop has a loop link `<stop>_stop` at its node, which carries the stop facility and the
activities of the persons there, so all walks take 0 s. Only transit runs on the network. With
`ScheduleBasedTeleportation`, vehicles jump from stop to stop according to the schedule.

## Schedule

Departure ids are `<line>_<HHMM>` of the departure at the first stop. Each departure has a vehicle of
its own with the same id, so the events name the departure. Vehicles do not dwell at stops.

| Line | Route | Stops (minutes from the first stop) | Departures |
|---|---|---|---|
| `local` | `local_suburb_center` | suburb 0, midtown 5, hub 10, center 20 | `local_0800`, `local_0802`, `local_0810` |
| `express` | `express_suburb_center` | suburb 0, center 10 | `express_0805` |
| `feeder` | `feeder_village_hub` | village 0, hub 6 | `feeder_0800`, `feeder_0810` |
| `trunk` | `trunk_hub_center` | hub 0, center 6 | `trunk_0808`, `trunk_0815`, `trunk_0822` |

## Persons

Each person is named after what it does in the base case, without delays. Its plan in `plans.xml`
is the earliest arrival on the schedule (derived by hand, not yet checked with a router).

| Person | Base case |
|---|---|
| `takes_later_but_faster_express` | leaves suburb at 07:58, lets `local_0800` (center 08:20) go and takes `express_0805`, center 08:15 |
| `transfers_with_two_minutes_at_hub` | leaves village at 07:58, `feeder_0800` to hub (08:06), `trunk_0808`, center 08:14 |
| `waits_at_hub_after_just_missing_trunk` | leaves village at 08:08, `feeder_0810` reaches hub at 08:16, one minute after `trunk_0815` left; `trunk_0822`, center 08:28 |
| `starts_at_hub_after_trunk_departed` | leaves home at hub at 08:18, after `trunk_0815` left; `trunk_0822`, center 08:28 |

Other connections are slower on purpose, e.g. `local_0810` passes hub at 08:20 but reaches center
only at 08:30, after `trunk_0822`.

## Tests

`tests/simulation/pt_delays.rs`, `pt_delays_base_case`: runs the base case with one partition and
with two, where the test puts hub and the links into it on partition 1, and compares the events
of both with `expected_events_base_case/events.0.xml`. In these, each person enters the vehicles its
name describes and starts work two seconds after the arrival of its last vehicle (one tick for the pt
leg to end, one for the walk of 0 s), and every vehicle arrives at and departs from each stop, the
first and the last included, at the planned time with delay 0.

## Planned delay variants

Not implemented yet; the input format of delays is still open, so the variants will be given by the
test code as fixed extra times per departure. How persons react to them depends on the open
decisions D05 and D06, so only the question each variant raises is fixed here.

| Variant | Person concerned | Question |
|---|---|---|
| `express_0805_late` (+10 min) | `takes_later_but_faster_express` | The chosen express is no longer the fastest connection. |
| `feeder_0800_late` (+3 min) | `transfers_with_two_minutes_at_hub` | The connection to `trunk_0808` is lost. |
| `trunk_0815_late_2min` (+2 min) | `waits_at_hub_after_just_missing_trunk` | A connection missed before becomes reachable. |
| `trunk_0815_late_5min` (+5 min) | `starts_at_hub_after_trunk_departed` | A departure that left on schedule is still there. |
| `local_0800_late` (+5 min at suburb) | none | Vehicles only: `local_0802` stays behind `local_0800` at every stop. |

## Not there yet

- A MATSim `config.xml` for the Java router (SwissRailRaptor). Its transfer parameters must be set
  explicitly, otherwise they shift the tight connections at hub.

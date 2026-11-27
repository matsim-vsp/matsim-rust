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

`tests/simulation/pt_delays.rs` runs each case with one partition and with two, where the test puts
hub and the links into it on partition 1, and compares the events of both with
`expected_events_<case>/events.0.xml`.

`pt_delays_base_case`: each person enters the vehicles its name describes and starts work two
seconds after the arrival of its last vehicle (one tick for the pt leg to end, one for the walk of
0 s), and every vehicle arrives at and departs from each stop, the first and the last included, at
the planned time with delay 0.

## Delay variants

In each variant, one departure has exogenous delays at one stop of its route. The test sets them as
the attribute `delays` of the departure, which a transit schedule file can give as well. It is a
JSON array with one object per stop with delays:

```xml
<departure id="trunk_0815" departureTime="08:15:00" vehicleRefId="trunk_0815">
	<attributes>
		<attribute name="delays" class="java.lang.String">[{"stopIndex":0,"departureDelayOffset":120}]</attribute>
	</attributes>
</departure>
```

`stopIndex` is the position of the stop in the route, from 0. `arrivalDelayOffset` is added to the
arrival at the stop (at the first stop, the departure starts later), `departureDelayOffset` to the
departure from it (the vehicle halts longer). Both are whole seconds, optional and 0 by default. The
vehicle carries the delay on to the next stops, where a halt can absorb it.

Only the drivers of `StopDelayDriverAgentSource` carry out these delays; the test adds it to the
agent sources of the controller. Without it, the default source creates drivers who ignore the
attributes.

The persons follow their plans; how they react to delays with new routes depends on the open
decisions D05 and D06. A passenger boards the next vehicle of its planned line that serves its
egress stop.

| Variant | Delay | Question | Result |
|---|---|---|---|
| `express_0805_late` | arrival +10 min at center (1) | The chosen express is no longer the fastest connection. | `takes_later_but_faster_express` still rides it, center 08:25 |
| `feeder_0800_late` | arrival +3 min at hub (1) | The connection to `trunk_0808` is lost. | `transfers_with_two_minutes_at_hub` reaches hub at 08:09 and takes `trunk_0815`, center 08:21 |
| `trunk_0815_late_2min` | departure +2 min at hub (0) | A connection missed before becomes reachable. | `waits_at_hub_after_just_missing_trunk` catches `trunk_0815` (leaves 08:17), center 08:23 |
| `trunk_0815_late_5min` | departure +5 min at hub (0) | A departure that left on schedule is still there. | `starts_at_hub_after_trunk_departed` and `waits_at_hub_after_just_missing_trunk` catch it (leaves 08:20), center 08:26 |
| `local_0800_late` | departure +5 min at suburb (0) | Vehicles only: `local_0802` stays behind `local_0800` at every stop. | `local_0802` leaves suburb and reaches each stop in the same tick as `local_0800`, after it |
| `local_0800_late_before_center` | arrival +4 min at center (3) | The delay takes effect on another partition than the one where the driver started. | `local_0800` reaches center at 08:24, `local_0802` waits in front of center until then |

## Not there yet

- A MATSim `config.xml` for the Java router (SwissRailRaptor). Its transfer parameters must be set
  explicitly, otherwise they shift the tight connections at hub.

# Automatic analysis

Automatic final-iteration analysis can be enabled with `output.analysis.enabled`.
It writes an offline HTML report and CSV/JSON/SVG files under `output/analysis`.

## Journeys and travel distributions

The final-iteration replay exports observed legs in `legs.csv` and planned journeys in
`journeys.csv`. A journey spans consecutive substantive activities. Activity types containing
`interaction` are treated as stage activities, so transit access, egress, and transfer waits stay
inside one journey. Journey duration is elapsed time from the first observed component departure
to the last component arrival; incomplete journeys keep their mode and distance but have no duration.
The purpose is the destination activity type.

The main mode uses MATSim's default analysis hierarchy: the highest-ranked component mode wins,
which folds walk-transit-walk and transit transfers into one transit journey. An entirely walked
journey remains walk. When a custom mode is mixed with a known non-walk mode, the report marks the
main mode `unknown_mixed_modes` rather than guessing at a hierarchy the run has not configured.
`journey_mode_share.csv` groups journey counts and shares by departure
interval, purpose, distance class, and main mode. Distance classes are under 1 km, 1–5 km, 5–10 km,
10–25 km, 25 km or more, and unknown. `journey_summary.csv` reports count, completion, and mean,
population standard deviation, median, and 90th percentile for duration and distance by main mode
and destination purpose. Percentiles use the nearest-rank definition.

Journey distance sums the prepared plan's route distance for every component leg. This includes
model-derived distances assigned to teleported routes during plan preparation. If any component
has no finite non-negative route distance, the total is blank; `distance_provenance` distinguishes
`planned_route`, `partial_planned_route`, and `unavailable`. Component leg indices and modes
link each journey to `legs.csv`. A journey with no observed components is `not_departed`; observed
journeys distinguish `completed`, `stuck`, `missing_arrival`, and `incomplete`. These tables
describe the recorded selected plan and replayed events of the latest completed iteration only.

To compare saved journey mode shares, run `analyze --run-dir RUN --compare-run-dir OTHER`; repeat
`--compare-run-dir` for more runs. The command refreshes RUN's latest-iteration report, reads each
supplied run's recorded `journey_mode_share.csv`, and writes a local comparison report and combined
table under `RUN/analysis/cross_run_comparison`. A comparison refuses a run whose report is failed
or whose recorded iteration is not its latest output iteration.

## Public transport performance and demand validation

Public transport is modeled by teleportation in this build. A `travelled with pt` event records
the line, route, access and egress stop and scheduled boarding time of one passenger trip, and no
transit vehicle drives through the network. The transit tables come from those records, the
person departure, arrival and stuck events, and the schedule and vehicle capacities recorded in
`run_metadata.json` (`transit`). The run's vehicle file is the only capacity source: a departure's
`vehicleRefId` is looked up in it, and the vehicle type's `<capacity>` (seats plus standing room)
is the capacity. Both XML and protobuf vehicle files carry it.

`transit_trips.csv` has one row per passenger transit leg. `service_modeling` is `teleported`
for a leg with a service record and `unrecorded` otherwise. `outcome` is `boarded`,
`missed_service` (the passenger reached the stop after the scheduled boarding time),
`no_service_record` (the leg arrived with no service record), `stuck` or `incomplete`.
Waiting is the scheduled boarding time minus the passenger's departure; in-vehicle time is the
arrival minus the boarding time. A missed service has no waiting time. Arrival delay is the
arrival minus the scheduled arrival at the egress stop, found by matching the recorded boarding
time and stop pair to a scheduled departure; teleported service follows the schedule, so delay
is only a consistency check. Vehicle-level delay and missed stops would need transit vehicle
service events, which this build does not record, so they are always unavailable.

| Table | Content |
| --- | --- |
| `transit_stop_hourly.csv` | boardings and alightings per interval, line and stop |
| `transit_line_summary.csv` | trips, missed services, mean waiting, in-vehicle time and delay per interval, line and route, with the number of observations behind each mean |
| `transit_occupancy.csv` | passengers, capacity and load factor per scheduled departure and route segment |
| `transit_journeys.csv` | access, egress, transfer, waiting and in-vehicle time per journey that uses transit |
| `transit_outcomes.csv` | trip outcomes per interval and service modeling |
| `transit_availability.csv` | which metric groups are available and why not |

Trip outcomes use the interval of the passenger's departure, line summaries and boardings the
interval of the scheduled boarding time, and alightings the interval of the arrival. A trip
with outcome `missed_service` still counts in boardings, alightings, line trips and occupancy,
because the simulation carried the passenger on that run; only its waiting time is unavailable.
An omitted `seats` or `standingRoom` element contributes zero persons, and a `<capacity>`
element naming neither declares no capacity.

An interval is the one containing the boarding time for boardings and the arrival time for
alightings. Counts are expanded by the reciprocal of `qsim.sample_size`; the `_sample` columns
keep the simulated counts. A load factor is the expanded passenger count divided by the vehicle
capacity, so it can exceed one. A journey's access and egress are the legs before the first and
after the last transit leg; the transfer time sums the time between leaving one vehicle and
boarding the next, walking and waiting included. The journey waiting time sums the wait at every
boarding, so a transfer wait is part of both the waiting and the transfer time. Any quantity whose inputs are missing (no
service record, no schedule, no matching departure, no capacity, an unfinished component leg) is
blank and listed as unavailable; it is never inferred, and a journey with a blank quantity has
status `incomplete`.

Set `output.analysis.transit_observed_data` to a CSV of observed demand to compare it with the
simulated boardings and alightings. Relative paths are resolved from the run's output directory.

```csv
scope,line_id,stop_id,station_id,period_start_seconds,period_end_seconds,metric,unit,value,source
stop,,1,,0,3600,boardings,persons,120,counter-a
line,Blue Line,,,0,3600,alightings,persons,800,survey
station,,,central,0,3600,boardings,persons,300,gate-counts
line_stop,Blue Line,3,,0,3600,alightings,persons,90,survey
```

`scope` is `stop`, `station` (the stop facility's `stop_area_id`), `line` or `line_stop`, and
the matching id columns have to be set. `metric` is `boardings` or `alightings` with unit
`persons`, `person` or `passengers`. The period has to be exactly one analysis interval.
`transit_validation_matches.csv` carries the observed value, the simulated sample count, the
expansion factor, the expanded simulated value, the residual, the relative error (blank when
the observation is zero), the network-wide simulated total of the same metric and interval as a
denominator, and the observation source with its row. Rows that cannot be compared, with a
reason such as `unknown_entity` or `period_mismatch`, are in `transit_validation_unmatched.csv`.
`transit_validation_summary.csv` gives matched and unmatched counts, observed and simulated
totals, bias, MAE, RMSE and the relative bias per scope and metric. An invalid file marks only
the `transit_validation` module failed.

## Link speeds

`link_speed` reconstructs traversal speeds from the same replay that produces the link
volumes. A speed observation needs a *full-link* traversal: the vehicle has to enter a
link at its start and leave it at its end. QSim records the first link of a network leg
with `vehicle enters traffic` and the last one with `vehicle leaves traffic` instead of
`entered link` and `left link`, and both carry a relative position along the link, so a
vehicle inserted at the end of its start link covers no distance. Such traversals are
reported as partial records rather than folded into the statistics, which is a
deliberate deviation from the MATSim link-speed analysis.

An observation is assigned to the interval in which the vehicle *entered* the link, so
a traversal crossing an interval boundary stays whole in its entry interval. Because an
entry is what starts a traversal, every interval that can hold a speed also holds a link
entry, and volume, coverage, group and speed tables therefore share one interval list.

`link_speed_hourly.csv` holds the per-link statistics, `link_speed_summary.csv` the
across-link mean and population standard deviation per interval,
`link_speed_histogram.csv` the fixed-bin distribution, and
`link_speed_diagnostics.csv` every record that could not produce a full-link speed.


## Observed validation

Set `output.analysis.observed_data` to a CSV file to compare observations with the latest
completed iteration. Relative paths are resolved from the run's output directory; standalone
reanalysis reuses the path recorded in the manifest. The file must contain one row per link and
period, with these exact headers:

```csv
link_id,period_start_seconds,period_end_seconds,vehicle_class,metric,unit,value,split
link-1,0,3600,all,count,vehicles,120,calibration
link-1,0,3600,all,speed,km/h,36,holdout
```

`link_id` is the external network link ID. Periods must match the configured analysis interval
exactly, and each split may contain only one row per link, period, class, and metric.
`vehicle_class` accepts `all` for aggregate results or a vehicle type ID from the run's
vehicle definitions. Class-specific count and speed tables are exported alongside aggregate link
tables. The class name `all` is reserved for aggregate observations. `metric` accepts `count` or
`speed`; count units are `vehicles`, `vehicle`, or
`veh`, and speed units are `m/s`, `mps`, `km/h`, or `kph`. Counts are expanded by the reciprocal
of `qsim.sample_size`, while speeds are not expanded. `split` is `calibration` or `holdout`. An
optional `source` column can identify a station or data source; the path, label, and source row
are carried into matched and unmatched exports.

The report exports matched rows, unmatched input rows, and bias, MAE, RMSE, and count GEH in CSV,
grouped by split, metric, and vehicle class. GEH scales matched count intervals to hourly rates before applying the formula, so
its thresholds remain comparable when `interval_seconds` differs from 3600. Relative error is
blank when the observed reference is zero. It also writes separate
calibration and holdout scatterplots by metric, time profiles, and residual maps. These plots use
aggregate `all` observations so vehicle classes are not counted again alongside the aggregate.
The input path is recorded as
observation provenance in each matched row. Validation input errors leave the core report intact
and mark only the validation module failed.

To compare completed runs, set `output.analysis.comparison_runs` to run output directories. Each
directory's `analysis/manifest.json` selects its latest completed iteration, and its published
aggregate and vehicle-class count and speed tables are combined in `cross_run_comparison.csv`.
Count rows include both the simulated sample and the population-expanded value, using each run's
recorded sample size; speed rows have identical sample and population values. Each row includes
the full period start and end, so runs with different interval widths remain identifiable.
Relative paths are resolved from the current run's output directory. A missing or incomplete
comparison report marks only the cross-run comparison module failed.


## Network distance, time and congestion

`network_distance_time.csv` reports observed vehicle link traversals by link and interval;
`network_distance_time_summary.csv` reports network totals by interval. Traversal distance is
`link.length * (exit_position - entry_position)`, so first and last link portions count. Visits
with entry position zero and exit position one are complete; other valid forward positions are
partial and included. Positions outside `[0, 1]` or a decreasing position, non-finite or negative
lengths and decreasing event times are excluded and counted in
`network_distance_time_diagnostics.csv`. A valid visit with zero elapsed time retains its observed
distance and traversal count, reports zero vehicle time and signed delay when the reference speed
is valid, and contributes no relative-speed ratio; it is counted in the non-positive-duration
diagnostic. Visits still open at the end are counted as unfinished and contribute no guessed
distance or time. A same-link route is a regular visit and is counted when its entry and exit
events are paired.

Vehicle time is the elapsed time between link entry and exit. A visit crossing an analysis
interval boundary is assigned whole to its entry interval, as in the link-speed tables. This
avoids assuming how the vehicle moved inside the link. Distance and signed free-flow-relative
delay are kept with that visit. Free-flow-relative delay is `observed time - distance /
freespeed`, so it can be negative. `relative_speed_ratio` is free-flow travel time divided by
observed time, so values below one indicate slower than free flow and values above one indicate
faster travel. Network ratios use the sums over visits with valid free speeds. A non-finite or non-positive free speed leaves delay blank for
that traversal while retaining its distance and time. The optional
`output.analysis.excess_delay_clip_seconds` setting exports separately labeled clipped excess
delay: per link and interval it sums positive traversal delay, then caps that sum at the configured
value. The network total sums those capped per-link values, so it matches the per-link export.
The default is unset, in which case the clipped-delay column and catalog entry are omitted.
It can be set in YAML or with `--set output.analysis.excess_delay_clip_seconds=120`.

The event stream records vehicles and person travel events but does not provide reliable
link-level passenger occupancy. Passenger distance and time are therefore explicitly unavailable;
PCE is a capacity weight and is not treated as a passenger count. Existing leg departure and
completion metrics provide agent travel profiles. `en_route_agents.csv` counts distinct people
with an observed departure not yet matched by an arrival or stuck event, reports departures,
arrivals, stuck events, interval-start and peak concurrent counts, and apportions person-seconds
from event timestamps. It covers travel modes in the person event stream and does not claim
link-level network occupancy. Link speed tables remain a
separate view and only include complete full-link traversals.


## Link classification

`output.analysis.link_labels` is a map keyed by external link ID. Each entry can
provide independent `road_type` and `road_size` strings. If no geographic
boundary is configured, it may also provide `urban_area`. Missing and blank
labels are exported and grouped as `unknown`; supplied category spelling is
preserved.

`output.analysis.urban_boundary` is an optional polygon encoded as a list of
`[x, y]` coordinates in the same coordinate system and units as network node
coordinates. It needs at least three finite points; the last point is connected
to the first automatically. Points on the polygon boundary count as inside, to
within floating-point rounding. Assignment uses the link's endpoints: both inside
is `inner` even if the segment leaves a concave polygon, both outside is `outer`
unless the segment crosses or touches the polygon, and exactly one inside is
`cross_boundary`. When a polygon is supplied, it determines urban labels instead
of per-link `urban_area` labels.

Classification is report metadata only; it does not filter the eligible network.
Group coverage reports each category's fixed eligible-link count and its used
and unused counts for every hourly interval. The three dimensions are
aggregated independently so links with incomplete labels remain visible.

The report also provides independent urban-area, road-type, and road-size
filters. They update the per-link hourly metrics, group coverage table, and map
together. A filtered group table recounts eligible links within the selected
subset, so its rows always describe exactly the links on screen;
`group_coverage.csv` is unaffected and keeps the full-network denominators.

The filters need the per-link hourly rows in the page, so `index.html` embeds one
JSON row per link and interval, roughly 260 bytes each. That is negligible for a
small network and grows to tens or hundreds of megabytes for a large one (about
300 MiB at 50,000 links over 24 hourly intervals), which browsers handle poorly.
`link_hourly.csv` carries the same data compactly and is the artefact to read for
a network of that size.

The local SVG map marks links used at least once during the final iteration in
green and unused links in gray. Dashed lines identify expressways, which means
the exact, case-sensitive label `expressway`; any other road type is drawn solid.
Hover over a map link to see its labels and usage.

For example:

```yaml
output:
  analysis:
    enabled: true
    link_labels:
      link-1:
        road_type: expressway
        road_size: large
    urban_boundary:
      - [0.0, 0.0]
      - [1000.0, 0.0]
      - [1000.0, 1000.0]
      - [0.0, 1000.0]
```

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

## Accessibility to supplied opportunities

The `accessibility` module reports how many jobs, schools or services each origin
can reach within a travel-time threshold. It stays `unavailable` until all three
of its inputs are configured, and a partial set or an unreadable file marks only
this module `failed`. Relative paths resolve from the run's output directory.

The measure is declared rather than implied: every exported row carries
`measure=cumulative_opportunities_within_threshold`. It sums the weight of every
supplied opportunity whose **potential** travel cost from the origin is at or
below the threshold, and the threshold is inclusive, so a destination costing
exactly the threshold counts as reachable.

Realized trips are not potential destinations. A journey table says how long one
person actually took, which says nothing about how long anyone else *could*
take, so `legs.csv` and the journey tables are never substituted for the supplied
costs. Where a cost is missing, the module reports no value rather than deriving
one from what happened to be travelled.

The three inputs, all CSVs with these exact headers:

```csv
# opportunities: one row per location
opportunity_id,category,x,y,count
job-1,jobs,100.0,200.0,250

# zones: the explicit coordinate/zone correspondence
zone_id,x,y
zone-1,0.0,0.0

# travel_costs: potential-destination costs, by mode and departure period
origin_zone,destination_zone,mode,period_start_seconds,travel_time_seconds
zone-1,zone-1,car,28800,0
zone-1,zone-2,car,28800,1800
```

`x` and `y` are in the same coordinate system and units as network node
coordinates. Weights and costs must be finite and non-negative, and a duplicate
location, zone or cost key is a module failure. An opportunity or a person is
placed in the zone whose centroid is nearest by horizontal distance, with ties
broken on the zone id, so the assignment does not depend on the input file's row
order. A person is placed by their first non-stage activity. A blank category is
reported as `unknown` rather than rejected. Cost rows naming a zone the zone file
does not list are counted in `accessibility_diagnostics.csv` instead of failing the
module, because a rectangular skim is routinely wider than the zones under study.

`accessibility_zones.csv` holds one row per origin zone, category, mode, departure
period and threshold. `status` distinguishes five outcomes, because folding them
together would misreport the data:

| `status` | meaning |
|---|---|
| `available` | every location of the category has a supplied cost from this origin |
| `available_missing_costs` | some locations have no cost; they are excluded and counted in `opportunity_locations_without_cost` |
| `unavailable:no_origin_costs` | no cost of this mode and period leaves the origin |
| `unavailable:no_travel_costs` | the cost file supplies no table for this mode and departure period |
| `unavailable:non_finite_measure` | the category's weights summed to a non-finite number |

A person whose plan has a home activity that cannot be placed gets
`unavailable:no_home_zone` rows rather than disappearing from the population. A
person with no selected plan has no recorded expectations at all and so is not in
`accessibility_persons.csv`; the run's `expected_travel` list is what both the
per-person and per-zone tables are built from. Every unavailable status leaves the
measure columns blank rather than reporting zero, so a missing prerequisite is
never read as poor accessibility.

`accessibility_summary.csv` reports, per category, mode, period and threshold, the
mean, median, minimum and maximum over the zones that have a supplied cost, plus a
`population_weighted_opportunities` column. The two differ exactly when
opportunities are unevenly distributed over people, which is the equity signal.
`persons_included` is the simulated person count the weighting covers, and
`sample_size` is the simulated fraction of the population they are, so a reader can
tell a sampled run from a full one. A zone with no supplied cost at all is counted
in `zones_without_costs` and excluded from the statistics, because treating it as a
zone holding zero opportunities would drag every mean down. A zone in
`available_missing_costs` *is* included, so `zones_without_costs` counts only fully
unavailable zones; read the missing-cost share from the zone table.

`accessibility_persons.csv` repeats the value of each person's own origin zone per
cell, which is what an equity analysis reads. Following the catalog rule in
`docs/architecture.md`, every accessibility name in `metric_catalog.json` is a
column of one of the tables above, so a consumer can look it up where it is
exported. The measure itself is catalogued as `opportunities`, and the declared
measure name is in each row's own `measure` column, which is what tells a consumer
which definition a value was computed under. The `aggregation_key` names the
origin, category, mode, period and threshold, which is how a comparison tool lines
two runs up.

`accessibility_map.svg` draws one small panel per reported cell on a shared
projection, up to 24 panels; further combinations stay in the CSVs and
`map_panels_omitted` counts them. A filled circle is an origin zone shaded across a
single-hue ramp normalized to its own panel, a gray circle is an origin with no
supplied cost, and a green ring is a zone holding opportunities of that category,
sized by their total weight. The per-zone and per-person tables are embedded in
`index.html` as bounded previews of 500 rows, because a per-person table over several
categories, modes, periods and thresholds outgrows a page; the summary and
diagnostics tables are embedded in full because each holds one row per reported
combination rather than per person. The CSVs hold every row either way.

For example:

```yaml
output:
  analysis:
    enabled: true
    accessibility:
      opportunities: accessibility/opportunities.csv
      zones: accessibility/zones.csv
      travel_costs: accessibility/travel_costs.csv
      # Defaults to a 45-minute cutoff when omitted.
      thresholds_seconds: [1800, 3600]
```

Every setting is also reachable from the command line, for example
`--set output.analysis.accessibility.thresholds_seconds=1800,3600`.

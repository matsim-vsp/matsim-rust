# Automatic analysis

Automatic final-iteration analysis can be enabled with `output.analysis.enabled`.
It writes an offline HTML report and CSV/JSON/SVG files under `output/analysis`.

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
exactly. `vehicle_class` accepts `all` for aggregate results or a vehicle type ID from the run's
vehicle definitions. Class-specific count and speed tables are exported alongside aggregate link
tables. The class name `all` is reserved for aggregate observations. `metric` accepts `count` or
`speed`; count units are `vehicles`, `vehicle`, or
`veh`, and speed units are `m/s`, `mps`, `km/h`, or `kph`. Counts are expanded by the reciprocal
of `qsim.sample_size`, while speeds are not expanded. `split` is `calibration` or `holdout`.

The report exports matched rows, unmatched input rows, and per-split bias, MAE, RMSE, and count
GEH in CSV. GEH scales matched count intervals to hourly rates before applying the formula, so
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

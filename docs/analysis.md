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

## Comparing completed runs

The shared analysis interface can compare existing reports with an explicit baseline:

```rust,ignore
use rust_qsim::simulation::analysis::compare_completed_runs;
use std::path::{Path, PathBuf};

let report = compare_completed_runs(
    Path::new("runs/baseline"),
    &[PathBuf::from("runs/alternative-a"), PathBuf::from("runs/alternative-b")],
)?;
```

Each input must contain a complete `analysis/manifest.json`, metric catalog and latest-iteration
tables. The comparison is written to `baseline/analysis/comparison/` and leaves each input report
unchanged. It exports one row per common aggregation key with both values, alternative-minus-
baseline difference, relative difference and the baseline denominator. Relative differences are
blank when the baseline is zero. Link rows are matched by external link ID; links missing from
either network are excluded and the number of corresponding links appears in the HTML report.
Runs with different interval widths, simulation end times, sample-size scales, or link
classification/filter definitions are rejected. Agent duration comparisons include only people
with a complete plan in both runs. Metrics without a compatible catalog definition or registered
table are omitted.

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

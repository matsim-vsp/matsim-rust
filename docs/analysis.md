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

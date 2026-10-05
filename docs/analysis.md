# Automatic analysis

Automatic final-iteration analysis can be enabled with `output.analysis.enabled`.
It writes an offline HTML report and CSV/JSON/SVG files under `output/analysis`.

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

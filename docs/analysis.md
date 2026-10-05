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
to the first automatically. Points on the polygon boundary count as inside. A
link whose endpoints are inside is `inner`, one whose endpoints are outside and
whose segment does not cross the polygon is `outer`, and a link with one inside
endpoint or whose segment crosses the polygon is `cross_boundary`. When a
polygon is supplied, it determines urban labels instead of per-link
`urban_area` labels.

Classification is report metadata only; it does not filter the eligible network.
Group coverage reports each category's fixed eligible-link count and its used
and unused counts for every hourly interval. The three dimensions are
aggregated independently so links with incomplete labels remain visible.

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

#!/usr/bin/env python3
"""Add a georeferenced coverage map to a completed report (requires pyproj)."""

import argparse
import json
import math
from pathlib import Path
import tempfile
from urllib.request import urlopen
import xml.etree.ElementTree as ET

from pyproj import Transformer


def coverage_data(network, svg, crs):
    transform = Transformer.from_crs(crs, "EPSG:4326", always_xy=True)
    nodes, links, covered = {}, {}, {}
    for _, element in ET.iterparse(svg):
        if element.tag.endswith("}line") and "data-link-id" in element.attrib:
            covered[element.attrib["data-link-id"]] = element.attrib["stroke"] == "#287a3d"
        element.clear()
    for _, element in ET.iterparse(network):
        if element.tag == "node":
            lon, lat = transform.transform(float(element.attrib["x"]), float(element.attrib["y"]), errcheck=True)
            if not (math.isfinite(lon) and math.isfinite(lat) and abs(lon) <= 180 and abs(lat) < 85.051129):
                raise ValueError(f"Invalid web-map coordinate for node {element.attrib['id']}")
            nodes[element.attrib["id"]] = [round(lat, 7), round(lon, 7)]
        elif element.tag == "link" and element.attrib["id"] in covered:
            links[element.attrib["id"]] = (element.attrib["from"], element.attrib["to"])
        element.clear()
    if not covered or covered.keys() != links.keys():
        raise ValueError("The coverage SVG and network do not contain the same plotted link IDs")
    node_ids = sorted({node for endpoints in links.values() for node in endpoints})
    indices = {node: i for i, node in enumerate(node_ids)}
    return {"crs": crs, "nodes": [nodes[node] for node in node_ids],
            "links": [[link, indices[start], indices[end], covered[link]]
                      for link, (start, end) in sorted(links.items())]}


def write_atomic(path, content):
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as temporary:
        staged = Path(temporary.name)
        temporary.write(content)
    try:
        staged.replace(path)
    finally:
        staged.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--crs", required=True, help="Explicit source CRS, e.g. EPSG:32647; never inferred")
    args = parser.parse_args()
    output = args.run_dir / "analysis"
    data = coverage_data(args.run_dir / "output_network.xml", output / "network_map.svg", args.crs)
    assets = output / "map_assets"
    assets.mkdir(exist_ok=True)
    for filename, url in {
        "leaflet.js": "https://unpkg.com/leaflet@1.9.4/dist/leaflet.js",
        "leaflet.css": "https://unpkg.com/leaflet@1.9.4/dist/leaflet.css",
        "LICENSE": "https://unpkg.com/leaflet@1.9.4/LICENSE",
    }.items():
        target = assets / filename
        if not target.exists():
            with urlopen(url, timeout=30) as response:
                write_atomic(target, response.read())
    payload = "const COVERAGE=" + json.dumps(data, ensure_ascii=True, separators=(",", ":"), allow_nan=False) + ";\n"
    write_atomic(output / "network_coverage.js", payload.encode())
    template = Path(__file__).resolve().parents[1] / "matsim_rust/src/simulation/analysis/coverage_map.html"
    write_atomic(output / "network_coverage.html", template.read_bytes())
    print(f"Wrote {len(data['links'])} links to {output / 'network_coverage.html'} ({args.crs})")


if __name__ == "__main__":
    main()

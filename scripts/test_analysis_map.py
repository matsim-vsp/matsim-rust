"""Run with the same pyproj environment as analysis_map.py."""

from pathlib import Path
import tempfile

from analysis_map import coverage_data


def test_coverage_map():
    with tempfile.TemporaryDirectory() as directory:
        network, svg = Path(directory) / "network.xml", Path(directory) / "map.svg"
        network.write_text('<network><nodes><node id="a" x="500000" y="0"/>'
                           '<node id="b" x="500010" y="10"/></nodes>'
                           '<links><link id="road" from="a" to="b"/></links></network>')
        svg.write_text('<svg xmlns="http://www.w3.org/2000/svg">'
                       '<line data-link-id="road" stroke="#287a3d"/></svg>')
        data = coverage_data(network, svg, "EPSG:32647")
        assert data["nodes"][0] == [0, 99], data
        assert data["nodes"][1][0] > 0 and data["nodes"][1][1] > 99
        assert data["links"] == [["road", 0, 1, True]], data
        svg.write_text(svg.read_text().replace("road", "missing"))
        try:
            coverage_data(network, svg, "EPSG:32647")
        except ValueError:
            pass
        else:
            raise AssertionError("Mismatched coverage accepted")
        network.write_text(network.read_text().replace('x="500000"', 'x="nan"'))
        try:
            coverage_data(network, svg, "EPSG:32647")
        except ValueError:
            pass
        else:
            raise AssertionError("Non-finite coordinate accepted")


if __name__ == "__main__":
    test_coverage_map()
    print("PASS: geographic coordinates, coverage flags, mismatched inputs and non-finite coordinates")

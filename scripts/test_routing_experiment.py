import csv
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from routing_experiment import hash_input, routing_profile_summary

SCRIPT = Path(__file__).with_name("routing_experiment.py")


class RoutingExperimentTests(unittest.TestCase):
    def test_directory_hash_includes_file_boundaries(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            one_file = root / "one"
            two_files = root / "two"
            one_file.mkdir()
            two_files.mkdir()
            (one_file / "a").write_bytes(b"foo\0b\0bar")
            (two_files / "a").write_bytes(b"foo")
            (two_files / "b").write_bytes(b"bar")

            self.assertNotEqual(hash_input(one_file)["sha256"], hash_input(two_files)["sha256"])

    def test_directory_hash_rejects_symlinked_directories(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            input_directory = root / "input"
            linked_directory = root / "linked"
            input_directory.mkdir()
            linked_directory.mkdir()
            (linked_directory / "network.xml").write_text("<network />", encoding="utf-8")
            (input_directory / "network").symlink_to(linked_directory, target_is_directory=True)

            with self.assertRaisesRegex(ValueError, "unsupported symlink directory"):
                hash_input(input_directory)

    def test_routing_profile_summary_counts_search_rows_and_expansions(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            output_directory = Path(temporary_directory)
            profile_directory = output_directory / "instrument"
            profile_directory.mkdir()
            profile_path = profile_directory / "routing_process_0.csv"
            with profile_path.open("w", newline="", encoding="utf-8") as profile:
                writer = csv.DictWriter(profile, fieldnames=["node_count", "nodes_expanded"])
                writer.writeheader()
                writer.writerows(
                    [
                        {"node_count": 5, "nodes_expanded": 7},
                        {"node_count": 9, "nodes_expanded": 6},
                    ]
                )

            summary = routing_profile_summary(output_directory)

            self.assertEqual(summary["search_count"], 2)
            self.assertEqual(summary["nodes_expanded"], 13)
            self.assertEqual(summary["files"], [str(profile_path)])

    def test_route_active_run_fails_without_search_rows(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            root = Path(temporary_directory)
            binary = root / "fake_qsim"
            binary.write_text(
                f"#!{sys.executable}\n"
                "import pathlib, sys\n"
                "output = next(value.split('=', 1)[1] for value in sys.argv "
                "if value.startswith('output.output_dir='))\n"
                "profile = pathlib.Path(output) / 'instrument' / 'routing_process_0.csv'\n"
                "profile.parent.mkdir(parents=True)\n"
                "profile.write_text('nodes_expanded\\n', encoding='utf-8')\n",
                encoding="utf-8",
            )
            binary.chmod(binary.stat().st_mode | 0o111)
            output_directory = root / "experiment"
            config = root / "config.yml"
            config.write_text("modules: {}\n", encoding="utf-8")
            command = [
                sys.executable,
                str(SCRIPT),
                "--config",
                str(config),
                "--input",
                str(config),
                "--binary",
                str(binary),
                "--output-dir",
                str(output_directory),
                "--workload",
                "route-active",
                "--population-size",
                "1",
                "--seed",
                "4711",
                "--qsim-workers",
                "1",
                "--replanning-workers",
                "1",
                "--runs",
                "1",
                "--warmups",
                "0",
                "--max-seconds",
                "10",
            ]

            result = subprocess.run(command, capture_output=True, text=True, check=False)
            report = json.loads((output_directory / "experiment.json").read_text(encoding="utf-8"))

            self.assertEqual(result.returncode, 1)
            self.assertEqual(report["runs"][0]["routing_profile"]["search_count"], 0)
            self.assertFalse(report["runs"][0]["routing_profile"]["verified"])
            self.assertIn("No A* search rows found", report["runs"][0]["route_validation_error"])


if __name__ == "__main__":
    unittest.main()

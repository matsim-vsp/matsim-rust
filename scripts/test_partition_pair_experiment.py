import tempfile
import unittest
from pathlib import Path
import sys
from subprocess import CompletedProcess
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
from partition_pair_experiment import (
    compare_run_signatures,
    compares_same_layout,
    diagnostics_order,
    fnv1a64,
    parse_networks,
    plan_state_sha256,
    require_no_metis_partitioning,
    validate_partition_metadata,
    valid_timeout,
)


class PartitionPairExperimentTests(unittest.TestCase):
    def test_metadata_reports_baseline_fallback_as_same_layout(self):
        self.assertTrue(compares_same_layout({"baseline_layout_id": "layout-a", "selected_layout_id": "layout-a"}))
        self.assertFalse(compares_same_layout({"baseline_layout_id": "layout-a", "selected_layout_id": "layout-b"}))

    def test_diagnostics_order_alternates_independently_of_layout_order(self):
        self.assertEqual(diagnostics_order(0), (False, True))
        self.assertEqual(diagnostics_order(1), (False, True))
        self.assertEqual(diagnostics_order(2), (True, False))
        self.assertEqual(diagnostics_order(3), (True, False))

    def test_metadata_must_match_worker_count_and_candidate_network(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            network = Path(temporary_directory) / "network.binpb"
            network.write_bytes(b"same selected layout")
            baseline = Path(temporary_directory) / "baseline.binpb"
            baseline.write_bytes(b"corrected weighted baseline")
            metadata = {
                "worker_count": 2,
                "baseline_network_fnv64": fnv1a64(baseline),
                "selected_network_fnv64": fnv1a64(network),
            }
            validate_partition_metadata(metadata, 2, baseline, network)
            with self.assertRaisesRegex(ValueError, "worker_count"):
                validate_partition_metadata(metadata, 3, baseline, network)
            network.write_bytes(b"different layout")
            with self.assertRaisesRegex(ValueError, "candidate network"):
                validate_partition_metadata(metadata, 2, baseline, network)
            baseline.write_bytes(b"unweighted baseline")
            with self.assertRaisesRegex(ValueError, "corrected weighted-METIS"):
                validate_partition_metadata(metadata, 2, baseline, network)

    def test_timeout_must_be_finite_and_positive(self):
        for value in (float("nan"), float("inf"), float("-inf"), 0.0, -1.0):
            self.assertFalse(valid_timeout(value))
        self.assertTrue(valid_timeout(1.0))

    def test_run_signatures_detect_instrumentation_and_repeatability_changes(self):
        baseline = {"events": "event-hash", "final_population": "plan-hash"}
        instrumented = {
            "events": "event-hash",
            "final_population": "plan-hash",
            "diagnostic_counts": "counts-hash",
        }
        self.assertTrue(compare_run_signatures(baseline, instrumented, False)["matches"])
        self.assertTrue(compare_run_signatures(instrumented, instrumented, True)["matches"])
        changed = {**instrumented, "diagnostic_counts": "different-counts"}
        self.assertFalse(compare_run_signatures(instrumented, changed, True)["matches"])
        changed_events = {**baseline, "events": "different-events"}
        self.assertFalse(compare_run_signatures(baseline, changed_events, False)["matches"])

    def test_networks_are_keyed_by_worker_count_and_resolved(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            network = Path(temporary_directory) / "network.binpb"
            network.touch()

            parsed = parse_networks([f"2={network}"])

            self.assertEqual(parsed, {2: network.resolve()})

    def test_network_mapping_rejects_duplicates_and_missing_paths(self):
        with self.assertRaisesRegex(ValueError, "unique"):
            parse_networks(["2=baseline.binpb", "2=candidate.binpb"])
        with self.assertRaisesRegex(ValueError, "expected WORKERS=PATH"):
            parse_networks(["network.binpb"])

    def test_plan_hash_ignores_xml_attribute_order_and_formatting(self):
        path = Path("plans.xml.zst")
        outputs = iter(
            [
                b'<plans><person id="1" age="20"></person></plans>',
                b'<plans>\n  <person age="20" id="1" />\n</plans>',
            ]
        )
        with patch(
            "partition_pair_experiment.subprocess.run",
            side_effect=lambda *args, **kwargs: CompletedProcess(args[0], 0, next(outputs), b""),
        ):
            self.assertEqual(plan_state_sha256(path), plan_state_sha256(path))

    def test_plan_hash_ignores_person_serialization_order(self):
        path = Path("plans.xml.zst")
        outputs = iter(
            [
                b'<plans><person id="b"/><person id="a"/></plans>',
                b'<plans><person id="a"/><person id="b"/></plans>',
            ]
        )
        with patch(
            "partition_pair_experiment.subprocess.run",
            side_effect=lambda *args, **kwargs: CompletedProcess(args[0], 0, next(outputs), b""),
        ):
            self.assertEqual(plan_state_sha256(path), plan_state_sha256(path))

    def test_pair_runner_rejects_configured_metis_partitioning(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            config = Path(temporary_directory) / "config.yml"
            config.write_text("modules:\n  partitioning:\n    method: !Metis\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "method: none"):
                require_no_metis_partitioning(config, [])

    def test_pair_runner_accepts_none_partitioning(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            config = Path(temporary_directory) / "config.yml"
            config.write_text("modules:\n  partitioning:\n    method: None\n", encoding="utf-8")
            require_no_metis_partitioning(config, [])

    def test_pair_runner_rejects_metis_override(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            config = Path(temporary_directory) / "config.yml"
            config.write_text("modules: {}\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "partitioning.method"):
                require_no_metis_partitioning(config, ["partitioning.method=Metis"])

    def test_pair_runner_rejects_inline_metis_config(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            config = Path(temporary_directory) / "config.yml"
            config.write_text("partitioning: {method: !Metis}\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "method: none"):
                require_no_metis_partitioning(config, [])

    def test_pair_runner_rejects_flow_and_quoted_partitioning_keys(self):
        for source in (
            'modules: {partitioning: {num_parts: 2, method: !Metis {}}}\n',
            '"partitioning":\n  method: !Metis\n',
        ):
            with self.subTest(source=source), tempfile.TemporaryDirectory() as temporary_directory:
                config = Path(temporary_directory) / "config.yml"
                config.write_text(source, encoding="utf-8")
                with self.assertRaises(ValueError):
                    require_no_metis_partitioning(config, [])

    def test_pair_runner_rejects_quoted_or_flow_method_values_and_skips_comments(self):
        for source in (
            'partitioning:\n  "method": !Metis {}\n',
            "partitioning:\n  # comment before setting\n  {num_parts: 2, method: !Metis {}}\n",
        ):
            with self.subTest(source=source), tempfile.TemporaryDirectory() as temporary_directory:
                config = Path(temporary_directory) / "config.yml"
                config.write_text(source, encoding="utf-8")
                with self.assertRaises(ValueError):
                    require_no_metis_partitioning(config, [])

    def test_pair_runner_rejects_unicode_escaped_yaml_keys(self):
        for source in (
            'modules:\n  "partitio\\u006eing":\n    method: !Metis {}\n',
            'partitioning:\n  "meth\\u006fd": !Metis {}\n',
            'modules:\n  "partitio\\x6eing":\n    method: !Metis {}\n',
            'partitioning:\n  "me\\x74hod": !Metis {}\n',
            'modules:\n  ? "parti\\\n    tioning"\n  :\n    method: !Metis {}\n',
        ):
            with self.subTest(source=source), tempfile.TemporaryDirectory() as temporary_directory:
                config = Path(temporary_directory) / "config.yml"
                config.write_text(source, encoding="utf-8")
                with self.assertRaises(ValueError):
                    require_no_metis_partitioning(config, [])


    def test_pair_runner_rejects_overrides_that_change_matched_conditions(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            config = Path(temporary_directory) / "config.yml"
            config.write_text("modules: {}\n", encoding="utf-8")
            for key in (
                "network.path",
                "population.path",
                "vehicles.path",
                "transit.schedule_path",
                "facilities.path",
                "ids.path",
                "partitioning.method",
                "partitioning.num_parts",
                "computational_setup.replanning_threads",
                "computational_setup.random_seed",
                "output.output_dir",
            ):
                with self.subTest(key=key), self.assertRaisesRegex(ValueError, key):
                    require_no_metis_partitioning(config, [f"{key}=other"])


if __name__ == "__main__":
    unittest.main()

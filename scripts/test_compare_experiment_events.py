import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from compare_experiment_events import summarize


class CompareExperimentEventsTests(unittest.TestCase):
    def test_partition_boundary_event_order_does_not_change_vehicle_signature(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            events_dir = Path(temporary_directory)
            (events_dir / "events.0.xml.zst").touch()
            (events_dir / "events.1.xml.zst").touch()
            leave = (10.0, {"type": "left link", "vehicle": "v", "link": "a"})
            enter = (10.0, {"type": "entered link", "vehicle": "v", "link": "b"})

            def local(path):
                return iter([leave, enter]) if path.name.endswith("0.xml.zst") else iter([])

            def migrated(path):
                return iter([enter]) if path.name.endswith("0.xml.zst") else iter([leave])

            with patch("compare_experiment_events.read_events", side_effect=local):
                baseline = summarize(events_dir)
            with patch("compare_experiment_events.read_events", side_effect=migrated):
                candidate = summarize(events_dir)

            self.assertEqual(
                baseline["canonical_events_sha256"],
                candidate["canonical_events_sha256"],
            )

    def test_summary_detects_per_person_timestamp_changes(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            events_dir = Path(temporary_directory)
            (events_dir / "events.0.xml.zst").touch()

            with patch(
                "compare_experiment_events.read_events",
                return_value=iter([(10.0, {"type": "departure", "person": "p1"})]),
            ):
                baseline = summarize(events_dir)
            with patch(
                "compare_experiment_events.read_events",
                return_value=iter([(11.0, {"type": "departure", "person": "p1"})]),
            ):
                candidate = summarize(events_dir)

            self.assertNotEqual(
                baseline["canonical_events_sha256"],
                candidate["canonical_events_sha256"],
            )

    def test_vehicle_sequence_includes_transit_driver_start(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            events_dir = Path(temporary_directory)
            (events_dir / "events.0.xml.zst").touch()
            driver_start = (10.0, {"type": "TransitDriverStarts", "driverId": "d", "vehicleId": "v"})
            vehicle_event = (10.0, {"type": "entered link", "vehicle": "v", "link": "l"})

            with patch("compare_experiment_events.read_events", return_value=iter([driver_start, vehicle_event])):
                baseline = summarize(events_dir)
            with patch("compare_experiment_events.read_events", return_value=iter([vehicle_event, driver_start])):
                candidate = summarize(events_dir)

            self.assertNotEqual(
                baseline["canonical_events_sha256"],
                candidate["canonical_events_sha256"],
            )

    def test_stranded_abandoned_and_resumed_outcomes_are_distinguished(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            events_dir = Path(temporary_directory)
            (events_dir / "events.0.xml.zst").touch()
            events = [
                (1.0, {"type": "stuckAndAbort", "person": "resumed"}),
                (2.0, {"type": "actstart", "person": "resumed"}),
                (3.0, {"type": "stuckAndAbort", "person": "stranded"}),
            ]
            with patch("compare_experiment_events.read_events", return_value=iter(events)):
                summary = summarize(events_dir)

            self.assertEqual(summary["stranded_agents"], 2)
            self.assertEqual(summary["abandoned_legs"], 2)
            self.assertEqual(summary["resumed_agents"], 1)


if __name__ == "__main__":
    unittest.main()

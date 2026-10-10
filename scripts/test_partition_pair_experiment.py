import tempfile
import unittest
from pathlib import Path
import sys
from subprocess import CompletedProcess
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
from partition_pair_experiment import parse_networks, plan_state_sha256


class PartitionPairExperimentTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()

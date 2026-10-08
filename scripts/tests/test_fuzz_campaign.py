import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "fuzz-campaign.py"
SPEC = importlib.util.spec_from_file_location("fuzz_campaign", SCRIPT)
campaign = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(campaign)


class EvidenceInventoryTests(unittest.TestCase):
    def test_repository_inventory_uses_pinned_nix_directory(self):
        hashes = campaign.source_hashes()
        self.assertIn("nix/flake.nix", hashes)
        self.assertIn("nix/flake.lock", hashes)
        self.assertIn("tests/support/vhdx.rs", hashes)
        self.assertIn("tests/support/vhdx_chain.rs", hashes)
        self.assertNotIn("flake.nix", hashes)
        self.assertTrue(all(len(digest) == 64 for digest in hashes.values()))

    def test_missing_inventory_refuses_before_creating_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "fuzz").mkdir()
            (root / "fuzz/targets.json").write_text("{}")
            output = root / "campaign"
            with patch.object(campaign, "ROOT", root), patch.object(
                campaign, "source_hashes", side_effect=lambda: campaign_hashes(root)
            ), self.assertRaisesRegex(SystemExit, "Missing fuzz evidence files"):
                campaign.main(["--output", str(output)])
            self.assertFalse(output.exists())


campaign_hashes = campaign.source_hashes

if __name__ == "__main__":
    unittest.main()

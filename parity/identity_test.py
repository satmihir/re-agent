"""Reference selection rejects implicit or cross-platform oracle identities."""

import json
from pathlib import Path
import unittest
from unittest.mock import patch

from parity import identity, run


def fixture_manifest(**fields):
    """Give boundary fixtures explicit identities without relying on the host manifest."""
    result = {"reference_commit": "a" * 40, "reference_build_flags": ["-trimpath", "-buildvcs=false"], **fields}
    result["platforms"] = {name: {"reference_binary_sha256": fields.get("reference_binary_sha256", "b" * 64),
                                 "go_version": fields.get("go_version", "go fixture version"),
                                 "python_version": fields.get("python_version", "Python fixture"),
                                 "normalized_lock": "expected/" + name.split("/")[0] + ".json",
                                 "normalized_tree_sha256": fields.get("normalized_tree_sha256")}
                           for name in identity.SUPPORTED}
    result["reference_platform"] = fields.get("reference_platform", identity.current_platform())
    return result


class IdentityTests(unittest.TestCase):
    def test_selects_only_actual_platform_and_rejects_missing_identity(self):
        manifest = fixture_manifest()
        manifest["platforms"]["darwin/arm64"]["reference_binary_sha256"] = "d" * 64
        with patch("sys.platform", "linux"), patch("platform.machine", return_value="x86_64"):
            self.assertEqual(identity.select(manifest)["reference_binary_sha256"], "b" * 64)
            del manifest["platforms"]["linux/amd64"]
            with self.assertRaisesRegex(run.HarnessError, "missing reference identity for linux/amd64"):
                identity.select(manifest)

    def test_malformed_selected_hash_fails_with_a_harness_error(self):
        for value in (None, 1, [], {}, "f" * 63, "F" * 64):
            with self.subTest(value=value):
                manifest = fixture_manifest()
                manifest["platforms"][identity.current_platform()]["reference_binary_sha256"] = value
                with self.assertRaisesRegex(run.HarnessError, "invalid reference binary SHA-256"):
                    identity.select(manifest)

    def test_unsupported_architecture_never_uses_another_lane(self):
        for system, machine in (("linux", "arm64"), ("darwin", "x86_64"), ("win32", "x86_64")):
            with self.subTest(system=system, machine=machine), patch("sys.platform", system), patch("platform.machine", return_value=machine):
                with self.assertRaisesRegex(run.HarnessError, "unsupported reference platform"):
                    identity.select({})

    def test_shared_inventory_binds_its_historical_capture_not_active_lane(self):
        manifest = fixture_manifest()
        manifest["platforms"]["darwin/arm64"]["reference_binary_sha256"] = "d" * 64
        inventory = {"platform": "darwin", "reference_commit": manifest["reference_commit"],
                     "reference_binary_sha256": "d" * 64}
        with patch("sys.platform", "linux"), patch("platform.machine", return_value="x86_64"):
            run.verify_shared_inventory(inventory, manifest)
            inventory["reference_binary_sha256"] = "b" * 64
            with self.assertRaisesRegex(run.HarnessError, "shared inventory capture"):
                run.verify_shared_inventory(inventory, manifest)

    def test_darwin_identity_matches_unchanged_committed_lock(self):
        suite = Path(__file__).resolve().parent
        manifest = json.loads((suite / "manifest.json").read_bytes())
        lock = json.loads((suite / "expected/darwin.json").read_bytes())
        with patch("sys.platform", "darwin"), patch("platform.machine", return_value="arm64"):
            self.assertEqual(identity.reference(manifest), lock["reference_identity"])

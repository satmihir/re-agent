"""Reference selection rejects implicit or cross-platform oracle identities."""

import json
import sys
import tempfile
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
                                 "python_executable": str(Path(sys.executable).resolve()),
                                 "normalized_lock": "expected/" + name.split("/")[0] + ".json",
                                 "normalized_tree_sha256": fields.get("normalized_tree_sha256")}
                           for name in identity.SUPPORTED}
    result["reference_platform"] = fields.get("reference_platform", identity.current_platform())
    return result


def rust_fixture_manifest(**fields):
    """Give Rust builder tests explicit per-platform compiler identities."""
    manifest = fixture_manifest(**fields)
    manifest.update(reference_language="rust", reference_build_flags=["--release", "--locked", "--offline"],
                    reference_rustflags=["--remap-path-prefix=<archive>=/reagent", "--remap-path-prefix=<cargo_home>=/cargo"])
    for entry in manifest["platforms"].values():
        del entry["go_version"]
        entry.update(rustup_toolchain="1.99.1", cargo_version="cargo test version", rustc_version="rustc test version")
    return manifest


class IdentityTests(unittest.TestCase):
    def test_rust_identity_binds_build_inputs_without_legacy_go_fields(self):
        manifest = rust_fixture_manifest()
        reference = identity.reference(manifest)
        self.assertEqual(set(reference), set(identity.RUST_REFERENCE_FIELDS))
        self.assertEqual(reference["cargo_version"], "cargo test version")
        self.assertEqual(reference["reference_rustflags"], manifest["reference_rustflags"])
        self.assertNotIn("go_version", reference)
        for key in ("rustup_toolchain", "cargo_version", "rustc_version", "python_version"):
            for invalid in (None, "", [], 1):
                with self.subTest(key=key, invalid=invalid):
                    broken = rust_fixture_manifest()
                    broken["platforms"][identity.current_platform()][key] = invalid
                    with self.assertRaisesRegex(run.HarnessError, "toolchain identity"):
                        identity.select(broken)

    def test_historical_go_lock_identity_has_exact_original_shape(self):
        reference = identity.reference(fixture_manifest())
        self.assertEqual(set(reference), set(identity.REFERENCE_FIELDS))
        self.assertEqual(reference["go_version"], "go fixture version")
        self.assertNotIn("reference_language", reference)

    def test_unknown_reference_language_is_rejected(self):
        with self.assertRaisesRegex(run.HarnessError, "unsupported reference language"):
            identity.select(fixture_manifest(reference_language="unknown"))

    def test_linux_python_guard_rejects_missing_invalid_and_different_executable(self):
        with patch("sys.platform", "linux"), patch("platform.machine", return_value="x86_64"):
            for value in (None, "", 1, [], {}, "python3", "/different/python3.11"):
                with self.subTest(value=value):
                    manifest = fixture_manifest()
                    manifest["platforms"]["linux/amd64"]["python_executable"] = value
                    with self.assertRaisesRegex(run.HarnessError, "reference Python executable"):
                        identity.guard_python(manifest)
            with self.assertRaisesRegex(run.HarnessError, "missing or invalid reference Python executable"):
                identity.guard_python({})

    def test_linux_python_guard_accepts_alias_of_exact_resolved_interpreter(self):
        with tempfile.TemporaryDirectory(prefix="parity-python-test-") as temporary:
            manifest = fixture_manifest()
            alias = Path(temporary) / "python3"
            alias.symlink_to(Path(sys.executable).resolve())
            with patch("sys.platform", "linux"), patch("platform.machine", return_value="x86_64"), patch("sys.executable", str(alias)):
                identity.guard_python(manifest)
                self.assertNotIn("python_executable", identity.reference(manifest))

    def test_darwin_guard_keeps_historical_interpreter_and_lock_contract(self):
        manifest = fixture_manifest()
        del manifest["platforms"]["darwin/arm64"]["python_executable"]
        with patch("sys.platform", "darwin"), patch("platform.machine", return_value="arm64"):
            identity.guard_python(manifest)
            self.assertEqual(set(identity.reference(manifest)), set(identity.REFERENCE_FIELDS))

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

    def test_darwin_capture_required_blocks_reuse_until_new_identity_is_prepared(self):
        manifest = fixture_manifest()
        entry = manifest['platforms']['darwin/arm64']
        entry.update(capture_status='required', reference_commit='c' * 40)
        with patch('sys.platform', 'darwin'), patch('platform.machine', return_value='arm64'):
            for capture in (False, True):
                with self.assertRaisesRegex(run.HarnessError, 'Darwin capture required'):
                    identity.select(manifest, capture=capture)
            entry['reference_commit'] = manifest['reference_commit']
            with self.assertRaisesRegex(run.HarnessError, 'Darwin capture required'):
                identity.reference(manifest)
            self.assertEqual(identity.select(manifest, capture=True)['reference_commit'], manifest['reference_commit'])

    def test_committed_darwin_identity_is_retained_but_not_usable_when_capture_required(self):
        suite = Path(__file__).resolve().parent
        manifest = json.loads((suite / 'manifest.json').read_bytes())
        lock = json.loads((suite / 'expected/darwin.json').read_bytes())
        with patch('sys.platform', 'darwin'), patch('platform.machine', return_value='arm64'):
            if manifest['platforms']['darwin/arm64'].get('capture_status') == 'required':
                self.assertEqual(manifest['platforms']['darwin/arm64']['reference_binary_sha256'],
                                 lock['reference_identity']['reference_binary_sha256'])
                with self.assertRaisesRegex(run.HarnessError, 'Darwin capture required'):
                    identity.reference(manifest)
            else:
                self.assertEqual(identity.reference(manifest), lock['reference_identity'])

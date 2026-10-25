"""Select the exact reference identity for the executing platform."""

import platform
import re
import sys


REFERENCE_FIELDS = ("reference_commit", "reference_binary_sha256", "reference_platform",
                    "reference_build_flags", "go_version", "python_version")
SUPPORTED = ("darwin/arm64", "linux/amd64")


def current_platform():
    """Reject architectures without an independently pinned reference."""
    from parity.run import HarnessError
    machine = platform.machine()
    name = sys.platform + "/" + {"aarch64": "arm64", "x86_64": "amd64"}.get(machine, machine)
    if name not in SUPPORTED:
        raise HarnessError("unsupported reference platform: " + name)
    return name


def select(manifest):
    """Flatten the selected entry into the existing lock identity contract."""
    from parity.run import HarnessError
    name = current_platform()
    platforms = manifest.get("platforms")
    entry = platforms.get(name) if isinstance(platforms, dict) else None
    if not isinstance(entry, dict):
        raise HarnessError("missing reference identity for " + name)
    digest = entry.get("reference_binary_sha256")
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise HarnessError("missing or invalid reference binary SHA-256 for " + name)
    if not all(isinstance(entry.get(key), str) and entry[key] for key in ("go_version", "python_version")):
        raise HarnessError("missing reference toolchain identity for " + name)
    relative = "expected/" + sys.platform + ".json"
    if entry.get("normalized_lock") != relative:
        raise HarnessError("normalized lock must name " + relative)
    return {**entry, "reference_commit": manifest.get("reference_commit"),
            "reference_platform": name, "reference_build_flags": manifest.get("reference_build_flags")}


def reference(manifest):
    """Keep the committed lock identity shape unchanged."""
    selected = select(manifest)
    return {key: selected.get(key) for key in REFERENCE_FIELDS}

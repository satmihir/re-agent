"""Select the exact reference identity for the executing platform."""

from pathlib import Path
import platform
import re
import sys


REFERENCE_FIELDS = ("reference_commit", "reference_binary_sha256", "reference_platform",
                    "reference_build_flags", "go_version", "python_version")
RUST_REFERENCE_FIELDS = ("reference_commit", "reference_binary_sha256", "reference_platform",
                         "reference_language", "reference_build_flags", "reference_rustflags",
                         "rustup_toolchain", "cargo_version", "rustc_version", "python_version")
SUPPORTED = ("darwin/arm64", "linux/amd64")


def current_platform():
    """Reject architectures without an independently pinned reference."""
    from parity.run import HarnessError
    machine = platform.machine()
    name = sys.platform + "/" + {"aarch64": "arm64", "x86_64": "amd64"}.get(machine, machine)
    if name not in SUPPORTED:
        raise HarnessError("unsupported reference platform: " + name)
    return name


def guard_python(manifest):
    """Pin the Linux harness interpreter without changing historical lock identities."""
    from parity.run import HarnessError
    name = current_platform()
    if name != "linux/amd64":
        return
    platforms = manifest.get("platforms")
    entry = platforms.get(name) if isinstance(platforms, dict) else None
    expected = entry.get("python_executable") if isinstance(entry, dict) else None
    if not isinstance(expected, str) or not expected or not Path(expected).is_absolute():
        raise HarnessError("missing or invalid reference Python executable for " + name +
                           "; platforms.linux/amd64.python_executable must name the absolute resolved interpreter")
    actual = str(Path(sys.executable).resolve())
    if actual != expected:
        raise HarnessError("reference Python executable mismatch: expected %s, got %s; "
                           "run parity with the pinned interpreter" % (expected, actual))


def select(manifest, capture=False):
    """Flatten the selected entry into the existing lock identity contract."""
    from parity.run import HarnessError
    name = current_platform()
    platforms = manifest.get("platforms")
    entry = platforms.get(name) if isinstance(platforms, dict) else None
    if not isinstance(entry, dict):
        raise HarnessError("missing reference identity for " + name)
    if entry.get('capture_status') not in (None, 'required', 'ready'):
        raise HarnessError('invalid reference capture status: ' + name)
    if entry.get("capture_status") == "required" and not capture:
        raise HarnessError("Darwin capture required" if name.startswith("darwin/") else "reference capture required: " + name)
    platform_commit = (entry.get('reference_commit') if entry.get('capture_status') == 'required'
                       else entry.get('reference_commit', manifest.get('reference_commit')))
    if platform_commit != manifest.get("reference_commit"):
        raise HarnessError("Darwin capture required: prepare the new platform commit and binary identity" if name.startswith("darwin/")
                           else "platform reference commit differs: " + name)
    digest = entry.get("reference_binary_sha256")
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise HarnessError("missing or invalid reference binary SHA-256 for " + name)
    language = manifest.get("reference_language", "go")
    if language not in ("go", "rust"):
        raise HarnessError("unsupported reference language: " + str(language))
    toolchain = ("rustup_toolchain", "cargo_version", "rustc_version") if language == "rust" else ("go_version",)
    if not all(isinstance(entry.get(key), str) and entry[key] for key in (*toolchain, "python_version")):
        raise HarnessError("missing reference toolchain identity for " + name)
    relative = "expected/" + sys.platform + ".json"
    if entry.get("normalized_lock") != relative:
        raise HarnessError("normalized lock must name " + relative)
    return {**entry, "reference_commit": manifest.get("reference_commit"),
            "reference_platform": name, "reference_language": language,
            "reference_build_flags": manifest.get("reference_build_flags"),
            "reference_rustflags": entry.get("reference_rustflags", manifest.get("reference_rustflags"))}


def reference(manifest):
    """Bind Rust build inputs without changing historical Go lock identities."""
    selected = select(manifest)
    fields = RUST_REFERENCE_FIELDS if selected["reference_language"] == "rust" else REFERENCE_FIELDS
    return {key: selected.get(key) for key in fields}

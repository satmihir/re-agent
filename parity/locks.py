"""Compact locks for normalized bytes and replay-validated substitution sites."""

from collections import Counter
from concurrent.futures import ProcessPoolExecutor
import base64
import hashlib
import json
from pathlib import Path
import re
import shutil
import sys
import tempfile

from parity import identity


SUBSTITUTION_KINDS = (
    "allocated-fixture-authority", "allocated-path-or-reference", "allocated-retry-date",
    "approved-workspace-path", "auth-status-catalog-display-name", "build-metadata",
    "cache-header", "cache-prefix", "captured-transport-socket",
    "case-specific-closed-endpoint", "case-specific-closed-port-invocation-env",
    "case-specific-closed-port-legacy-trace", "duration-ms", "effect-record-workspace",
    "embedded-request", "embedded-response", "exec-timeout-elapsed", "footer-elapsed",
    "independently-validated-cache-key", "independently-validated-derived-value",
    "model-switch-latency-ms", "owned-run-reason-authority", "registered-text-span",
    "registered-fixture-cancel-delivery-projection", "retry-delay-policy",
    "retry-display-delay", "snapshot-date", "trace-time", "typed-identity",
    "typed-scope-identity", "validated-auth-refresh-expiry", "validated-derived-digest",
    "validated-derived-length", "exec-effective-timeout-elapsed", "git-do-jev-ms",
    "git-do-allocated-remote", "git-do-failed-display-remote-prefix", "clipped-allocated-path-prefix",
    "exec-allocated-executable", "exec-literal-environment",
    "exec-fixture-cwd", "exec-fixture-stdout-length", "tool-allocated-path-argument",
    "git-fixture-workspace", "tool-workspace-refusal", "tool-approved-workspace-path",
    "git-index-stat-cache", "exec-owned-elapsed", "cli-version-build-metadata", "cli-validated-preview-request-length",
    "cli-shell-workspace", "cli-shell-duration-ms", "cli-shell-elapsed",
    "tool-human-approved-workspace-path", "tool-human-workspace-echo",
    "terminal-startup-session", "terminal-startup-path", "terminal-artifact-path",
    "terminal-state-projection", "terminal-workspace-path", "cli-shell-termination", "cli-shell-status",
    "cli-reset-checkpoint", "cli-exec-executable", "terminal-exec-timeout-elapsed", "terminal-progress-spinner",
    "agent-envelope-run-reference", "agent-source-trace-path", "agent-result-source-trace-path",
    "agent-delivered-effect-workspace", "agent-exact-delivery-preview", "agent-embedded-result",
    "agent-embedded-roster", "agent-run-source-trace-path", "agent-exact-tool-preview",
    "agent-embedded-tool-outcome", "agent-router-state", "agent-embedded-request", "agent-historical-transcript",
    "checkpoint-roster-witnessed-trace", "checkpoint-legacy-exact-bytes", "checkpoint-typed-base64",
    "checkpoint-witnessed-session", "checkpoint-owned-workspace", "checkpoint-witnessed-trace",
    "checkpoint-runtime-workspace", "checkpoint-handoff-transcript", "checkpoint-gc-owned-path",
    "checkpoint-shell-duration-ms", "checkpoint-outcome-workspace", "checkpoint-derived-state-sha256",
    "checkpoint-linkage-inverse", "fixture-owned-script-path", "fixture-owned-executable-path",
    "checkpoint-agent-delivery",
    "terminal-agent-recap-workspace", "exec-embedded-owned-outcome",
    "agent-sharing-projection-workspace",
)

COMPARISON_POLICY = {
    "version": 1,
    "artifacts": {"B": "byte-exact", "N": "lossless typed substitutions then byte-exact",
                  "S": "owned semantic projection then byte-exact"},
    "substitution_kinds": list(SUBSTITUTION_KINDS),
    "ledger_contract": {
        "rows": "sorted multiset of artifact, typed path, substitution kind and multiplicity",
        "timing_sites": "include eligible top-level trace timing slots even when already zero",
        "certificates": "typed ownership and witness metadata; never raw values or raw digests",
        "closed_endpoint": "legacy trace and independent invocation certificates share one site kind",
        "validation": "complete raw replay must reproduce every original ledger row before locking",
        "excluded": ["before", "after", "offsets", "raw hashes", "raw lengths", "raw identities"],
    },
}


def canonical(value):
    """Encode lock data with one deterministic JSON spelling."""
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def digest(value):
    """Hash canonical JSON, never a raw observation."""
    return hashlib.sha256(canonical(value)).hexdigest()


def timing_sites(artifacts):
    """Register eligible zero-valued timing slots omitted by the observer's no-op fast path."""
    sites = Counter()
    for artifact, raw in artifacts.items():
        if not re.fullmatch(r"trace-[1-9][0-9]*\.jsonl", artifact):
            continue
        for line in raw.splitlines():
            event = json.loads(line)
            kind, key = None, None
            if event.get("type") == "api.attempt.finished":
                kind, key = "duration-ms", "duration_ms"
            if event.get("type") == "auto.route":
                kind, key = "duration-ms", "router_latency_ms"
            if event.get("type") in ("model.switch.finished", "model.switch.failed"):
                kind, key = "model-switch-latency-ms", "latency_ms"
            data = event.get("data", {})
            if key is not None and key in data:
                if type(data[key]) not in (int, float) or data[key] != 0:
                    raise AssertionError("noncanonical trace timing slot: " + artifact)
                sites[(artifact, ("data", key), kind)] += 1
    return sites


def ledger_contract(ledger, artifacts):
    """Project replay-verified rows to typed sites, excluding every raw value and raw digest."""
    if not isinstance(ledger, list):
        raise AssertionError("invalid substitution ledger")
    sites, certificates, observed_timing = Counter(), [], Counter()
    eligible_timing = timing_sites(artifacts)
    for index, row in enumerate(ledger):
        if not isinstance(row, dict):
            raise AssertionError("invalid substitution ledger row")
        if row.keys() == {"frozen_evidence"} and index == len(ledger) - 1:
            continue
        if row.keys() == {"artifact", "raw_normalized_sha256", "normalized_length"}:
            name = row["artifact"]
            if name not in artifacts or hashlib.sha256(artifacts[name]).hexdigest() != row["raw_normalized_sha256"] or len(artifacts[name]) != row["normalized_length"]:
                raise AssertionError("normalized ledger artifact differs: " + str(name))
            continue
        artifact, kind = row.get("artifact"), row.get("kind")
        if not isinstance(artifact, str):
            raise AssertionError("substitution row lacks artifact")
        if kind == "registered-fixture-cancel-delivery":
            fields = ("artifact", "kind", "classification", "case", "phase", "witness")
            if row.get("classification") != "S" or not all(isinstance(row.get(key), str) for key in fields):
                raise AssertionError("invalid cancellation certificate")
            certificate = {key: row[key] for key in fields}
            request = row.get("request", {})
            if not all(isinstance(request.get(key), str) for key in ("method", "path")):
                raise AssertionError("invalid cancellation request certificate")
            certificate["request"] = {key: request[key] for key in ("method", "path")}
            certificates.append(certificate)
            continue
        if kind == 'agent-causal-correspondence':
            expected = {'artifact', 'kind', 'classification', 'trace_sources', 'http_source_order', 'logical_slots'}
            order, slots, sources = row.get('http_source_order'), row.get('logical_slots'), row.get('trace_sources')
            if (set(row) != expected or row.get('classification') != 'S'
                    or not isinstance(order, list) or sorted(order) != list(range(len(order)))
                    or not isinstance(slots, list) or len(slots) != len(order)
                    or not isinstance(sources, list) or len(set(sources)) != len(sources)
                    or any(not isinstance(source, str) for source in sources)):
                raise AssertionError('invalid agent causal correspondence certificate')
            for slot in slots:
                if (slot is not None and (not isinstance(slot, (list, tuple)) or len(slot) != 4
                        or type(slot[0]) is not int or slot[0] < 0 or not isinstance(slot[1], str)
                        or not re.fullmatch('RUN_[1-9][0-9]*', slot[1])
                        or type(slot[2]) is not int or slot[2] < 1 or type(slot[3]) is not int or slot[3] < 1)):
                    raise AssertionError('invalid agent logical request slot')
            certificates.append(dict(artifact=artifact, kind=kind, classification='S',
                                     trace_count=len(sources), logical_slots=slots))
            continue
        if kind == "independently-validated-failed-admission-guard":
            if row.get("classification") != "S" or not isinstance(row.get("guard"), dict) or not isinstance(row.get("reason"), str):
                raise AssertionError("invalid failed-admission certificate")
            certificates.append({key: row[key] for key in ("artifact", "kind", "classification", "guard", "reason")})
            continue
        if kind not in SUBSTITUTION_KINDS:
            raise AssertionError("unregistered substitution kind: " + str(kind))
        path = row.get("path")
        required = {"artifact", "kind", "path", "start", "end", "before", "after",
                    "before_sha256", "after_sha256", "before_length", "after_length"}
        optional = {"case", "classification"}
        if kind == 'checkpoint-linkage-inverse':
            proof = {'raw_state_sha256', 'substituted_state_sha256', 'raw_checksum_valid',
                     'corruption_delta', 'inverse_verified'}
            required |= proof
            if not required <= row.keys() or row.keys() - required - optional:
                raise AssertionError('invalid substitution row fields')
            if (row.get('inverse_verified') is not True
                    or row.get('corruption_delta') not in (None, 'checksum-prefix-zero',
                        'envelope-version-99', 'envelope-id-fedcba9876543210')
                    or row.get('raw_checksum_valid') is not (row.get('corruption_delta') != 'checksum-prefix-zero')
                    or any(not isinstance(row.get(key), str) or not re.fullmatch('[0-9a-f]{64}', row[key])
                           for key in ('raw_state_sha256', 'substituted_state_sha256'))):
                raise AssertionError('invalid checkpoint inverse linkage proof')
            certificates.append(dict(artifact=artifact, kind=kind, inverse_verified=True,
                raw_checksum_valid=row['raw_checksum_valid'], corruption_delta=row['corruption_delta']))
        if kind == "terminal-state-projection":
            required |= {"classification", "certificate"}
            optional = set()
        if not required <= row.keys() or row.keys() - required - optional:
            raise AssertionError("invalid substitution row fields")
        if not isinstance(path, list) or any(type(item) not in (str, int) for item in path):
            raise AssertionError("invalid substitution path")
        if kind == "terminal-state-projection":
            owner = re.fullmatch(r"invoke-([0-9]{3})\.(?:tty|terminal\.json)|invocation-([1-9][0-9]{0,2})\.stderr", artifact)
            number = int(owner[1] or owner[2]) if owner else 0
            witness = "invoke-%03d.screens.json" % number
            if (not number or path != ["terminal-state-projection"] or row["classification"] != "S"
                    or row["certificate"] != witness or witness not in artifacts):
                raise AssertionError("invalid terminal projection certificate")
            certificates.append({key: row[key] for key in ("artifact", "kind", "classification", "certificate")})
        if kind.startswith("case-specific-closed-port-"):
            kind = "case-specific-closed-port"
        site = (artifact, tuple(path), kind)
        top_timing = (re.fullmatch(r"trace-[1-9][0-9]*\.jsonl", artifact)
                      and ((kind == "duration-ms" and path in (["data", "duration_ms"], ["data", "router_latency_ms"]))
                           or (kind == "model-switch-latency-ms" and path == ["data", "latency_ms"])))
        if top_timing:
            if site not in eligible_timing:
                raise AssertionError("substitution timing row lacks a registered site")
            observed_timing[site] += 1
        else:
            sites[site] += 1
    if observed_timing - eligible_timing:
        raise AssertionError("substitution timing rows exceed registered sites")
    sites.update(eligible_timing)
    rows = [[artifact, list(path), kind, count] for (artifact, path, kind), count in sites.items()]
    return {"version": 1, "sites": sorted(rows, key=canonical),
            "certificates": sorted(certificates, key=canonical)}


def case_lock(artifacts, policies, ledger):
    """Lock normalized artifact names, bytes, lengths, policies and the typed ledger contract."""
    if not artifacts or policies.keys() != artifacts.keys() or any(value not in ("B", "N", "S") for value in policies.values()):
        raise AssertionError("incomplete normalized artifacts or comparison policies")
    rows = [[name, len(raw), hashlib.sha256(raw).hexdigest(), policies[name]]
            for name, raw in sorted(artifacts.items())]
    return {"artifacts": rows, "normalized_sha256": digest(rows),
            "ledger_sha256": digest(ledger_contract(ledger, artifacts))}


def frozen_lock(expected):
    """Replay all raw evidence before deriving a committed normalized-only case lock."""
    from parity.run import read_frozen_case, validate_replay
    documents, ledger, captured = read_frozen_case(expected)
    policies = json.loads(documents["artifact-policies.json"])
    validate_replay(captured["raw"], json.loads(documents["replay.json"]),
                    captured["normalized"], policies, ledger[:-1])
    return case_lock(captured["normalized"], policies, ledger)


def tree_digest(cases):
    """Hash the ordered logical normalized tree using the original independent audit algorithm."""
    tree = hashlib.sha256()
    for case_id, lock in sorted(cases.items()):
        for name, length, sha256, _ in sorted(lock["artifacts"]):
            tree.update((case_id + "/" + name).encode() + b"\0" + str(length).encode() + b"\0" + bytes.fromhex(sha256))
    return tree.hexdigest()


def pack(cases):
    """Intern repeated normalized artifact descriptors without compressing reviewable JSON."""
    descriptors = sorted({tuple(row[1:]) for lock in cases.values() for row in lock["artifacts"]})
    indexes = {value: index for index, value in enumerate(descriptors)}
    return {"version": 1, "artifact_values": [list(value) for value in descriptors],
            "cases": {case_id: {**lock, "artifacts": [[row[0], indexes[tuple(row[1:])]] for row in lock["artifacts"]]}
                      for case_id, lock in sorted(cases.items())}, "normalized_tree_sha256": tree_digest(cases)}


def unpack(document):
    """Expand compact locks, rejecting corrupt descriptors and inconsistent tree digests."""
    if document.get("version") != 1:
        raise AssertionError("unsupported normalized lock version")
    values, cases = document["artifact_values"], {}
    for case_id, lock in document["cases"].items():
        try:
            artifacts = [[name, *values[index]] for name, index in lock["artifacts"]
                         if type(index) is int and 0 <= index < len(values)]
            if len(artifacts) != len(lock["artifacts"]) or digest(artifacts) != lock["normalized_sha256"]:
                raise AssertionError("artifact descriptors or normalized digest differ")
            cases[case_id] = {**lock, "artifacts": artifacts}
        except (AssertionError, IndexError, TypeError, ValueError) as exc:
            raise AssertionError("invalid normalized lock for " + case_id + ": " + str(exc)) from exc
    if tree_digest(cases) != document["normalized_tree_sha256"]:
        raise AssertionError("normalized lock tree digest differs")
    return cases


def verify_case(case_id, expected, artifacts, policies, ledger):
    """Name the case whose normalized artifacts, policies or typed substitution sites differ."""
    actual = case_lock(artifacts, policies, ledger)
    if actual != expected:
        fields = sorted(key for key in expected.keys() | actual.keys() if expected.get(key) != actual.get(key))
        raise AssertionError("normalized lock mismatch for " + case_id + ": " + ", ".join(fields))


def verify_inventory(expected, actual):
    """Fail with complete missing and extra case IDs, never silently narrow an oracle."""
    missing, extra = sorted(set(expected) - set(actual)), sorted(set(actual) - set(expected))
    if missing or extra:
        raise AssertionError("normalized case inventory differs; missing: %s; extra: %s" %
                             (", ".join(missing) or "none", ", ".join(extra) or "none"))


def validate_policy(policy):
    """Bind the detailed reviewable allowlist to every supported substitution and certificate kind."""
    expected = set(SUBSTITUTION_KINDS) | {"registered-fixture-cancel-delivery",
                                        "independently-validated-failed-admission-guard", "agent-causal-correspondence"}
    if not isinstance(policy, dict) or policy.get("schema_version") != 1:
        raise AssertionError("unsupported comparison policy version")
    rows = policy.get("typed_allowlist")
    if not isinstance(rows, list) or any(not isinstance(row, dict) or not isinstance(row.get("kind"), str) for row in rows):
        raise AssertionError("invalid typed substitution allowlist")
    kinds = [row["kind"] for row in rows]
    missing, extra = sorted(expected - set(kinds)), sorted(set(kinds) - expected)
    if missing or extra or len(kinds) != len(set(kinds)):
        raise AssertionError("comparison policy kinds differ; missing=%s extra=%s duplicate=%s" %
                             (missing, extra, len(kinds) != len(set(kinds))))
    if "ledger_contract" in policy and policy["ledger_contract"] != COMPARISON_POLICY["ledger_contract"]:
        raise AssertionError("comparison policy ledger contract differs")


REFERENCE_FIELDS = identity.REFERENCE_FIELDS
REPIN_REFERENCE = "2ab234f79d1d42ba8a969ba3f4103eeeba717967"
RUST_REPIN_REFERENCE = "e6c9571396af1656c48a089e06b293165828e0c0"
REPIN_RECEIPT = "repin-receipt.json"


def read_lock(suite, manifest):
    """Load committed normalized locks and bind their inventory, policy and pinned identity."""
    from parity.run import HarnessError, verify_shared_inventory
    suite = Path(suite)
    selected = identity.select(manifest)
    relative = selected["normalized_lock"]
    try:
        document = json.loads((suite / relative).read_bytes())
        inventory = json.loads((suite / "inventory.json").read_bytes())
        expected = [case["id"] for case in inventory["cases"]
                    if case["status"] == "implemented" and sys.platform in case["platforms"]]
        if len(expected) != len(set(expected)):
            raise AssertionError("duplicate executable case in committed inventory")
        if sys.platform == 'darwin' and set(expected) - document['cases'].keys():
            raise HarnessError('new leaves need a Darwin capture')
        verify_inventory(expected, document["cases"])
        cases = unpack(document)
        validate_policy(json.loads((suite / "policy.json").read_bytes()))
        if document.get("reference_identity") != identity.reference(manifest):
            raise AssertionError("normalized lock pinned reference identity differs from manifest")
        verify_shared_inventory(inventory, manifest)
        declared = manifest.get("leaf_inventory")
        if declared is not None and any(inventory.get(key) != declared.get(key) for key in ("metadata_fields", "metadata_sha256")):
            raise AssertionError("shared inventory metadata differs from manifest")
        if selected.get("normalized_tree_sha256") != document["normalized_tree_sha256"]:
            raise AssertionError("normalized lock tree differs from manifest")
        return {**document, "cases": cases}
    except (OSError, ValueError, KeyError, TypeError, AssertionError) as exc:
        raise HarnessError("normalized oracle lock: " + str(exc)) from exc


def update_lock(suite, manifest, staging, cases, jobs=4):
    """Explicitly update locks, inventory and selected samples only after complete raw replay."""
    from parity import run, samples
    suite, staging = Path(suite), Path(staging)
    selected = identity.select(manifest)
    run.verify_inventory(staging, cases, manifest)
    executable = sorted(case.id for case in cases if not case.pending and sys.platform in case.platforms)
    # The verification stamp is cache metadata, never a case.
    actual = {path.name for path in staging.iterdir()} - {"inventory.json", "cache.json", "reference", "verified.json", REPIN_RECEIPT}
    verify_inventory(executable, actual)
    relative = selected["normalized_lock"]
    path = suite / relative
    previous_document = json.loads(path.read_bytes()) if path.exists() else {}
    old_cases = unpack(previous_document) if previous_document else {}
    policy = json.loads((suite / "policy.json").read_bytes())
    validate_policy(policy)
    policy["ledger_contract"] = COMPARISON_POLICY["ledger_contract"]
    if jobs == 1:
        new_cases = {case_id: frozen_lock(staging / case_id) for case_id in executable}
    else:
        with ProcessPoolExecutor(max_workers=max(jobs, run.replay_jobs())) as pool:
            futures = {case_id: pool.submit(frozen_lock, staging / case_id) for case_id in executable}
            new_cases = {case_id: future.result() for case_id, future in futures.items()}
    timing_updates, shell_updates, exec_updates, race_updates = [], [], [], []
    repin_receipt = None
    if manifest['reference_commit'] == REPIN_REFERENCE:
        allowlist = json.loads((suite / 'fixtures/repin-2ab234f7.json').read_bytes())
        repin_receipt = verify_repin(old_cases, new_cases, staging, allowlist,
                                     previous_document.get('reference_identity', {}), identity.reference(manifest), cases=cases)
    elif manifest['reference_commit'] == RUST_REPIN_REFERENCE:
        allowlist = json.loads((suite / 'fixtures/repin-rust-e6c95713.json').read_bytes())
        removals = (json.loads((suite / 'fixtures/covered-removals-rust-e6c95713.json').read_bytes())
                    if old_cases.keys() - new_cases.keys() else None)
        repin_receipt = verify_rust_repin(old_cases, new_cases, staging, allowlist,
                                          previous_document.get('reference_identity', {}), identity.reference(manifest),
                                          removal_allowlist=removals)
    else:
        protected_cases, timing_updates = approved_zero_exec_timing_updates(old_cases, new_cases, staging)
        protected_cases, shell_updates = approved_shell_timing_updates(protected_cases, new_cases, staging)
        protected_cases, exec_updates = approved_exec_ledger_updates(protected_cases, new_cases, staging)
        protect_wire_locks(protected_cases, new_cases)
        protect_previous_locks(protected_cases, new_cases)
    document = {**pack(new_cases), "reference_identity": identity.reference(manifest)}
    changed = sorted(case_id for case_id in old_cases.keys() & new_cases.keys() if old_cases[case_id] != new_cases[case_id])
    report = {"changed_case_digests": [{"id": case_id,
               "before": {key: old_cases[case_id][key] for key in ("normalized_sha256", "ledger_sha256")},
               "after": {key: new_cases[case_id][key] for key in ("normalized_sha256", "ledger_sha256")}}
              for case_id in changed], "missing_case_digests": sorted(old_cases.keys() - new_cases.keys()),
              "extra_case_digests": sorted(new_cases.keys() - old_cases.keys())}
    # A caller seals the suite only after every generated file has been published.
    with tempfile.TemporaryDirectory(prefix="parity-lock-update-") as temporary:
        prepared = Path(temporary)
        (prepared / "expected").mkdir()
        if (suite / "samples").exists():
            shutil.copytree(suite / "samples", prepared / "samples")
        inventory = json.loads((staging / "inventory.json").read_bytes())
        if (suite / "inventory.json").exists():
            previous = json.loads((suite / "inventory.json").read_bytes())
            if repin_receipt is not None:
                verify_previous_inventory(suite, previous, manifest, allowlist)
                if manifest['reference_commit'] == RUST_REPIN_REFERENCE:
                    verify_rust_pending_inventory(previous, inventory)
                    if repin_receipt.get('covered_removals'):
                        verify_covered_removal_inventory(previous, inventory, repin_receipt['covered_removals'])
            else:
                run.verify_shared_inventory(previous, manifest)
                inventory.update({key: previous[key] for key in ("platform", "reference_binary_sha256")})
        (prepared / "inventory.json").write_text(json.dumps(inventory, ensure_ascii=False, indent=2) + "\n")
        approved_samples = {row['id'] for row in repin_receipt['updates']
                            if row['full_replay_verified']} if repin_receipt else set()
        selected_samples = samples.sample_directory(prepared)
        # Only already replayed replacements may pass the historical W/T sample guards.
        for case_id in approved_samples:
            (selected_samples / (case_id + '.tar.gz')).unlink(missing_ok=True)
        sample_changes = samples.refresh_samples(staging, prepared)
        for original in (suite / "samples").rglob("*.tar.gz"):
            replacement = prepared / original.relative_to(suite)
            approved = (replacement.parent == selected_samples
                        and original.name.removesuffix('.tar.gz') in approved_samples)
            if not replacement.exists() or (replacement.read_bytes() != original.read_bytes() and not approved):
                raise AssertionError("existing sample identity violation: " + str(original.relative_to(suite)))
        for original in (suite / 'samples').rglob('index.json'):
            replacement = prepared / original.relative_to(suite)
            if (original.parent != suite / selected_samples.relative_to(prepared)
                    and (not replacement.exists() or replacement.read_bytes() != original.read_bytes())):
                raise AssertionError('existing sample index identity violation: ' + str(original.relative_to(suite)))
        (prepared / relative).write_bytes(canonical(document) + b"\n")
        (prepared / "policy.json").write_text(json.dumps(policy, ensure_ascii=False, indent=2) + "\n")
        generated = ["inventory.json", "policy.json",
                     *samples.generated_paths(prepared), relative]
        for name in generated:
            destination = suite / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(prepared / name, destination)
    if repin_receipt is not None:
        (staging / REPIN_RECEIPT).write_bytes(canonical(repin_receipt) + b'\n')
    manifest["platforms"][selected["reference_platform"]]["normalized_tree_sha256"] = document["normalized_tree_sha256"]
    print(json.dumps({**report, "changed_sample_cases": sample_changes,
                      "approved_zero_exec_timing_updates": timing_updates,
                      "approved_shell_timing_updates": shell_updates,
                      "approved_exec_ledger_updates": exec_updates,
                      "approved_race_fixture_updates": race_updates,
                      "repin_receipt": repin_receipt,
                      "normalized_tree_sha256": document["normalized_tree_sha256"]}, indent=2))
    return {**document, "cases": new_cases, "approved_zero_exec_timing_updates": timing_updates,
            "approved_shell_timing_updates": shell_updates, "approved_exec_ledger_updates": exec_updates,
            "approved_race_fixture_updates": race_updates, "repin_receipt": repin_receipt}


def verify_previous_inventory(suite, inventory, manifest, allowlist):
    """Authenticate a shared historical capture against its own retained platform lock."""
    from parity import run
    system = inventory.get('platform')
    names = [name for name in identity.SUPPORTED if name.split('/')[0] == system]
    if len(names) != 1:
        raise run.HarnessError('unknown previous inventory platform')
    if inventory.get('reference_commit') == manifest['reference_commit']:
        entry = manifest.get('platforms', {}).get(names[0], {})
        if entry.get('reference_commit', manifest['reference_commit']) != manifest['reference_commit']:
            raise run.HarnessError('previous inventory mislabels an uncaptured platform identity')
        run.verify_shared_inventory(inventory, manifest)
        return
    previous = json.loads((Path(suite) / 'expected' / (system + '.json')).read_bytes())['reference_identity']
    if (previous.get('reference_platform') != names[0]
            or previous.get('reference_commit') not in (allowlist['previous_reference_commit'], allowlist['reference_commit'])):
        raise run.HarnessError('previous inventory lock reference identity differs')
    historical = dict(manifest, reference_commit=previous['reference_commit'],
                      platforms={names[0]: previous})
    run.verify_shared_inventory(inventory, historical)


def verify_repin(old_cases, new_cases, staging, allowlist, previous_identity, new_identity, cases=()):
    """Bind runnable replacements to raw replay and explicit pending transitions to fresh metadata."""
    if (allowlist.get('schema_version') != 1
            or allowlist.get('previous_reference_commit') != '60fda0d3f3eabc76fa11deed72aed7850532c97e'
            or allowlist.get('reference_commit') != REPIN_REFERENCE
            or new_identity.get('reference_commit') != REPIN_REFERENCE
            or previous_identity.get('reference_commit') not in (allowlist.get('previous_reference_commit'), REPIN_REFERENCE)
            or previous_identity.get('reference_platform') != new_identity.get('reference_platform')):
        raise AssertionError('re-pin allowlist reference identity differs')
    platform = new_identity['reference_platform']
    approved = allowlist.get('platforms', {}).get(platform)
    if not isinstance(approved, dict):
        raise AssertionError('invalid re-pin platform allowlist: ' + platform)
    pending = {}
    for case_id, entry in approved.items():
        if isinstance(entry, str) and entry.strip():
            continue
        if (not isinstance(entry, dict) or entry.keys() != {'change', 'reason'}
                or entry['change'] != 'pending' or not isinstance(entry['reason'], str)
                or not entry['reason'].strip()):
            raise AssertionError('invalid re-pin platform allowlist: ' + platform)
        pending[case_id] = entry['reason']
    removed = sorted(old_cases.keys() - new_cases.keys())
    changed = sorted(case_id for case_id in new_cases if old_cases.get(case_id) != new_cases[case_id])
    unlisted = sorted(set(changed) - (approved.keys() - pending.keys()))
    unlisted_removed = sorted(set(removed) - pending.keys())
    if unlisted_removed or unlisted:
        raise AssertionError('unlisted re-pin change: removed=%s changed=%s' % (unlisted_removed, unlisted))
    metadata = pending_repin_metadata(staging, cases, pending, new_identity, new_cases) if pending else {}
    updates = []
    for case_id in removed:
        previous, actual = old_cases[case_id], metadata[case_id]
        updates.append(dict(id=case_id, reason=pending[case_id], status='pending',
                            old_case_sha256=digest(previous), new_case_sha256=digest(actual),
                            before=previous, after=actual, full_replay_verified=False))
    for case_id in changed:
        previous, actual = old_cases.get(case_id), new_cases[case_id]
        if frozen_lock(Path(staging) / case_id) != actual:
            raise AssertionError('re-pin replay evidence differs: ' + case_id)
        updates.append(dict(id=case_id, reason=approved[case_id],
                            old_case_sha256=digest(previous) if previous is not None else None,
                            new_case_sha256=digest(actual),
                            before=previous, after=actual, full_replay_verified=True))
    return dict(schema_version=1, platform=platform, allowlist_sha256=digest(allowlist),
                previous_reference_identity=previous_identity, reference_identity=new_identity,
                replayed_cases=len(new_cases), updates=sorted(updates, key=lambda row: row['id']))


def verify_rust_repin(old_cases, new_cases, staging, allowlist, previous_identity, new_identity, removal_allowlist=None):
    """Authorize only named existing-leaf replacements backed by complete raw replay."""
    if (allowlist.get('schema_version') != 1
            or allowlist.get('previous_reference_commit') != REPIN_REFERENCE
            or allowlist.get('reference_commit') != RUST_REPIN_REFERENCE
            or new_identity.get('reference_commit') != RUST_REPIN_REFERENCE
            or new_identity.get('reference_language') != 'rust'
            or previous_identity.get('reference_commit') not in (REPIN_REFERENCE, RUST_REPIN_REFERENCE)
            or previous_identity.get('reference_platform') != new_identity.get('reference_platform')):
        raise AssertionError('Rust re-pin allowlist reference identity differs')
    platform = new_identity['reference_platform']
    approved = allowlist.get('platforms', {}).get(platform)
    if (not isinstance(approved, dict)
            or any(not isinstance(reason, str) or not reason.strip() for reason in approved.values())):
        raise AssertionError('invalid Rust re-pin platform allowlist: ' + platform)
    timing = allowlist.get('shell_timing_ledger', {}).get(platform, {})
    if (not isinstance(timing, dict)
            or any(not isinstance(reason, str) or not reason.strip() for reason in timing.values())):
        raise AssertionError('invalid Rust shell timing allowlist')
    markdown = allowlist.get('markdown_progress_ledger', {}).get(platform, {})
    if (not isinstance(markdown, dict) or set(markdown) - {'C42.markdown-progress'}
            or any(not isinstance(reason, str) or not reason.strip() for reason in markdown.values())):
        raise AssertionError('invalid Rust markdown progress allowlist')
    removed = sorted(old_cases.keys() - new_cases.keys())
    added = sorted(new_cases.keys() - old_cases.keys())
    changed = sorted(case_id for case_id in old_cases.keys() & new_cases.keys()
                     if old_cases[case_id] != new_cases[case_id])
    unlisted = sorted(set(changed) - approved.keys() - timing.keys() - markdown.keys())
    if (removed and removal_allowlist is None) or added or unlisted:
        raise AssertionError('unlisted Rust re-pin change: removed=%s added=%s changed=%s' %
                             (removed, added, unlisted))
    covered = verify_covered_removals(old_cases, new_cases, staging, removal_allowlist,
                                     previous_identity, new_identity) if removed else []
    updates = []
    for case_id in changed:
        previous, actual = old_cases[case_id], new_cases[case_id]
        if frozen_lock(Path(staging) / case_id) != actual:
            raise AssertionError('Rust re-pin replay evidence differs: ' + case_id)
        proof = {}
        if case_id in timing:
            proof = verify_rust_shell_timing(case_id, previous, actual, Path(staging) / case_id)
        if case_id in markdown:
            proof = verify_markdown_progress_ledger(previous, actual, Path(staging) / case_id, previous_identity)
        updates.append(dict(id=case_id, reason=markdown.get(case_id, timing.get(case_id, approved.get(case_id))),
                            old_case_sha256=digest(previous), new_case_sha256=digest(actual),
                            before=previous, after=actual, full_replay_verified=True, **proof))
    return dict(schema_version=1, platform=platform, allowlist_sha256=digest(allowlist),
                previous_reference_identity=previous_identity, reference_identity=new_identity,
                replayed_cases=len(new_cases), updates=updates, covered_removals=covered,
                covered_removals_allowlist_sha256=digest(removal_allowlist) if covered else None)


def verify_covered_removals(old_cases, new_cases, staging, allowlist, previous_identity, new_identity):
    """Authorize the reviewed W08 covering set and one exact duplicate timeout removal."""
    expected = {'T34.redirected-group': ['T32.timeout']}
    for zone in ('America-Los_Angeles', 'Etc-GMT+7'):
        for readonly, plan in ((0, 1), (1, 0), (1, 1)):
            for suffix in ('', '-proxy', '-anthropic'):
                case_id = f'W08.{zone}-ro{readonly}-plan{plan}{suffix}'
                expected[case_id] = [f'W08.{zone}-ro0-plan0{suffix}', f'W08.UTC-ro{readonly}-plan{plan}{suffix}']
    previous_commit = previous_identity.get('reference_commit')
    if (allowlist.get('schema_version') != 1 or allowlist.get('reference_commit') != RUST_REPIN_REFERENCE
            or allowlist.get('previous_reference_commits') != [REPIN_REFERENCE, RUST_REPIN_REFERENCE]
            or allowlist.get('removed') != expected or not isinstance(allowlist.get('reason'), str)
            or not allowlist['reason'].strip()
            or previous_commit not in (REPIN_REFERENCE, RUST_REPIN_REFERENCE)
            or new_identity.get('reference_commit') != RUST_REPIN_REFERENCE
            or new_identity.get('reference_language') != 'rust'
            or previous_identity.get('reference_platform') != new_identity.get('reference_platform')
            or (previous_commit == RUST_REPIN_REFERENCE and previous_identity != new_identity)):
        raise AssertionError('covered removal authorization or reference identity differs')
    removed = sorted(old_cases.keys() - new_cases.keys())
    if set(removed) - expected.keys():
        raise AssertionError('unlisted covered removal')
    targets = sorted({target for case_id in removed for target in expected[case_id]})
    for target in targets:
        if target not in old_cases or target not in new_cases or old_cases[target] != new_cases[target]:
            raise AssertionError('covered removal target is missing or changed: ' + target)
        if frozen_lock(Path(staging) / target) != new_cases[target]:
            raise AssertionError('covered removal target raw replay differs: ' + target)
    return [dict(id=case_id, old_case_sha256=digest(old_cases[case_id]), reason=allowlist['reason'],
                 covered_by=[dict(id=target, case_sha256=digest(new_cases[target]), full_replay_verified=True)
                             for target in expected[case_id]]) for case_id in removed]


def verify_covered_removal_inventory(previous, current, removals):
    """Permit only authorized removed rows, never edits to retained metadata."""
    old = {row['id']: row for row in previous['cases']}
    new = {row['id']: row for row in current['cases']}
    approved = {row['id'] for row in removals}
    if (previous['metadata_fields'] != current['metadata_fields'] or new.keys() - old.keys()
            or (old.keys() - new.keys()) - approved
            or any(old[case_id] != row for case_id, row in new.items())):
        raise AssertionError('covered removal inventory changes retained metadata')


def inverse_markdown_progress_ledger(ledger):
    """Undo only the one C42 spinner slot and its two following footer indices."""
    restored, spinner, footers = [], 0, set()
    for row in ledger:
        if row.get('artifact') != 'invoke-001.screens.json' or row.get('path', [])[:1] != ['terminal-cell-slot']:
            restored.append(row)
            continue
        path, kind = row['path'], row.get('kind')
        if path == ['terminal-cell-slot', 4] and kind == 'terminal-progress-spinner':
            if row.get('after') != base64.b64encode('⠋'.encode()).decode():
                raise AssertionError('markdown progress spinner replacement differs')
            spinner += 1
            continue
        if kind == 'footer-elapsed' and path in (['terminal-cell-slot', 5], ['terminal-cell-slot', 6]):
            footers.add(path[1])
            row = dict(row, path=['terminal-cell-slot', path[1] - 1])
        restored.append(row)
    if spinner != 1 or footers != {5, 6}:
        raise AssertionError('markdown progress requires exactly one slot and two footer shifts')
    return restored


def verify_markdown_progress_ledger(previous, actual, staged, previous_identity):
    """Reconstruct the old C42 lock from current replay without replaying its stale ledger."""
    from parity import golden, run
    case_id = 'C42.markdown-progress'
    old = golden.cache_path({'reference_commit': previous_identity['reference_commit']}) / case_id
    documents, ledger, captured = run.read_frozen_case(old)
    policies = json.loads(documents['artifact-policies.json'])
    if case_lock(captured['normalized'], policies, ledger) != previous:
        raise AssertionError('markdown progress original frozen lock differs')
    replay = json.loads(documents['replay.json'])
    replayed, replay_policies, replay_ledger = run.replay_case(captured['raw'], replay)
    run.account_raw(captured['raw'], replayed, replay_policies, replay)
    if replayed != captured['normalized'] or replay_policies != policies:
        raise AssertionError('markdown progress old raw replay changes normalized bytes or policies')
    if case_lock(replayed, replay_policies, inverse_markdown_progress_ledger(replay_ledger)) != previous:
        raise AssertionError('markdown progress old contract reconstruction differs')
    _, staged_ledger, staged_capture = run.read_frozen_case(staged)
    if staged_capture['normalized'] != captured['normalized']:
        raise AssertionError('markdown progress staged normalized bytes differ')
    if case_lock(staged_capture['normalized'], policies, staged_ledger) != actual:
        raise AssertionError('markdown progress staged lock differs')
    if case_lock(replayed, policies, replay_ledger) != actual:
        raise AssertionError('markdown progress staged contract differs from old raw replay')
    if case_lock(replayed, policies, inverse_markdown_progress_ledger(staged_ledger)) != previous:
        raise AssertionError('markdown progress staged contract reconstruction differs')
    return dict(old_raw_replay_verified=True, normalized_bytes_unchanged=True,
                original_contract_reconstructed=True,
                added_sites=[['invoke-001.screens.json', ['terminal-cell-slot', 4], 'terminal-progress-spinner', 1]],
                reindexed_footer_slots={'5': 4, '6': 5})


def rust_shell_timing_sites(case_id):
    """Enumerate the reviewed embedded shell timing sites, not arbitrary duration paths."""
    match = re.fullmatch(r'W39\.shell-(511|512|513)-plan-([01])', case_id)
    if not match and case_id != 'W47.projection':
        raise AssertionError('unreviewed Rust shell timing case: ' + case_id)
    index, trace = (2, 'trace-2.jsonl') if match else (4, 'trace-3.jsonl')
    handoff = ['handoff', index, 'shell', 'duration_ms']
    request = ['input', 0, 'content', 0, 'text', *handoff]
    history = ['history', 0, 'user', 'text', *handoff]
    sites = [('cache-input', history), ('http-3.body', request),
             (trace, ['data', *history]), (trace, ['data', 'initial_history', index, 'shell', 'duration_ms']),
             (trace, ['data', 'request_body', 'json', *request]), (trace + '.captured-request', request)]
    if match:
        evidence = ['state', 'json', 'recent_evidence', 1 - int(match[2]), 'shell', 'duration_ms']
        sites += [('generation-request-source', request), ('http-2.body', evidence), ('packet-source', evidence)]
    else:
        sites += [('handoff-source', [index, 'shell', 'duration_ms']), ('switch-request-source', request)]
    return [[artifact, path, 'duration-ms', 1] for artifact, path in sites]


def verify_rust_shell_timing(case_id, previous, actual, staged):
    """Replay both pins and permit only reviewed zero/no-op ledger sites, never output changes."""
    from parity import golden, run
    old = golden.cache_path({'reference_commit': REPIN_REFERENCE}) / case_id
    if previous['artifacts'] != actual['artifacts'] or previous['normalized_sha256'] != actual['normalized_sha256']:
        raise AssertionError('Rust shell timing normalized artifacts differ: ' + case_id)
    if frozen_lock(old) != previous:
        raise AssertionError('Rust shell timing old replay differs: ' + case_id)
    contracts, normalized = [], []
    for directory in (old, staged):
        _, ledger, captured = run.read_frozen_case(directory)
        normalized.append(captured['normalized'])
        contracts.append(ledger_contract(ledger, captured['normalized']))
    if normalized[0] != normalized[1]:
        raise AssertionError('Rust shell timing normalized bytes differ: ' + case_id)
    allowed = rust_shell_timing_sites(case_id)
    stripped = [dict(contract, sites=[row for row in contract['sites'] if row not in allowed])
                for contract in contracts]
    if stripped[0] != stripped[1]:
        raise AssertionError('Rust shell timing unreviewed ledger change: ' + case_id)
    return dict(old_full_replay_verified=True, normalized_bytes_unchanged=True,
                removed_sites=[row for row in contracts[0]['sites'] if row not in contracts[1]['sites']],
                added_sites=[row for row in contracts[1]['sites'] if row not in contracts[0]['sites']])


def verify_rust_pending_inventory(previous, current):
    """Keep the ten declared pending leaves and their metadata exactly frozen."""
    fields = ('id', 'description', 'comparison', 'slow', 'pending', 'platforms', 'exclusive')
    pending = []
    for inventory in (previous, current):
        rows = inventory.get('cases')
        if (inventory.get('metadata_fields') != list(fields) or not isinstance(rows, list)
                or any(not isinstance(row, dict) or not set(fields) <= row.keys() for row in rows)
                or len({row['id'] for row in rows}) != len(rows)
                or inventory.get('metadata_sha256') != digest([{key: row[key] for key in fields} for row in rows])):
            raise AssertionError('Rust re-pin inventory metadata digest differs')
        pending.append({row['id']: row for row in rows if row.get('pending') or row.get('status') == 'pending'})
    expected = {'T36.before-start'} | {'W54.' + lane + '.cancel-body' for lane in
        ('openai', 'anthropic', 'proxy', 'chatgpt', 'jev', 'auth-discovery', 'auth-refresh', 'auth-models', 'auth-revoke')}
    if set(pending[0]) != expected or pending[0] != pending[1]:
        raise AssertionError('Rust re-pin must preserve the ten pending leaves')


def pending_repin_metadata(staging, cases, pending, reference, new_cases):
    """Require real capture inventory and supplied non-runnable leaves, never implicit collection."""
    try:
        inventory = json.loads((Path(staging) / 'inventory.json').read_bytes())
    except (OSError, ValueError) as exc:
        raise AssertionError('re-pin pending inventory unavailable') from exc
    system = reference['reference_platform'].split('/')[0]
    if (inventory.get('platform') != system
            or any(inventory.get(key) != reference.get(key)
                   for key in ('reference_commit', 'reference_binary_sha256'))):
        raise AssertionError('re-pin pending inventory reference differs')
    fields = ('id', 'description', 'comparison', 'slow', 'pending', 'platforms', 'exclusive')
    rows = inventory.get('cases')
    if (inventory.get('metadata_fields') != list(fields) or not isinstance(rows, list)
            or any(not isinstance(row, dict) or not set(fields) <= row.keys() for row in rows)
            or inventory.get('metadata_sha256') != digest([{field: row[field] for field in fields} for row in rows])):
        raise AssertionError('re-pin pending inventory metadata digest differs')
    result = {}
    for case_id, reason in sorted(pending.items()):
        leaves = [case for case in cases if case.id == case_id]
        rows = [row for row in inventory.get('cases', []) if row.get('id') == case_id]
        if (case_id in new_cases or len(leaves) != 1 or len(rows) != 1
                or leaves[0].run is not None or leaves[0].pending != reason
                or system not in leaves[0].platforms):
            raise AssertionError('re-pin pending declaration differs: ' + case_id)
        expected = {field: list(leaves[0].platforms) if field == 'platforms' else getattr(leaves[0], field)
                    for field in fields}
        expected['status'] = 'pending'
        if rows[0] != expected:
            raise AssertionError('re-pin pending inventory metadata differs: ' + case_id)
        result[case_id] = rows[0]
    return result


def approved_zero_exec_timing_updates(old_cases, new_cases, staging):
    """Prove approved ledger-only migrations by reconstructing each exact prior lock."""
    from parity import run

    protected, updates = dict(old_cases), []
    for case_id, previous in sorted(old_cases.items()):
        actual = new_cases.get(case_id)
        if (not case_id.startswith(('W', 'T')) or actual is None or actual == previous
                or actual != dict(previous, ledger_sha256=actual['ledger_sha256'])):
            continue
        documents, ledger, captured = run.read_frozen_case(Path(staging) / case_id)
        policies = json.loads(documents['artifact-policies.json'])
        run.validate_replay(captured['raw'], json.loads(documents['replay.json']),
                            captured['normalized'], policies, ledger[:-1])
        if case_lock(captured['normalized'], policies, ledger) != actual:
            raise AssertionError('migration evidence differs from captured lock: ' + case_id)
        # Replay emits identity duration rows only for eligible exec timing sites.
        zero_rows = [row for row in ledger if row.get('kind') == 'duration-ms'
                     and row.get('path', [])[-1:] == ['duration_ms']
                     and row.get('before') == row.get('after') == 'MA==']
        legacy = [row for row in ledger if row not in zero_rows]
        if not zero_rows or case_lock(captured['normalized'], policies, legacy) != previous:
            continue
        protected[case_id] = actual
        updates.append(dict(id=case_id, old_ledger_sha256=previous['ledger_sha256'],
                            new_ledger_sha256=actual['ledger_sha256'],
                            sites=[dict(artifact=row['artifact'], path=row['path']) for row in zero_rows]))
    return protected, updates


SHELL_TIMING_SITES = (
    ['cache-input', ['history', 0, 'shell', 'duration_ms'], 'duration-ms', 1],
    ['trace-1.jsonl', ['data', 'history', 0, 'shell', 'duration_ms'], 'duration-ms', 1],
    ['trace-1.jsonl', ['data', 'initial_history', 0, 'shell', 'duration_ms'], 'duration-ms', 1],
)


def approved_shell_timing_updates(old_cases, new_cases, staging):
    """Allow only C17 ledger changes whose exact old lock reconstructs by site omission."""
    from parity import run

    protected, updates = dict(old_cases), []
    for case_id, previous in sorted(old_cases.items()):
        actual = new_cases.get(case_id)
        if (not case_id.startswith('C17.') or actual is None or actual == previous
                or actual != dict(previous, ledger_sha256=actual['ledger_sha256'])):
            continue
        documents, ledger, captured = run.read_frozen_case(Path(staging) / case_id)
        policies = json.loads(documents['artifact-policies.json'])
        run.validate_replay(captured['raw'], json.loads(documents['replay.json']),
                            captured['normalized'], policies, ledger[:-1])
        if case_lock(captured['normalized'], policies, ledger) != actual:
            raise AssertionError('shell timing migration evidence differs: ' + case_id)
        contract = ledger_contract(ledger, captured['normalized'])
        if any(contract['sites'].count(site) != 1 for site in SHELL_TIMING_SITES):
            raise AssertionError('shell timing migration lacks its three sites: ' + case_id)
        matches = []
        for mask in range(8):
            omitted = [site for index, site in enumerate(SHELL_TIMING_SITES) if mask & (1 << index)]
            inverse = dict(contract, sites=[site for site in contract['sites'] if site not in omitted])
            reconstructed = dict(actual, ledger_sha256=digest(inverse))
            if reconstructed == previous:
                matches.append((omitted, reconstructed))
        if len(matches) != 1:
            raise AssertionError('shell timing migration cannot reconstruct unique old lock: ' + case_id)
        omitted, reconstructed = matches[0]
        protected[case_id] = actual
        updates.append(dict(id=case_id, old_case_sha256=digest(previous), new_case_sha256=digest(actual),
                            old_ledger_sha256=previous['ledger_sha256'], new_ledger_sha256=actual['ledger_sha256'],
                            inverse_case_sha256=digest(reconstructed), added_sites=omitted,
                            normalized_sha256=actual['normalized_sha256']))
    return protected, updates


EXEC_LEDGER_CASES = {
    **dict.fromkeys(('T24.create', 'T24.parent-redirect', 'T24.parent-swap', 'T24.symlink'), (0, None, 4, 3)),
    'T29.literal-environment': (0, 'exec-literal-environment', 2, 1),
    **dict.fromkeys(('T30.timeout-1', 'T30.timeout-omitted', 'T31.nonzero', 'T31.signal', 'T31.success',
                     'T33.exact', 'T33.interleaved', 'T33.invalid', 'T33.large', 'T33.multibyte'), (0, None, 2, 1)),
    **{'T32.success-' + str(n): (n if n > 1 else 0, 'exec-fixture-cwd', 2, 1) for n in range(1, 9)},
    **{'T32.failure-' + str(n): (n, 'exec-fixture-cwd', 2, 1) for n in (1, 4, 8)},
    **dict.fromkeys(('T32.timeout', 'T34.redirected-escape', 'T34.redirected-group',
                     'T34.retained-escape', 'T34.retained-group'), (1, None, 2, 1)),
    'T35.redirected': (2, None, 2, 1),
}


def exec_ledger_sites(case_id):
    """Expand the exact relative echo and legacy history sites for the approved exec cases."""
    steps, output_kind, history, count = EXEC_LEDGER_CASES[case_id]
    fields = [('resolved_executable', 'exec-allocated-executable')]
    if output_kind:
        fields.extend([('stdout', output_kind), ('stdout_bytes_seen', 'exec-fixture-stdout-length')])
    relative = [(['data', 'steps', step, field] if steps else ['data', field], kind)
                for step in range(steps or 1) for field, kind in fields]
    added = [[artifact, path, kind, count] for artifact in ('cache-input', 'trace-1.jsonl') for path, kind in relative]
    removed = [['trace-1.jsonl', ['data', 'history', history, 'tool', 'outcome'] + path, kind, count]
               for path, kind in relative]
    return added, removed


def approved_exec_ledger_updates(old_cases, new_cases, staging):
    """Allow only 32 replay-proven exec ledger migrations with one exact inverse mask."""
    from parity import run

    protected, updates = dict(old_cases), []
    for case_id, previous in sorted(old_cases.items()):
        actual = new_cases.get(case_id)
        if (case_id not in EXEC_LEDGER_CASES or actual is None or actual == previous
                or actual != dict(previous, ledger_sha256=actual['ledger_sha256'])):
            continue
        documents, ledger, captured = run.read_frozen_case(Path(staging) / case_id)
        policies = json.loads(documents['artifact-policies.json'])
        run.validate_replay(captured['raw'], json.loads(documents['replay.json']),
                            captured['normalized'], policies, ledger[:-1])
        if case_lock(captured['normalized'], policies, ledger) != actual:
            raise AssertionError('exec ledger migration evidence differs: ' + case_id)
        contract = ledger_contract(ledger, captured['normalized'])
        added, removed = exec_ledger_sites(case_id)
        if (any(contract['sites'].count(site) != 1 for site in added)
                or any(site in contract['sites'] for site in removed)):
            raise AssertionError('exec ledger migration lacks exact sites: ' + case_id)
        # Only the complete, identified echo relocation is approved; never a partial mask.
        groups = [(False, [site for site in added if site[0] == 'cache-input']),
                  (False, [site for site in added if site[0] == 'trace-1.jsonl']), (True, removed)]
        matches = []
        for mask in range(1 << len(groups)):
            sites = list(contract['sites'])
            for index, (restore, rows) in enumerate(groups):
                if mask & (1 << index):
                    sites = sites + rows if restore else [site for site in sites if site not in rows]
            inverse = dict(contract, sites=sorted(sites, key=canonical))
            reconstructed = dict(actual, ledger_sha256=digest(inverse))
            if reconstructed == previous:
                matches.append((mask, reconstructed))
        if len(matches) != 1 or matches[0][0] != 7:
            raise AssertionError('exec ledger migration cannot reconstruct unique old lock with mask 7: ' + case_id)
        mask, reconstructed = matches[0]
        protected[case_id] = actual
        updates.append(dict(id=case_id, old_case_sha256=digest(previous), new_case_sha256=digest(actual),
                            old_ledger_sha256=previous['ledger_sha256'], new_ledger_sha256=actual['ledger_sha256'],
                            inverse_case_sha256=digest(reconstructed), normalized_sha256=actual['normalized_sha256'],
                            inverse_mask=mask, mask_count=1 << len(groups),
                            omitted_sites=[site for index, (restore, rows) in enumerate(groups)
                                           if not restore and mask & (1 << index) for site in rows],
                            restored_sites=[site for index, (restore, rows) in enumerate(groups)
                                            if restore and mask & (1 << index) for site in rows]))
    return protected, updates


def approved_race_fixture_updates(old_cases, new_cases, staging, pairs):
    """Approve only reviewed complete hash pairs whose replacement fully replays."""
    protected, updates = dict(old_cases), []
    for case_id, pair in sorted(pairs.items()):
        previous, actual = old_cases.get(case_id), new_cases.get(case_id)
        if (previous is None or actual is None or previous == actual
                or digest(previous) != pair['old_case_sha256']
                or digest(actual) != pair['new_case_sha256']):
            continue
        if frozen_lock(Path(staging) / case_id) != actual:
            raise AssertionError('race fixture migration evidence differs: ' + case_id)
        protected[case_id] = actual
        updates.append(dict(id=case_id, **pair))
    return protected, updates


def protect_wire_locks(old_cases, new_cases):
    """Stop area expansion before publishing any unapproved area-1 oracle change."""
    allowed = {'W28.window-cutoff-items'}
    changed = sorted(case_id for case_id, previous in old_cases.items()
                     if case_id.startswith('W') and case_id not in allowed
                     and new_cases.get(case_id) != previous)
    if changed:
        raise AssertionError('area-1 lock identity violation: ' + ', '.join(changed))


def protect_previous_locks(old_cases, new_cases):
    """Area expansion cannot refresh existing results outside proved migrations."""
    changed = sorted(case_id for case_id, previous in old_cases.items()
                     if new_cases.get(case_id) != previous)
    if changed:
        raise AssertionError('earlier-area lock identity violation: ' + ', '.join(changed))

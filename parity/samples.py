"""Small, complete normalized cases selected for review, never candidate observations."""

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import tarfile

from parity.identity import current_platform


SELECTIONS = {
    'C01.unknown': 'Unknown-command help, stderr/stdout channels and exit code; mutation 1 catcher.',
    'C02.version': 'Version prefix and narrowly certified build metadata.',
    'C03.post-prompt': 'Post-prompt flag rejection; mutation 2 catcher.',
    'C03.terminator': 'Explicit -- retains flag-looking prompt bytes.',
    'C04.file-crlf': 'One-LF removal preserves the CR byte in a file prompt.',
    'C05.agents-script': 'Invalid feature combination fails before external effects.',
    'C06.anthropic-key': 'Credential requirement on the independently selected provider.',
    'C07.grant-absolute': 'Launch grant and canonical root evidence.',
    'C08.agents': 'Launch-feature declarations without invoking an agent.',
    'C09.tool-recap': 'Reply-only stdout plus tool progress and recap on stderr.',
    'C10.duplicate-next-step': 'Accepted-call ledger prevents duplicate effects across model steps.',
    'C11.ambiguous': 'Piped input command-versus-submission behavior.',
    'C12.context': 'Local context breakdown with no provider request.',
    'C13.pin-same-model': 'Manual selection preserves pinning semantics on the following request.',
    'C14.default': 'Compaction request, committed summary and ordinary following turn.',
    'C15.uppercase-high': 'Case-sensitive review level validation.',
    'C16.implement-here': 'Real plan picker and next-turn mode transition.',
    'C16.refuse-edit_file-flag': 'Plan dispatch denial preserves independently read bytes; mutation 4 catcher.',
    'C17.success': 'Human shell history, actual effects and independently matched request string.',
    'C21.context-gc': 'Dim GC notice, restored terminal status and next turn.',
    'C22.review-command': 'Review completion, invalid syntax and actual tool loop.',
    'C26.exec-timeout-continues': 'Real exec timeout presentation and continued ordinary turn.',
    'C27.model-retry': 'Retry-wait screen, successful retry and billing attempt accounting.',
    'C28.auto-startup': 'Auto status before any router or model request.',
    'C29.fresh-prompt': 'Prompt region immediately follows actual PTY reply output.',
    'C30.wrap-and-edit': 'Exact wrapped input cells, cursor and Ctrl-U redraw.',
    'C31.paste-and-submit': 'Multiline bracketed paste submitted as one turn.',
    'C32.tool-turn': 'Tool progress, output, summary and restored prompt.',
    'C33.long-reply-during-turn': 'All long-reply lines reach the PTY scrollback exactly once.',
    'C34.typeahead-queue-long-reply': 'Queued input survives actual PTY reply pressure; mutation 6 catcher.',
    'C35.resize-during-turn': 'Barrier-controlled SIGWINCH, resized rules and output preservation.',
    'C36.edit-error-recovery': 'Editor failure recovers the real controlling terminal.',
    'C37.interrupt-shell-command': 'Foreground process barrier and the narrowly witnessed signal alternatives.',
    'C38.interrupt-running-command': 'Exec cancellation, uncertain effects and reset recovery.',
    'C39.resize-while-typing': 'Resizing preserves edited input and cursor.',
    'C40.narrow-terminal': 'Usable input without a full prompt region at 18 columns.',
    'C41.effort-cancel-ctrl-c': 'Picker cancellation restores exact terminal state.',
    'C42.markdown-color-48': 'Exact Markdown styled cells and wide/combining Unicode at narrow width.',
    'W01.catalog-gpt-6-sol-model': 'Catalog-backed model selection rather than an invalid-input-only sample.',
    'W02.chatgpt-explicit': 'Plan inference, stable session UUID and request-ID ownership.',
    'W03.live-key-userinfo': 'Proxy routing with credential-bearing input and redacted diagnostics.',
    'W04.claude-haiku-4-5-live-auto': 'Messages adapter and catalog-derived effort.',
    'W05.live-mapping-anthropic-ro1-plan1': 'Complete logical-to-provider tool declarations with both authority modes enabled.',
    'W06.at-limit': 'Instruction loading at the exact accepted size boundary.',
    'W07.gpt-6-luna-crlf': 'Lossless CRLF escaping, which is covered by a deliberate mutation.',
    'W08.America-Los_Angeles-ro0-plan1': 'Independent local snapshot date plus plan content ordering.',
    'W09.claude-haiku-4-5-ascii-at': 'Full 10 MiB boundary payload, not an abbreviated excerpt.',
    'W10.base-proxy': 'Independent cache-key reconstruction through the proxy adapter.',
    'W11.compaction': 'Cache prefix identity across history compaction.',
    'W12.batch-and-next': 'Final-eligible-block cache markers across a batched tool exchange.',
    'W14.native': 'Opaque reasoning/message/tool native continuation.',
    'W15.native': 'Sequential batched tool effects and distinct item/call IDs.',
    'W16.success': 'Anthropic native thinking, signatures and tool input.',
    'W17.arguments-object': 'Whole-envelope failure on nested typed tool arguments.',
    'W18.anthropic-top-array': 'Provider top-level type rejection.',
    'W19.anthropic-duplicate-id': 'Whole-envelope validation of duplicate native call IDs.',
    'W20.malformed': 'Malformed tool input is a tool observation, not a provider failure.',
    'W21.crlf': 'SSE CRLF framing and JSON event-type precedence.',
    'W22.final-nonempty': 'Indexed item ordering and final nonempty precedence.',
    'W23.bad-item-even-final': 'Strict malformed native-item rejection despite a final event.',
    'W24.coded': 'Coded stream failure detail, status and retained usage.',
    'W25.completed-before-null-final': 'The first completion wins over a later malformed terminal event.',
    'W26.anthropic-408': 'Retryable HTTP status behavior, also covered by a mutation.',
    'W27.connection-refused': 'Owned failed-dial error without fabricated response bytes.',
    'W28.no-retry-incomplete-event': 'Explicit response.incomplete does not retry; the mutation catcher.',
    'W29.anthropic-exhaust-429-2': 'Clipped final wait and retry exhaustion with exact request identity.',
    'W30.cancel-partial': 'Partial-response cancellation followed by a successful chat turn.',
    'W31.code': 'Owned overflow classification and exact error wording.',
    'W32.anthropic-absent': 'Unknown usage remains distinct from known zero.',
    'W33.anthropic-contracts-adjacent-invalid': 'All three invalid-UTF-8 contracts, including unchanged opaque bytes.',
    'W33.openai-contracts-adjacent-nonascii': 'OpenAI opaque native data beside valid non-ASCII; complements Anthropic.',
    'W34.incomplete-compact': 'Unanswered history recovery through explicit compaction.',
    'W35.default': 'Auto fallback launch defaults.',
    'W36.crlf': 'Local malformed-key rejection and recovery without sending the invalid key.',
    'W37.failed-model': 'Failed manual selection preserves Auto state atomically.',
    'W38.configured-claude-haiku-4-5-omitted-1': 'Configured route criteria and actual provider destination.',
    'W39.history': 'Router provenance, UTF-8 previews and native omissions; the mutation catcher.',
    'W40.escaped-boundary': 'Exact final double-encoded packet byte boundaries.',
    'W41.body-deadline': 'The router deadline includes a progressing response body.',
    'W42.confidence-0': 'Confidence is not route sufficiency.',
    'W43.threshold-below': 'Ladder threshold behavior, also covered by a deliberate mutation.',
    'W44.unknown-effect': 'Unknown exec effects stop routing and generation without replay.',
    'W45.known': 'Router usage remains separate from generation usage.',
    'W46.handoff': 'Switch history preservation and destination default effort.',
    'W47.projection': 'Numbered handoff projection, tool trust and injection-like text.',
    'W48.roundtrip': 'Roundtrip reprojection without recursively nested handoffs.',
    'W49.window': 'Transactional cross-provider admission and independent rejected-request size.',
    'W50.fresh': 'Fresh model selection atomicity.',
    'W51.effect-unknown': 'Transactional switch guarded by a real unknown exec effect.',
    'W52.anthropic-1-compact': 'Signed native tail and pinned system transition across compaction.',
    'W53.defaults.endpoints': 'Default endpoint diagnostics without public I/O.',
    'W53.issuer.models.password': 'Issuer catalog handling, credential redaction and allocated display-name URL normalization.',
    'W54.anthropic.cancel-body': 'Narrow fixture-input delivery projection, preserving every Go output.',
    'W54.anthropic.progress': 'Shared transport read progress rather than timeout-only behavior.',
    'T01.list_files.array': 'Owned common decoder error envelope and unchanged workspace.',
    'T02.list_files.offset.omitted': 'Omitted numeric argument defaults.',
    'T03.environment': 'Withheld-name boundary leaves .environment readable.',
    'T04.ancestor': 'Outside symlink ancestor write refuses publication and preserves an independent sentinel.',
    'T05.default': 'Ordered directory listing with withheld-entry accounting.',
    'T06.empty': 'Empty UTF-8 file and line boundaries.',
    'T07.nul': 'Binary read refusal at a byte boundary.',
    'T08.literal': 'Literal search and exact offsets.',
    'T09.empty': 'Owned search argument errors.',
    'T10.excluded': 'Search withheld/excluded entries.',
    'T11.exact-max': 'Search result byte bound.',
    'T12.empty': 'Create empty file and next read.',
    'T13.write_file.missing': 'Unread overwrite authorization refusal.',
    'T14.explicit': 'Stale explicit digest preserves external bytes.',
    'T15.missing': 'Explicit digest grammar.',
    'T16.missing': 'Exact edit_not_found wording.',
    'T17.atomic': 'Batch failure preserves original bytes.',
    'T18.append-no-lf': 'Append never inserts a newline.',
    'T19.create': 'Write creates parents and records digest.',
    'T20.unread': 'Deletion requires digest authority.',
    'T21.outside-link': 'Symlink deletion preserves outside sentinel.',
    'T22.partial-permission': 'Partial-delete ledger names only actual removed paths.',
    'T23.absolute': 'Workspace path grammar and next read.',
    'T24.create': 'Reserved missing destination rechecks creation.',
    'T25.same': 'Same-root workspace selection is a no-op.',
    'T26.mixed-switch_workspace': 'Workspace access tools run alone.',
    'T27.rebound-write': 'Next tool uses selected workspace binding.',
    'T28.valid': 'Project instructions discovery.',
    'T29.literal-environment': 'Literal argv and independently expected sanitized environment.',
    'T30.string-vector': 'Argument-vector owned error.',
    'T31.success': 'Successful exec status and byte observations.',
    'T32.failure-1': 'then failure preserves skipped-command effects.',
    'T33.large': 'Independent encoded output-budget calculation.',
    'T34.redirected-escape': 'TERM-resistant group escape and independent descendant liveness.',
    'T35.retained': 'Exited leader with inherited pipe cleanup.',
    'T36.started-sigint': 'Post-start cancellation and ordinary subsequent invocation.',
    'T38.bounds.distance-at': 'Exact Myers work boundary.',
    'T39.arguments.count101': 'Git inspection count cap.',
    'T40.promisor.remote': 'Promisor refusal with fully present objects.',
    'T41.arguments.empty-paths': 'Git recipe owned argument refusal.',
    'T42.status.normal': 'Read recipe commands and independently checked repository.',
    'T43.explain_branch.normal': 'Branch comparison recipe manifest.',
    'T44.pr_status.missing-pr': 'Recorded gh response and proof checks.',
    'T45.commit.explicit': 'Scoped real commit preserves unrelated changes.',
    'T46.split_commits.normal': 'Real one-file-per-commit effects.',
    'T47.amend.normal': 'Amend replaces only unpushed tip.',
    'T48.commit_push.normal': 'Local bare remote tip proves push.',
    'T49.ship_pr.closed': 'Closed PR starts a new branch through recorded gh.',
    'T50.follow_up_pr.noop': 'Scoped no-op commit behavior and follow-up proof.',
    'T51.latest-default.clean': 'New branch starts from latest local default.',
    'T52.conflict-lease.conflict': 'Conflicting rebase rolls back refs and worktree.',
    'T53.described-target.done': 'Loopback Jev chooses a real revert target.',
    'T54.first-failure.amend': 'Failed command status retains evidence.',
    'T55.plan-edit_file': 'Plan dispatch cannot change independently read bytes.',
    'T56.off': 'Recall registration and unavailable outcome.',
    'T57.cap-across-turns': 'Report limit persists across eleven turns.',
    'T58.live-declarations': 'Captured live declaration shapes.',
    'T59.maps-numbers': 'Exact map/struct/numeric wire encoding.',
    'T60.continuation-long-path': 'Escaping-heavy encoded bound and exact complete-line continuation.',
}


SELECTIONS.update({
    'A01.off': 'Lifecycle declarations and dispatch gates reject without child I/O.',
    'A02.accept-entries128': 'Inclusive selected-entry limit with original numbering and trust.',
    'A03.fresh-live': 'Fresh labelled context reads live files without parent history.',
    'A04.native-prebatch': 'Complete-batch native history boundary; mutation 30 catcher.',
    'A05.fixed-history': 'Held fork remains fixed after the parent advances.',
    'A06.claude-haiku-4-5': 'Cross-provider labelled history and opaque native omission.',
    'A07.selected-3-1': 'Supplied selected-entry order and original numbering.',
    'A08.destination-window': 'Admission precedes child publication and request.',
    'A09.later-plan-send': 'Later root plan mode refuses work for a nonplanning writer.',
    'A10.capacity-idle-dismiss': 'Idle named capacity and dismissal/name release.',
    'A11.concurrent-inflight': 'Independent held requests and immutable in-flight root input.',
    'A12.poll-0': 'Polling, later join, and once-only delivery receipts.',
    'A13.batch-delivery': 'All current-batch outcomes precede delivery.',
    'A14.anonymous-named': 'Named Session reuse versus anonymous collection.',
    'A15.root-finish': 'Held descendant prevents premature final completion; mutation 33.',
    'A16.cancel-running': 'Distinct lifecycle history, capacity, and retained-effect rules.',
    'A17.exec-inflight': 'Root failure joins an actually started child exec.',
    'A18.batch': 'Whole-batch call reservation and next-task counters.',
    'A19.cached-over-input': 'Cached input above inclusive input cannot produce negative charge.',
    'A20.competing': 'Independent sibling observations and stale edits; mutation 31.',
    'A21.ledger': 'Bounded escaped delivery keeps uncertainty and omitted counts; mutation 34.',
    'A22.timeout-continue': 'Timeout continuation differs from blocked cancellation/reset.',
    'A23.bounded-pages': 'Lifetime subtree pages preserve full lexical records and totals.',
    'A24.cross-chat-reuse': 'Named lifetime cost and root tree cost across user turns.',
    'A25.default': 'Complete review prompt transport, without a judgment-quality claim.',
    'A26.probe-cleanup': 'Independent write probe, verification read and safe cleanup.',
    'A27.manual-text': 'Transactional compaction and following write authorization.',
    'A28.inrun-at': 'Measured 40-percent trigger, summary accounting and continuation.',
    'A29.gc-on-redundant-recent-three': 'Original trace plus protected third-latest result; mutation 35.',
    'A30.gc-break-even-arithmetic': 'Independent GC cache-economics arithmetic.',
    'A31.request-view-original-recall': 'GC only changes request view; original recall grants no authority.',
    'A32.recall-resume-unchanged': 'Restored GC setting and recall registration; mutation 8.',
    'A33.actual-completed-ordinary': 'Actual terminal status and ordinary following action.',
    'A34.live-crash-child-helper': 'Live helper/child crash, repeated recovery and fresh name reuse.',
    'A35.schema-agents': 'Event/schema/scope and accepted/batch/delivery ordering.',
    'A36.trace-existing-restore': 'Exclusive trace failure is nonfatal and later trace recovers.',
    'A37.v2-restored-settings': 'Canonical v2 state, restored settings and following request.',
    'A37.fixed-v2-roundtrip': 'Fixed authored v2 canonical bytes before and after import.',
    'A37.cross-go-to-candidate': 'Protected Go checkpoint imported by the supplied executable.',
    'A37.cross-candidate-to-go': 'Candidate-written checkpoint imported by protected Go.',
    'A37.cross-candidate-self': 'Candidate self-import round trip.',
    'A37.v1-go-migration-import': 'Fixed root-independent Go-only v1 migration and v2 import.',
    'A38.switch-encoded-above': 'Independent encoded-byte guard at exactly limit plus one.',
    'A39.reverse-release': 'Settled lexical delivery despite reverse completion; mutation 32.',
    'A40.call-ledger-compact': 'Compaction retains accepted call IDs and prevents replay.',
    'C19.corrupt-checksum': 'Changed state with old checksum refuses before request; mutation 7.',
    'C20.persistence-barriers': 'Admitted helper sabotages pre-model checkpoint publication.',
    'W13.native-fork': 'Actual native Anthropic fork cache prefix.',
    'T37.same-root': 'Independent overlap notice for canonical shared workspace execs.',
})


def normalized_digest(artifacts):
    """Hash artifact names, logical lengths and normalized bytes, not raw evidence."""
    digest = hashlib.sha256()
    for name, raw in sorted(artifacts.items()):
        digest.update(name.encode() + b'\0' + str(len(raw)).encode() + b'\0' + hashlib.sha256(raw).digest())
    return digest.hexdigest()


def bundle(case_id, artifacts, policies):
    """Encode all normalized artifact bytes with deterministic, inspectable tar metadata."""
    metadata = (json.dumps({'id': case_id, 'artifact_policies': policies},
                           ensure_ascii=False, sort_keys=True, indent=2) + '\n').encode()
    for name in artifacts:
        path = Path(name)
        if path.is_absolute() or '..' in path.parts:
            raise ValueError('unsafe sample artifact name: ' + name)
    contents = {'case.json': metadata, **{'artifacts/' + name: raw for name, raw in artifacts.items()}}
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode='w', format=tarfile.PAX_FORMAT) as archive:
        for name, raw in sorted(contents.items()):
            path = Path(name)
            if path.is_absolute() or '..' in path.parts:
                raise ValueError('unsafe sample artifact name: ' + name)
            item = tarfile.TarInfo(name)
            item.mode, item.size = 0o644, len(raw)
            archive.addfile(item, io.BytesIO(raw))
    return gzip.compress(stream.getvalue(), mtime=0)


def sample_directory(suite):
    """Retain Darwin's root samples and isolate Linux bundles and index below linux/."""
    from parity.run import HarnessError
    name = current_platform().split("/")[0]
    root = Path(suite) / 'samples'
    destination = root / 'linux' if name == 'linux' else root
    if root.is_symlink() or destination.is_symlink():
        raise HarnessError('sample directory must not be a symlink')
    return destination


def sample_files(directory):
    """Exclude only the separately owned Linux subtree from Darwin's sample inventory."""
    return {path.name for path in directory.iterdir()
            if not (current_platform() == 'darwin/arm64' and path.name == 'linux' and path.is_dir() and not path.is_symlink())}


def generated_paths(suite):
    """Publish only files owned by the executing platform's sample lane."""
    directory = sample_directory(suite)
    if not directory.exists():
        return []
    return [str((directory / name).relative_to(suite)) for name in sorted(sample_files(directory))]


def refresh_samples(lane, suite):
    """Refresh only the selected full normalized cases from replay-verified reference data."""
    from parity.run import HarnessError, load_golden, read_frozen_case

    lane, suite = Path(lane), Path(suite)
    inventory = json.loads((lane / 'inventory.json').read_bytes())
    if inventory.get('platform') != current_platform().split('/')[0]:
        raise HarnessError('sample source platform differs from executing platform')
    cases = {case['id']: case for case in inventory['cases']}
    missing = sorted(set(SELECTIONS) - cases.keys())
    if missing:
        raise HarnessError('sample selection has missing cases: ' + ', '.join(missing))
    implemented = {case['id'][:3] for case in cases.values() if case['status'] == 'implemented'}
    selected = {case_id[:3] for case_id in SELECTIONS}
    if selected != implemented:
        raise HarnessError('sample families differ: ' + ', '.join(sorted(selected ^ implemented)))
    destination = sample_directory(suite)
    destination.mkdir(parents=True, exist_ok=True)
    index = {'schema_version': 1, 'format': 'Deterministic gzip tar: case.json and artifacts/<logical path>; every normalized artifact is complete.',
             'review': 'tar -tzf parity/samples/<case>.tar.gz; tar -xOzf parity/samples/<case>.tar.gz artifacts/<path>. Opaque/binary artifacts are not converted to text.',
             'pending_families': {family: '; '.join(sorted({case['pending'] for case in cases.values()
                                                          if case['id'].startswith(family + '.')}))
                                  for family in sorted({case_id[:3] for case_id in cases} - implemented)},
             'cases': []}
    if current_platform() == 'linux/amd64':
        index['platform'] = 'linux'
        index['review'] = index['review'].replace('parity/samples/', 'parity/samples/linux/')
    changed = []
    for case_id, reason in SELECTIONS.items():
        documents, _, _ = read_frozen_case(lane / case_id)
        policies = json.loads(documents['artifact-policies.json'])
        artifacts = load_golden(lane / case_id, policies)
        raw = bundle(case_id, artifacts, policies)
        filename = case_id + '.tar.gz'
        path = destination / filename
        if not path.exists() or path.read_bytes() != raw:
            if case_id.startswith('W') and path.exists():
                raise HarnessError('area-1 sample identity violation: ' + case_id)
            if case_id.startswith('T') and path.exists():
                raise HarnessError('earlier-area sample identity violation: ' + case_id)
            changed.append(case_id)
        path.write_bytes(raw)
        index['cases'].append({'id': case_id, 'description': cases[case_id]['description'], 'why': reason,
                               'path': filename, 'artifacts': len(artifacts), 'normalized_bytes': sum(map(len, artifacts.values())),
                               'normalized_sha256': normalized_digest(artifacts), 'bundle_sha256': hashlib.sha256(raw).hexdigest()})
    expected = {'index.json', *(case_id + '.tar.gz' for case_id in SELECTIONS)}
    extra = sample_files(destination) - expected
    if extra:
        raise HarnessError('extra sample files: ' + ', '.join(sorted(extra)))
    (destination / 'index.json').write_text(json.dumps(index, ensure_ascii=False, indent=2) + '\n')
    return changed


def verify_samples(lane, suite):
    """Require complete selected sample bundles to equal regenerated normalized reference bytes."""
    from parity.run import HarnessError, load_golden, read_metadata

    lane, suite = Path(lane), Path(suite)
    directory = sample_directory(suite)
    index = json.loads((directory / 'index.json').read_bytes())
    if index.get('platform', 'darwin') != current_platform().split('/')[0]:
        raise HarnessError('selected sample index platform differs from executing platform')
    rows = {row['id']: row for row in index['cases']}
    if len(rows) != len(index['cases']) or rows.keys() != SELECTIONS.keys():
        raise HarnessError('sample case inventory differs')
    if sample_files(directory) != {'index.json', *(case_id + '.tar.gz' for case_id in SELECTIONS)}:
        raise HarnessError('sample file inventory differs')
    mismatched = []
    for case_id, row in rows.items():
        documents = read_metadata(lane / case_id)
        policies = json.loads(documents['artifact-policies.json'])
        artifacts = load_golden(lane / case_id, policies)
        path = directory / (case_id + '.tar.gz')
        expected = bundle(case_id, artifacts, policies)
        if (path.is_symlink() or path.read_bytes() != expected
                or row['path'] != path.name or row['why'] != SELECTIONS[case_id]
                or row['artifacts'] != len(artifacts)
                or row['normalized_bytes'] != sum(map(len, artifacts.values()))
                or row['normalized_sha256'] != normalized_digest(artifacts)
                or row['bundle_sha256'] != hashlib.sha256(expected).hexdigest()):
            mismatched.append(case_id)
    if mismatched:
        raise HarnessError('normalized samples differ: ' + ', '.join(mismatched))


def refresh_inventory(lane, suite):
    """Copy the complete reference case inventory without changing metadata encoding."""
    from parity.run import HarnessError, read_lane_inventory

    if current_platform() != 'darwin/arm64':
        raise HarnessError('standalone shared inventory migration is Darwin-only; use parity-golden for Linux')
    raw, _, _ = read_lane_inventory(lane)
    (Path(suite) / 'inventory.json').write_bytes(raw)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--lane', type=Path, required=True)
    parser.add_argument('--suite', type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument('--inventory', action='store_true', help='also migrate the complete lane inventory')
    args = parser.parse_args()
    if args.inventory:
        refresh_inventory(args.lane, args.suite)
    print(json.dumps({'changed_sample_cases': refresh_samples(args.lane, args.suite)}, indent=2))


if __name__ == '__main__':
    main()

"""Linux clipped Git diagnostics require independent call, command and allocation witnesses."""
import base64
import copy
import json
import unittest
from unittest.mock import patch

from parity import locks
from parity.observe import Normalizer, go_json
from parity.observe_tools import Tools


CASE = 'T52.conflict-lease.lease'
ROOT = '/tmp/reagent-parity-cases-bpvpy8jm/' + CASE
KIND = 'git-do-failed-display-remote-prefix'
ARGV = ['git', 'push', '-q', '--force-with-lease', 'origin', 'feature/parity']
HEADER = '  ✗ git_do recipe=rebase_push → '


def fixture(root=ROOT):
    output = ('To ' + root + '/origin.git\n ! [rejected] feature/parity -> feature/parity (stale info)\n'
              "error: failed to push some refs to '" + root + "/origin.git'\n")
    message = ' '.join(ARGV) + ' exited 1: ' + output.strip()
    outcome = dict(ok=False, code='step_failed', message=message, effect='applied',
                   data=dict(status='failed', reason=message, decision={},
                             steps=[dict(argv=ARGV[:], exit_code=1, output=output)]))
    call = dict(call_id='call_git', name='git_do', arguments='{"recipe":"rebase_push"}')
    events = [dict(type='model.accepted', data=dict(blocks=[dict(kind='tool_call', call=copy.deepcopy(call))])),
              dict(type='tool.started', data=dict(call, workspace=root + '/workspace')),
              dict(type='tool.finished', data=dict(call_id=call['call_id'], name='git_do', outcome=outcome))]
    for seq, event in enumerate(events, 1):
        event.update(schema_version=1, seq=seq, session_id='b' * 16, run_id='a' * 16,
                     step=1, time='2026-10-20T12:00:0%dZ' % seq)
    line = HEADER + ('step_failed: ' + message)[:119] + '…\n'
    invocation = dict(started=1792497599, ended=1792497605, stdout=line.encode(),
                      stderr=b'literal nonpath prose\n' + line.encode() + b'untouched suffix\n', returncode=0)
    observations = {'git/independent-commands.json': go_json([
        dict(argv=['git', 'init', '-q', '--bare', '-b', 'main', root + '/origin.git'], exit_code=0, stdout='', stderr=''),
        dict(argv=['git', 'remote', 'add', 'origin', root + '/origin.git'], exit_code=0, stdout='', stderr='')]),
        'git/git-commands.jsonl': go_json(dict(argv=ARGV[:], exit_code=1, stdout='', stderr=output)) + b'\n'}
    return events, [invocation], observations


def project(items, platform='linux', case=CASE, root=ROOT):
    events, invocations, observations = items
    normal = Normalizer(root, '')
    with patch('sys.platform', platform):
        Tools(normal, [('trace', go_json(event)) for event in events], [], invocations, case, observations)
        output = normal.text(invocations[0]['stderr'].decode(), 'invocation-1.stderr', ())
    return output.encode(), normal.ledger


class GitFailedDisplayTest(unittest.TestCase):
    def test_only_visible_prefix_changes_without_reclip_or_hidden_prose(self):
        reference = None
        for allocation in ('bpvpy8jm', 'mwgji8f9'):
            root = ROOT.replace('bpvpy8jm', allocation)
            items = fixture(root)
            actual, ledger = project(items, root=root)
            expected = b'literal nonpath prose\n' + (HEADER + 'step_failed: ' + ' '.join(ARGV)
                       + ' exited 1: To <ROOT>…\n').encode() + b'untouched suffix\n'
            self.assertEqual(actual, expected)
            display = [row for row in ledger if row['kind'] == KIND]
            self.assertEqual(len(display), 1)
            self.assertEqual(display[0]['path'], ['git-do-failed-display'])
            self.assertEqual(base64.b64decode(display[0]['before']), items[1][0]['stderr'].splitlines(keepends=True)[1])
            self.assertNotIn(b'origin.git', base64.b64decode(display[0]['after']))
            if reference is None:
                reference = actual
            self.assertEqual(actual, reference)

    def test_recorder_and_allocation_negatives(self):
        for control in ('missing', 'duplicate', 'argv', 'exit', 'boolean-exit', 'stdout', 'output', 'shape',
                        'allocation-missing', 'allocation-duplicate', 'allocation-exit', 'allocation-path'):
            with self.subTest(control=control):
                items = fixture()
                observations = items[2]
                record = json.loads(observations['git/git-commands.jsonl'])
                allocations = json.loads(observations['git/independent-commands.json'])
                if control == 'missing':
                    observations['git/git-commands.jsonl'] = b''
                elif control == 'duplicate':
                    observations['git/git-commands.jsonl'] *= 2
                elif control.startswith('allocation-'):
                    if control == 'allocation-missing':
                        allocations.pop()
                    if control == 'allocation-duplicate':
                        allocations.append(copy.deepcopy(allocations[0]))
                    if control == 'allocation-exit':
                        allocations[0]['exit_code'] = 1
                    if control == 'allocation-path':
                        allocations[0]['argv'][-1] += '-forged'
                    observations['git/independent-commands.json'] = go_json(allocations)
                else:
                    if control == 'argv':
                        record['argv'][-1] = 'other'
                    if control in ('exit', 'boolean-exit'):
                        record['exit_code'] = True if control == 'boolean-exit' else 2
                    if control == 'stdout':
                        record['stdout'], record['stderr'] = record['stderr'], ''
                    if control == 'output':
                        record['stderr'] += 'forged'
                    if control == 'shape':
                        record['forged'] = True
                    observations['git/git-commands.jsonl'] = go_json(record) + b'\n'
                with self.assertRaises(AssertionError):
                    project(items)

    def test_call_outcome_and_display_negatives(self):
        for control in ('accepted-missing', 'accepted-duplicate', 'accepted-call', 'start-duplicate',
                        'finish-duplicate', 'sequence', 'session', 'run', 'step', 'time', 'invocation-overlap', 'workspace', 'arguments',
                        'argv', 'exit', 'output', 'message', 'reason', 'status', 'ok', 'terminal',
                        'prefix', 'ellipsis', 'width', 'duplicate-line', 'prose-prefix'):
            with self.subTest(control=control):
                items = fixture()
                events, invocations, _ = items
                accept, start, finish = events
                outcome = finish['data']['outcome']
                if control == 'accepted-missing':
                    events.remove(accept)
                if control == 'accepted-duplicate':
                    events.append(copy.deepcopy(accept))
                if control == 'accepted-call':
                    accept['data']['blocks'][0]['call']['call_id'] = 'forged'
                if control == 'start-duplicate':
                    events.append(copy.deepcopy(start))
                if control == 'finish-duplicate':
                    events.append(copy.deepcopy(finish))
                if control == 'sequence':
                    accept['seq'] = 4
                if control == 'session':
                    accept['session_id'] = 'c' * 16
                if control == 'run':
                    accept['run_id'] = 'c' * 16
                if control == 'step':
                    accept['step'] = 2
                if control == 'time':
                    accept['time'] = '2026-10-20T11:00:00Z'
                if control == 'invocation-overlap':
                    invocations.append(copy.deepcopy(invocations[0]))
                if control == 'workspace':
                    start['data']['workspace'] += '-forged'
                if control == 'arguments':
                    start['data']['arguments'] = '{"recipe":"commit_push"}'
                if control == 'argv':
                    outcome['data']['steps'][0]['argv'][-1] = 'other'
                if control == 'exit':
                    outcome['data']['steps'][0]['exit_code'] = 2
                if control == 'output':
                    outcome['data']['steps'][0]['output'] += 'forged'
                if control == 'message':
                    outcome['message'] += 'forged'
                if control == 'reason':
                    outcome['data']['reason'] += 'forged'
                if control == 'status':
                    outcome['data']['status'] = 'done'
                if control == 'ok':
                    outcome['ok'] = True
                if control == 'terminal':
                    invocations[0]['terminal'] = {}
                raw = invocations[0]['stderr']
                if control == 'prefix':
                    raw = raw.replace(b'  \xe2\x9c\x97 git_do', b'  ! git_do')
                if control == 'ellipsis':
                    raw = raw.replace('…'.encode(), b'...')
                if control == 'width':
                    raw = raw.replace('…'.encode(), 'x…'.encode())
                if control == 'duplicate-line':
                    raw += raw.splitlines(keepends=True)[1]
                if control == 'prose-prefix':
                    raw = raw.replace(HEADER.encode(), b'forged ' + HEADER.encode())
                invocations[0]['stderr'] = raw
                with self.assertRaises(AssertionError):
                    project(items)

    def test_case_platform_model_and_unowned_invocation_remain_literal(self):
        for platform, case in (('darwin', CASE), ('linux', 'T52.conflict-lease.conflict')):
            items = fixture()
            actual, ledger = project(items, platform, case)
            self.assertEqual(actual, items[1][0]['stderr'])
            self.assertFalse(any(row['kind'] == KIND for row in ledger))
        items = fixture()
        items[0][:] = [dict(items[0][0], data=dict(blocks=[dict(kind='text', text=items[1][0]['stdout'].decode())]))]
        actual, ledger = project(items)
        self.assertEqual(actual, items[1][0]['stderr'])
        self.assertEqual(ledger, [])
        items = fixture()
        items[1].insert(0, dict(items[1][0], started=1792497500, ended=1792497501))
        actual, ledger = project(items)
        self.assertEqual(actual, items[1][0]['stderr'])
        self.assertFalse(any(row['kind'] == KIND for row in ledger))
        items = fixture()
        normal = Normalizer(ROOT, '')
        with patch('sys.platform', 'linux'):
            Tools(normal, [('trace', go_json(e)) for e in items[0]], [], items[1], CASE, items[2])
            self.assertEqual(normal.text(items[1][0]['stdout'].decode(), 'invocation-1.stdout', ()),
                             items[1][0]['stdout'].decode())
            self.assertEqual(normal.text(items[1][0]['stderr'].decode(), 'invocation-1.stderr', ('model',)),
                             items[1][0]['stderr'].decode())
        self.assertEqual(normal.ledger, [])

    def test_full_observer_replay_locks_bytes_and_keeps_prior_lock_protection(self):
        from parity import run
        items = fixture()
        events, invocations, observations = items
        trace = b''.join(go_json(event) + b'\n' for event in events)
        raw = dict(observations, **{'invoke-001.trace.jsonl': trace,
                   'invoke-001.stderr': invocations[0]['stderr'], 'invoke-001.stdout': invocations[0]['stdout'],
                   'invoke-001.status': b'0\n'})
        replay = dict(root=ROOT, artifacts='/allocated/artifacts', traces=['invoke-001.trace.jsonl'],
                      requests=0, server=None,
                      invocations=[{key: invocations[0][key] for key in ('started', 'ended')}])
        with patch('sys.platform', 'linux'):
            artifacts, policies, ledger = run.replay_case(raw, replay)
            replay['raw_policies'] = run.account_raw(raw, artifacts, policies, replay)
            run.validate_replay(raw, replay, artifacts, policies, ledger)
            current = locks.case_lock(artifacts, policies, ledger)
            display = next(row for row in ledger if row.get('kind') == KIND)
            old_artifacts = dict(artifacts)
            old_artifacts[display['artifact']] = old_artifacts[display['artifact']].replace(
                base64.b64decode(display['after']), base64.b64decode(display['before']))
            old_ledger = []
            for row in ledger:
                if row is display:
                    continue
                if row.get('artifact') == display['artifact'] and 'raw_normalized_sha256' in row:
                    import hashlib
                    row = dict(row, raw_normalized_sha256=hashlib.sha256(old_artifacts[row['artifact']]).hexdigest(),
                               normalized_length=len(old_artifacts[row['artifact']]))
                old_ledger.append(row)
            previous = locks.case_lock(old_artifacts, policies, old_ledger)
            for platform in ('linux', 'darwin'):
                with patch('sys.platform', platform), self.assertRaisesRegex(AssertionError, 'earlier-area'):
                    locks.protect_previous_locks({CASE: previous}, {CASE: current})
            changed_raw = dict(raw)
            changed_raw['invoke-001.stderr'] += b'forged'
            with self.assertRaises(run.HarnessError):
                run.validate_replay(changed_raw, replay, artifacts, policies, ledger)


if __name__ == '__main__':
    unittest.main()

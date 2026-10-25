"""Checkpoint and picker witnesses for narrowly scoped fresh Session IDs."""

import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

from parity.observe import Normalizer, finish, sha256
from parity.observe_cli_sessions import LOCAL_RESETS
from parity.cases.cli_pty_commands import SEED, PLAN_REQUEST, NEXT
from parity.observe_cli_terminal import encoded, integrate
from parity.vt import Terminal


OLD = '1111111111111111'
NEW = '2222222222222222'


def footer(identity):
    return 'session ID: %s · resume: reagent chat --resume %s\n' % (identity, identity)


def checkpoint(identity, workspace, **changes):
    state = dict(version=2, id=identity, launch=str(workspace), active=str(workspace),
                 model='gpt-6-sol', provider='openai', effort='low', read_only=False,
                 plan=False, phase='idle', history=None, blocked='', last_trace='',
                 literal='unchanged 0123456789abcdef', prompt=dict(text=(
                     'Pinned literal Workspace: /literal/root\n# Runtime\n\nProvider: openai\n'
                     'Model: gpt-6-sol\nReasoning effort: low\nPlatform: darwin\nWorkspace: '
                     + str(workspace) + '\nMode: read, write, and execute\n'
                     'Budget: unlimited model requests and unlimited tool calls per run\n'
                     'Unrelated literal /literal/root\n')))
    state.update(changes)
    raw = json.dumps(state, separators=(',', ':')).encode()
    return (b'{"version":2,"id":' + json.dumps(identity).encode() + b',"state":' + raw
            + b',"sha256":' + json.dumps(sha256(raw)).encode() + b'}')


class SessionSlotsTest(unittest.TestCase):
    def local(self, root, case='C12.reset'):
        root = root / case
        artifacts = root / 'artifacts'
        artifacts.mkdir(parents=True)
        workspace = root / 'workspace'
        stderr = ('re:agent chat · gpt-6-sol\nsession ID: ' + OLD + '\nworkspace '
                  + str(workspace) + ' · read, write · 32 steps, 128 tool calls per turn\n'
                  + footer(NEW) + 'fresh session; the conversation so far is no longer sent\n').encode()
        stdin = LOCAL_RESETS[case]
        invocation = dict(started=0, ended=100, stdout=b'', stderr=stderr, returncode=0, stdin=stdin)
        observations = {'invoke-001.stdin': stdin}
        for index, identity in enumerate((OLD, NEW), 1):
            observations['invoke-001.session-%d.checkpoint.json' % index] = checkpoint(identity, workspace)
        return SimpleNamespace(root=root, artifacts=artifacts, trace_paths=[],
                               invocations=[invocation], observations=observations)

    def test_finish_keeps_separate_sessions_and_lossless_checkpoint_witnesses(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            outputs = []
            for case in LOCAL_RESETS:
                ctx = self.local(root, case)
                result = finish(ctx)
                display = result['invocation-1.stderr']
                self.assertIn(b'session ID: SESSION_1\n', display)
                self.assertEqual(display.count(b'session ID: SESSION_2'), 1)
                for index in (1, 2):
                    name = 'invoke-001.session-%d.checkpoint.json' % index
                    cp = json.loads(result[name])
                    self.assertEqual(cp['id'], 'SESSION_%d' % index)
                    self.assertEqual(cp['state']['id'], cp['id'])
                    self.assertEqual(cp['state']['active'], '<ROOT>/workspace')
                    self.assertIn(b'"literal":"unchanged 0123456789abcdef"', result[name])
                    state_raw = result[name].split(b'"state":', 1)[1].rsplit(b',"sha256":', 1)[0]
                    self.assertEqual(cp['sha256'], sha256(state_raw))
                    self.assertEqual(ctx.artifact_policies[name], 'N')
                outputs.append(result)
            self.assertNotEqual(outputs[0]['invoke-001.stdin'], outputs[1]['invoke-001.stdin'])

    def test_reset_checkpoints_normalize_only_owned_pinned_runtime_workspace(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            results = [finish(self.local(root / name)) for name in ('first', 'other')]
            self.assertEqual(results[0], results[1])
            for result in results:
                state = json.loads(result['invoke-001.session-1.checkpoint.json'])['state']
                self.assertIn('Workspace: <ROOT>/workspace\nMode:', state['prompt']['text'])
                self.assertIn('Pinned literal Workspace: /literal/root', state['prompt']['text'])
            for mutation in ('workspace', 'duplicate', 'provider', 'missing'):
                with self.subTest(mutation=mutation):
                    ctx = self.local(root / mutation)
                    name = 'invoke-001.session-2.checkpoint.json'
                    state = json.loads(ctx.observations[name])['state']
                    text = state['prompt']['text']
                    if mutation == 'workspace':
                        text = text.replace('Workspace: ' + str(ctx.root / 'workspace'), 'Workspace: /forged')
                    elif mutation == 'duplicate':
                        text += '\n' + text[text.index('# Runtime'):]
                    elif mutation == 'provider':
                        text = text.replace('Provider: openai', 'Provider: anthropic')
                    else:
                        text = text.replace('# Runtime', '# Other')
                    ctx.observations[name] = checkpoint(NEW, ctx.root / 'workspace', prompt=dict(text=text))
                    with self.assertRaisesRegex(AssertionError, 'checkpoint pinned workspace'):
                        finish(ctx)

    def test_local_reset_raw_replay_accounts_for_complete_checkpoint_witnesses(self):
        from parity import run
        with tempfile.TemporaryDirectory() as temporary:
            ctx = self.local(Path(temporary))
            normalized = finish(ctx)
            policies = dict(ctx.artifact_policies)
            ledger = json.loads((ctx.artifacts / 'substitutions.json').read_text())
            raw = dict(ctx.observations)
            raw.update({'invoke-001.stdout': b'', 'invoke-001.stderr': ctx.invocations[0]['stderr'],
                        'invoke-001.status': b'0\n'})
            replay = dict(root=str(ctx.root), artifacts=str(ctx.artifacts), server='', requests=0, traces=[],
                          invocations=[dict(started=100, ended=90, monotonic_started=1, monotonic_ended=2)])
            replay['raw_policies'] = run.account_raw(raw, normalized, policies, replay)
            run.validate_replay(raw, replay, normalized, policies, ledger)
            self.assertEqual(replay['raw_policies']['invoke-001.stdin'], 'B')
            self.assertEqual(replay['raw_policies']['invoke-001.session-2.checkpoint.json'], 'N')
            self.assertFalse(any(name.startswith('retained/') for name in normalized))
            for name in ctx.observations:
                changed = dict(raw)
                del changed[name]
                with self.subTest(missing=name), self.assertRaises((AssertionError, run.HarnessError, KeyError)):
                    run.validate_replay(changed, replay, normalized, policies, ledger)

    def test_local_reset_rejects_missing_forged_and_mismatched_owners(self):
        changes = ('missing', 'digest', 'id', 'launch', 'mode', 'startup', 'stdin', 'footer-count', 'framing')
        with tempfile.TemporaryDirectory() as temporary:
            for index, change in enumerate(changes):
                with self.subTest(change=change):
                    ctx = self.local(Path(temporary) / str(index))
                    name = 'invoke-001.session-2.checkpoint.json'
                    inv = ctx.invocations[0]
                    if change == 'missing':
                        del ctx.observations[name]
                    elif change == 'digest':
                        ctx.observations[name] = ctx.observations[name].replace(b'unchanged', b'FORGED')
                    elif change in ('id', 'launch', 'mode'):
                        kwargs = {'launch': '/other'} if change == 'launch' else {'plan': True} if change == 'mode' else {}
                        ctx.observations[name] = checkpoint(OLD if change == 'id' else NEW, ctx.root / 'workspace', **kwargs)
                    elif change == 'stdin':
                        ctx.observations['invoke-001.stdin'] = b'/exit\n'
                    elif change == 'startup':
                        inv['stderr'] = inv['stderr'].replace(OLD.encode(), NEW.encode())
                    elif change == 'footer-count':
                        inv['stderr'] += footer(NEW).encode()
                    else:
                        inv['stderr'] = inv['stderr'].replace(b'fresh session;', b'other session;')
                    with self.assertRaises(AssertionError):
                        finish(ctx)

    def terminal(self, root, fresh=NEW, case='C16.implement-fresh'):
        root = root / case
        artifacts = root / 'artifacts'
        artifacts.mkdir(parents=True)
        workspace = root / 'workspace'
        opening = ('re:agent chat · gpt-6-sol\nsession ID: ' + OLD + '\nworkspace '
                   + str(workspace) + ' · read, write · 32 steps, 128 tool calls per turn\n'
                   + '/help for commands · Ctrl-D to exit\n❯ 2  Implement fresh\n').encode()
        raw = opening + (footer(fresh) + 'fresh session; implementing plan\nnext turn\n').encode()
        terminal = Terminal(30, 180)
        terminal.feed(opening)
        metadata = dict(version=1, initial_size=[30, 180], events=[
            dict(kind='input', offset=0, hex=(SEED.encode() + b'\r').hex()),
            dict(kind='input', offset=0, hex=(PLAN_REQUEST.encode() + b'\r').hex()),
            dict(kind='input', offset=len(opening), hex=b'\x1b[A'.hex()),
            dict(kind='snapshot', offset=len(opening), label='plan-picker-selected'),
            dict(kind='input', offset=len(opening), hex=b'\r'.hex()),
            dict(kind='input', offset=len(raw), hex=(NEXT.encode() + b'\r').hex()),
            dict(kind='input', offset=len(raw), hex=b'/exit\r'.hex())],
            snapshots=[dict(label='plan-picker-selected', offset=len(opening), state=terminal.snapshot())])
        invocation = dict(args=['chat', '--workspace', str(workspace)], stdout=b'', stderr=raw,
                          terminal=dict(rows=30, cols=180, stdout_tty=True), stdin=b'\x1b[A\r',
                          trace=artifacts / 'invoke-001.traces', started=0, ended=100)
        invocation['stdin'] = b''.join(bytes.fromhex(event['hex']) for event in metadata['events'] if event['kind'] == 'input')
        ctx = SimpleNamespace(root=root, artifacts=artifacts, observations={
            'invoke-001.tty': raw, 'invoke-001.terminal.json': encoded(metadata)}, invocations=[invocation])
        traces = []
        normal = Normalizer(root, '', artifacts=artifacts)
        for index, identity in enumerate((OLD, OLD, fresh, fresh)):
            event = dict(type='run.started', session_id=identity, data=dict(workspace=str(workspace)))
            traces.append((artifacts / 'invoke-001.traces' / str(index) / 'events.jsonl', json.dumps(event).encode()))
            normal.identity(identity, 'session')
        return ctx, normal, traces, metadata

    def test_fresh_terminal_projects_one_announcement_without_changing_cell_geometry(self):
        with tempfile.TemporaryDirectory() as temporary:
            ctx, normal, traces, _ = self.terminal(Path(temporary))
            artifacts, _, _ = integrate(ctx, normal, traces)
            screen = json.loads(artifacts['invoke-001.screens.json'])
            grid = screen['final']['grid']
            self.assertEqual([len(row) for row in grid], [180] * 30)
            text = '\n'.join(''.join(cell['text'] for cell in row) for row in grid)
            self.assertEqual(text.count('session ID: SESSION_2       '), 1)
            self.assertNotIn(NEW, text)
            self.assertIn('SESSION_1', text)
            self.assertEqual(sum(row.get('kind') == 'terminal-startup-session' for row in normal.ledger), 3)

    def test_fresh_terminal_rejects_unowned_trace_input_and_footer(self):
        with tempfile.TemporaryDirectory() as temporary:
            for index, change in enumerate(('trace', 'workspace', 'enter', 'arrow', 'selection', 'input', 'footer', 'missing-announcement', 'duplicate-announcement')):
                with self.subTest(change=change):
                    ctx, normal, traces, metadata = self.terminal(Path(temporary) / str(index))
                    if change in ('trace', 'workspace'):
                        event = json.loads(traces[2][1])
                        if change == 'trace':
                            event['session_id'] = OLD
                        else:
                            event['data']['workspace'] = '/elsewhere'
                        traces[2] = traces[2][0], json.dumps(event).encode()
                    elif change == 'input':
                        ctx.invocations[0]['stdin'] = b'changed'
                    elif change in ('footer', 'missing-announcement', 'duplicate-announcement'):
                        raw = ctx.invocations[0]['stderr']
                        if change == 'footer':
                            raw = raw.replace(NEW.encode(), b'3333333333333333')
                        elif change == 'missing-announcement':
                            raw = raw.replace(footer(NEW).encode(), b'')
                        else:
                            raw += footer(NEW).encode()
                        ctx.invocations[0]['stderr'] = raw
                        ctx.observations['invoke-001.tty'] = raw
                    else:
                        if change == 'enter':
                            metadata['events'][4]['hex'] = b'\n'.hex()
                        elif change == 'arrow':
                            metadata['events'][2]['hex'] = b'\x1b[B'.hex()
                        else:
                            metadata['snapshots'][0]['state']['grid'][5][2]['text'] = '3'
                        ctx.observations['invoke-001.terminal.json'] = encoded(metadata)
                        ctx.invocations[0]['stdin'] = b''.join(bytes.fromhex(event['hex']) for event in metadata['events'] if event['kind'] == 'input')
                    with self.assertRaises(AssertionError):
                        integrate(ctx, normal, traces)

    def test_other_families_do_not_gain_midstream_footer_slots(self):
        with tempfile.TemporaryDirectory() as temporary:
            ctx, normal, traces, _ = self.terminal(Path(temporary), case='C16.implement-here')
            artifacts, _, _ = integrate(ctx, normal, traces)
            grid = json.loads(artifacts['invoke-001.screens.json'])['final']['grid']
            text = '\n'.join(''.join(cell['text'] for cell in row) for row in grid)
            self.assertIn(NEW, text)
            # The unregistered midstream announcement stays literal.
            self.assertEqual(sum(row.get('kind') == 'terminal-startup-session' for row in normal.ledger), 1)


if __name__ == '__main__':
    unittest.main()

"""Consent result slots certified by independent terminal bytes and launch config."""

import datetime as dt
import json
import os
from pathlib import Path

from parity.cases.cli_dependencies import _screen
from parity.observe import child, go_json, lex, replace_spans, string_spans
from parity.observe_tools import Tools
from parity.observe_cli_terminal import replay


def human_workspace_outcomes(normal, traces, invocations, observations, case_id):
    """Return exact Tools.path_outcomes owners for the two human approval leaves."""
    identities = {'T23.human-consent': ('human-approve', 'human-approve'),
                  'T26.lifetime': ('life-approve', 'lifetime-approve')}
    if case_id not in identities:
        return {}
    call_id, label = identities[case_id]
    events = [lex(line).value for _, line in traces]
    finishes = [e for e in events if e.get('type') == 'tool.finished'
                and e.get('data', {}).get('call_id') == call_id]
    assert len(finishes) == 1, 'human approval lacks one owned result'
    finish = finishes[0]
    key = Tools.key(finish)
    starts = [e for e in events if e.get('type') == 'tool.started' and Tools.key(e) == key]
    roots = [e for e in events if e.get('type') == 'run.started' and e.get('run_id') == key[0]]
    accepted = [e for e in events if e.get('type') == 'model.accepted'
                and e.get('run_id') == key[0] and e.get('step') == key[1]]
    assert len(starts) == len(roots) == len(accepted) == 1, 'human approval lacks call provenance'
    start, root, accept = starts[0], roots[0], accepted[0]
    workspace, target = normal.root + '/workspace', normal.root + '/target'
    call = {field: start['data'].get(field) for field in ('call_id', 'name', 'arguments')}
    assert call['name'] == 'request_workspace_access'
    assert json.loads(call['arguments']) == {'path': target}, 'human approval path differs'
    assert accept['data'].get('blocks') == [{'kind': 'tool_call', 'call': call}]
    assert root['seq'] < accept['seq'] < start['seq'] < finish['seq']
    assert root['data']['workspace'] == start['data']['workspace'] == workspace
    assert root['data']['approved_workspaces'] == [workspace], 'human approval was preapproved'
    begin, end = [dt.datetime.fromisoformat(e['time'].replace('Z', '+00:00')).timestamp()
                  for e in (start, finish)]
    owners = [(index, item) for index, item in enumerate(invocations, 1)
              if item['started'] <= begin <= end <= item['ended']]
    assert len(owners) == 1, 'human approval lacks one terminal invocation'
    index, invocation = owners[0]
    cols = 160 if case_id == 'T23.human-consent' else 120
    assert invocation.get('terminal') in ({'rows': 40, 'cols': cols},
                                          {'rows': 40, 'cols': cols, 'stdout_tty': False})
    assert '--allow-workspace' not in invocation['args']
    prefix = 'invoke-%03d' % index
    raw = observations[prefix + '.tty']
    assert raw == invocation['tty'] == invocation['stderr'], 'human terminal bytes differ'
    journal = json.loads(observations[prefix + '.terminal.json'])
    assert journal['version'] == 1 and journal['initial_size'] == [40, cols]
    inputs = [bytes.fromhex(e['hex']) for e in journal['events'] if e['kind'] == 'input']
    assert b''.join(inputs) == invocation['stdin'], 'human input lacks raw invocation witness'
    shots = [shot for shot in journal['snapshots'] if shot['label'] == label]
    assert len(shots) == 1, 'human approval lacks one pre-input screen'
    shot = shots[0]
    assert type(shot['offset']) is int and 0 < shot['offset'] <= len(raw)
    replay(raw, journal)
    screen = _screen(shot['state'])
    assert 'Location: ' + json.dumps(target) in screen
    assert 'Workspace consent' in screen and 'Allow for this session' in screen
    assert '❯ 2  Deny' in screen, 'human consent default was not deny'
    assert 'Authority: read, write, and execute' in screen
    assert 'until this chat exits' in screen
    boundaries = [i for i, event in enumerate(journal['events'])
                  if event == {'kind': 'snapshot', 'offset': shot['offset'], 'label': label}]
    assert len(boundaries) == 1
    following = journal['events'][boundaries[0] + 1]
    assert following == {'kind': 'input', 'offset': shot['offset'], 'hex': b'\x1b[A\r'.hex()}, 'human approval lacks highlighted Enter'
    outcome = finish['data'].get('outcome')
    expected = {'workspace': workspace, 'ok': True, 'code': 'ok', 'message': '',
                'data': {'approved_path': target, 'workspace': workspace, 'missing': False},
                'truncated': False, 'effect': 'none'}
    assert go_json(outcome) == go_json(expected), 'human approval envelope differs'
    changes = [(('data', 'approved_path'), '<ROOT>/target', 'tool-human-approved-workspace-path'),
               (('data', 'workspace'), '<ROOT>/workspace', 'tool-human-approved-workspace-path')]
    return {key: (outcome, changes)}


def cancelled_exec_executable(normal, traces, invocations, observations, case_id):
    """Bind C38's resolved executable to its pre-invocation isolated PATH evidence."""
    if case_id != 'C38.interrupt-running-command':
        return {}
    proof = json.loads(observations['cli-exec-executable.json'])
    executable = normal.root + '/home/.parity-bin/python3'
    assert set(proof) == {'argv', 'cwd', 'search_path', 'executable', 'target'}, 'cancelled exec executable evidence shape differs'
    assert proof['argv'] == ['python3', 'exec-ready.py'] and proof['cwd'] == '.', 'cancelled exec command differs from fixture'
    assert proof['executable'] == executable and proof['search_path'] == str(Path(executable).parent) + os.pathsep + os.defpath, 'cancelled exec PATH allocation differs'
    assert isinstance(proof['target'], str) and Path(proof['target']).is_absolute(), 'cancelled exec link lacks its independently observed target'
    events = [lex(line).value for _, line in traces]
    finishes = [event for event in events if event.get('type') == 'tool.finished' and event['data'].get('name') == 'exec']
    assert len(finishes) == 1 and len(invocations) == 1, 'cancelled exec lacks one result and invocation'
    finish = finishes[0]
    key = Tools.key(finish)
    starts = [event for event in events if event.get('type') == 'tool.started' and Tools.key(event) == key]
    roots = [event for event in events if event.get('type') == 'run.started' and event.get('run_id') == finish['run_id']]
    ends = [event for event in events if event.get('type') == 'run.finished' and event.get('run_id') == finish['run_id']]
    assert len(starts) == len(roots) == len(ends) == 1, 'cancelled exec lacks its call and Run witnesses'
    start, root, end = starts[0], roots[0], ends[0]
    assert root['seq'] < start['seq'] < finish['seq'] < end['seq'] and end['data']['status'] == 'cancelled', 'cancelled exec lifecycle differs'
    assert root['session_id'] == start['session_id'] == finish['session_id'] == end['session_id'], 'cancelled exec Session differs'
    assert root['data']['workspace'] == start['data']['workspace'] == normal.root + '/workspace', 'cancelled exec workspace differs'
    assert start['data']['name'] == 'exec' and json.loads(start['data']['arguments']) == dict(argv=proof['argv'], cwd=proof['cwd']), 'cancelled exec start differs from fixture'
    begin, stop = [dt.datetime.fromisoformat(event['time'].replace('Z', '+00:00')).timestamp() for event in (start, finish)]
    assert invocations[0]['started'] <= begin <= stop <= invocations[0]['ended'], 'cancelled exec lies outside its invocation'
    outcome = finish['data']['outcome']
    data = outcome['data']
    assert outcome['workspace'] == normal.root + '/workspace' and outcome['ok'] is False and outcome['code'] == 'cancelled' and outcome['effect'] == 'unknown', 'cancelled exec outcome differs'
    assert outcome['truncated'] is False and data['argv'] == proof['argv'] and data['cwd'] == proof['cwd'] and data['termination_reason'] == 'cancelled', 'cancelled exec command result differs'
    assert data['resolved_executable'] == executable, 'cancelled exec resolved executable differs from isolated PATH'
    return {key: (outcome, [(('data', 'resolved_executable'), '<ROOT>/home/.parity-bin/python3', 'cli-exec-executable')])}


ECHO_KIND = 'tool-human-workspace-echo'


class WorkspaceEchoes:
    """Certify exact consent-owned results in later history and serialized requests.

    Merge path_outcomes into Tools, then install json on Normalizer before Derived
    is constructed. Direct finishes and same-run approval history remain Tools'
    responsibility; all other echoes use the same independently certified slots.
    """

    def __init__(self, normal, traces, invocations, observations, case_id):
        self.normal = normal
        self.base_json = normal.json
        self.path_outcomes = human_workspace_outcomes(normal, traces, invocations, observations, case_id)
        self.owners = {}
        if not self.path_outcomes:
            return
        events = [lex(line).value for _, line in traces]
        workspace, target = normal.root + '/workspace', normal.root + '/target'
        calls = {'human-approve': ('request_workspace_access', target, workspace, None),
                 'human-before-switch': ('list_files', '.', workspace, None),
                 'human-switch': ('switch_workspace', target, workspace, target),
                 'human-read': ('read_file', 'proof.txt', target, None),
                 'human-child': ('switch_workspace', target + '/child', target, target + '/child'),
                 'human-return': ('switch_workspace', workspace, target + '/child', workspace),
                 'human-parent': ('switch_workspace', normal.root, workspace, None),
                 'human-deny': ('request_workspace_access', normal.root + '/sibling', workspace, None),
                 'human-after-denial': ('list_files', '.', workspace, None)}
        if case_id == 'T26.lifetime':
            calls = {'life-approve': ('request_workspace_access', target, workspace, None),
                     'life-new-request': ('request_workspace_access', target, workspace, None),
                     'life-new-list': ('list_files', '.', workspace, None)}
            for phase in ('reset', 'manual', 'fresh'):
                calls.update({'life-' + phase + '-switch': ('switch_workspace', target, workspace, target),
                              'life-' + phase + '-read': ('read_file', 'proof.txt', target, None),
                              'life-' + phase + '-return': ('switch_workspace', workspace, target, workspace)})
        for call_id, (name, argument, source, destination) in calls.items():
            finishes = [e for e in events if e.get('type') == 'tool.finished'
                        and e.get('data', {}).get('call_id') == call_id]
            assert len(finishes) == 1, 'workspace echo lacks one owned finish'
            finish = finishes[0]
            key = Tools.key(finish)
            starts = [e for e in events if e.get('type') == 'tool.started' and Tools.key(e) == key]
            accepted = [e for e in events if e.get('type') == 'model.accepted'
                        and (e.get('run_id'), e.get('step')) == key[:2]]
            assert len(starts) == len(accepted) == 1, 'workspace echo lacks call witnesses'
            start, accept = starts[0], accepted[0]
            call = {field: start['data'].get(field) for field in ('call_id', 'name', 'arguments')}
            assert call['name'] == finish['data']['name'] == name
            assert json.loads(call['arguments']) == {'path': argument}
            assert accept['data']['blocks'] == [{'kind': 'tool_call', 'call': call}]
            assert accept['seq'] < start['seq'] < finish['seq']
            assert start['data']['workspace'] == source
            begin, end = [dt.datetime.fromisoformat(e['time'].replace('Z', '+00:00')).timestamp()
                          for e in (start, finish)]
            assert sum(i['started'] <= begin <= end <= i['ended'] for i in invocations) == 1
            outcome = finish['data']['outcome']
            assert outcome['workspace'] == source
            slots = [(('workspace',), source)]
            if key in self.path_outcomes:
                slots.extend([(('data', 'approved_path'), target), (('data', 'workspace'), workspace)])
            if destination is not None:
                assert outcome['ok'] is True and outcome['code'] == 'ok'
                assert outcome['data']['changed'] is True
                assert outcome['data']['workspace_state']['kind'] == 'workspace_state'
                slots.extend([(('data', 'previous_workspace'), source),
                              (('data', 'workspace'), destination),
                              (('data', 'workspace_state', 'workspace'), destination)])
            for location, expected in slots:
                value = outcome
                for field in location:
                    value = value[field]
                assert value == expected, 'workspace echo allocation slot differs'
            self.owners[call_id] = (key, name, outcome, slots)

    def json(self, raw, artifact, path=(), event=None, opaque=False, response_slot=None):
        """Project only typed outcome leaves and their exact serialized echoes."""
        normalized = self.base_json(raw, artifact, path, event, opaque, response_slot)
        if not self.owners or path[:1] == ('provider-response',):
            return normalized
        top = lex(raw, opaque=opaque)
        root = top.value
        run = root.get('run_id') if isinstance(root, dict) else None
        same_run = isinstance(root, dict) and root.get('type') == 'model.requested'
        return self.project(raw, normalized, artifact, path, run, same_run)

    def project(self, raw, normalized, artifact, path, run=None, same_run=False, call_id=None):
        before, after = lex(raw), lex(normalized)
        changes = []

        def outcome(left, right, location, call_id, name=None, direct=False):
            owner = self.owners.get(call_id)
            if not owner:
                return
            key, tool_name, expected, slots = owner
            assert name is None or name == tool_name, 'workspace echo tool name differs'
            assert go_json(left.value) == go_json(expected), 'workspace echo differs from owned raw outcome'
            for fields, value in slots:
                # Tools already owns this unembedded same-run approval envelope.
                if direct and same_run and run == key[0] and key in self.path_outcomes and fields != ('workspace',):
                    continue
                a, b = left, right
                for field in fields:
                    a, b = child(a, field), child(b, field)
                replacement = '<ROOT>' + value[len(self.normal.root):]
                assert b.value in (value, replacement), 'workspace echo slot changed before certification'
                if b.value == replacement:
                    continue
                encoded = string_spans(normalized[b.start:b.end], a.value, replacement)
                self.normal.record(artifact, path + location + fields, normalized[b.start:b.end], encoded,
                                   ECHO_KIND, b.start, b.end)
                changes.append((b.start, b.end, encoded))

        def walk(a, b, location=()):
            value = a.value
            if isinstance(value, dict):
                if value.get('kind') in ('text', 'refusal') or value.get('role') == 'assistant':
                    return  # Model text and provider-native output are never path slots.
                if value.get('kind') == 'tool' and isinstance(value.get('tool'), dict):
                    tool = child(a, 'tool')
                    other = child(b, 'tool')
                    outcome(child(tool, 'outcome'), child(other, 'outcome'), location + ('tool', 'outcome'),
                            tool.value.get('call_id'), tool.value.get('name'), direct=True)
                    return
                if value.get('type') == 'function_call_output' and isinstance(value.get('output'), str):
                    if value.get('call_id') not in self.owners:
                        return
                    text, other = child(a, 'output'), child(b, 'output')
                    projected = self.project(text.value.encode(), other.value.encode(), artifact,
                                             path + location + ('output', 'json'), call_id=value['call_id'])
                    replacement = string_spans(normalized[other.start:other.end], other.value, projected.decode())
                    if replacement != normalized[other.start:other.end]:
                        changes.append((other.start, other.end, replacement))
                    return
                for field, item in a.children:
                    if field == 'native' or (value.get('type') == 'tool.finished' and field == 'data'):
                        continue
                    walk(item, child(b, field), location + (field,))
            elif isinstance(value, list):
                for index, item in a.children:
                    walk(item, child(b, index), location + (index,))
            elif isinstance(value, str):
                handoff = 'Historical conversation supplied by re:agent, encoded as JSON records below.'
                if location[-1:] == ('request_body',) and isinstance(before.value, dict) and before.value.get('type') == 'api.attempt.started':
                    replacement = self.project(value.encode(), b.value.encode(), artifact, path + location + ('json',)).decode()
                elif location[-1:] == ('text',) and value.startswith(handoff):
                    prefix, records = value.rsplit('\n', 1)
                    new_prefix, new_records = b.value.rsplit('\n', 1)
                    assert prefix == new_prefix
                    replacement = prefix + '\n' + self.project(records.encode(), new_records.encode(), artifact,
                                                               path + location + ('handoff',)).decode()
                else:
                    return
                encoded = string_spans(normalized[b.start:b.end], b.value, replacement)
                if encoded != normalized[b.start:b.end]:
                    changes.append((b.start, b.end, encoded))

        if call_id is not None:
            outcome(before, after, (), call_id)
        else:
            walk(before, after)
        return replace_spans(normalized, changes)

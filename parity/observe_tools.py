"""Narrow git_do substitutions backed by raw HTTP or command witnesses."""
import datetime as dt
import json
import re

from parity.invocation_time import elapsed
from parity.observe import child, go_json, go_string, lex, replace_spans, string_spans
from parity.observe_tools_exec import Exec
from parity.observe_tools_arguments import Arguments
from parity.observe_tools_index import KIND as INDEX_KIND, index_projection


INSPECT_PREFIX = ['git', '--no-pager', '-c', 'core.fsmonitor=false', '-c', 'core.hooksPath=/dev/null',
                  '-c', 'core.attributesFile=/dev/null', '-c', 'core.quotePath=true', '-c', 'diff.external=',
                  '-c', 'diff.submodule=short', '-c', 'log.showSignature=false', '-c', 'credential.helper=',
                  '-c', 'protocol.allow=never', '-c', 'gc.auto=0', '-c', 'maintenance.auto=false']


ARTIFACTS = frozenset({'git/independent-commands.json', 'git/git-commands.jsonl',
                       'git/gh-commands.jsonl', 'git/recipe-manifest.json'})


class Tools:
    def __init__(self, normal, traces, captures, invocations=(), case_id=None, observations=None):
        self.normal = normal
        self.case_id = case_id or ''
        self.captures = captures
        self.invocations = invocations
        self.events = [lex(line).value for _, line in traces]
        self.observations = observations or {}
        self.exec = Exec(normal, self.events, invocations, self.case_id)
        self.arguments = Arguments(normal, self.events, self.case_id)
        self.outcomes = {}
        self.path_outcomes = self.workspace_outcomes()
        self.used_captures = set()
        self.remote = None
        self.enabled = bool(re.fullmatch(r'T(?:3[89]|4[0-9]|5[0-4])\..+', self.case_id)
                            or self.case_id in ('W53.propagation.git-do', 'T59.maps-numbers'))
        if not self.enabled:
            return
        for finish in self.events:
            data = finish.get('data', {})
            if finish.get('type') != 'tool.finished' or data.get('name') != 'git_do':
                continue
            outcome = data.get('outcome', {})
            result = outcome.get('data')
            if not isinstance(result, dict) or 'decision' not in result:
                continue
            starts = [e for e in self.events if e.get('type') == 'tool.started'
                      and self.key(e) == self.key(finish) and e.get('data', {}).get('name') == 'git_do']
            if len(starts) != 1 or starts[0]['seq'] >= finish['seq']:
                raise AssertionError('git_do result lacks one preceding owned call')
            start = starts[0]
            begin, end = [dt.datetime.fromisoformat(e['time'].replace('Z', '+00:00')).timestamp()
                          for e in (start, finish)]
            owners = [i for i in invocations if i['started'] <= begin <= end <= i['ended']]
            if len(owners) != 1:
                raise AssertionError('git_do result lacks one invocation witness')
            key = self.key(finish)
            if key in self.outcomes:
                raise AssertionError('duplicate git_do result owner')
            substitutions = []
            if 'jev_ms' in result:
                self.jev(start, result, begin, end, owners[0])
                substitutions.append((('data', 'jev_ms'), b'0', 'git-do-jev-ms'))
            substitutions.extend(self.remote_spans(result))
            substitutions.extend(self.failure_message(outcome, substitutions))
            self.outcomes[key] = (outcome, substitutions)

    @staticmethod
    def key(event):
        return (event.get('run_id'), event.get('step'), event.get('data', {}).get('call_id'))

    def jev(self, start, result, begin, end, invocation):
        duration, count = result['jev_ms'], result.get('jev_calls')
        limit = elapsed(invocation)*1000
        if 'monotonic_started' not in invocation:
            limit = min((end-begin)*1000 + 1, limit)
        if (result.get('chosen_by') != 'jev' or type(duration) is not int or not 0 <= duration <= limit
                or type(count) is not int or not 1 <= count <= 2):
            raise AssertionError('git_do Jev timing escaped owned call bounds')
        args = json.loads(start['data']['arguments'])
        intent = args.get('intent')
        if not isinstance(intent, str) or not intent:
            raise AssertionError('git_do Jev timing lacks input intent')
        offered = result['decision'].get('offered', [])
        prefix, separator, rest = intent.strip().partition(':')
        if separator and prefix in offered:
            intent = rest.strip()
        expected = 'Request: ' + intent + '\nRepository: '
        matches = []
        for index, capture in enumerate(self.captures):
            if (capture.get('method') != 'POST' or not capture.get('path', '').endswith('/systemone')
                    or capture.get('authority') != self.normal.authority):
                continue
            try:
                packet = lex(capture['body']).value
                state = packet.get('state', '')
                questions = packet.get('questions', {})
                snapshot = json.loads(state[len(expected):]) if state.startswith(expected) else None
            except (ValueError, TypeError, AttributeError):
                continue
            if (packet.get('model') == 'jev-1.13.0' and snapshot == result['decision'].get('snapshot')
                    and set(questions) in ({'outcome', 'scope'}, {'target'})
                    and all(q.get('type') == 'choice' for q in questions.values())):
                matches.append(index)
        if len(matches) != count or any(index in self.used_captures for index in matches):
            raise AssertionError('git_do Jev timing lacks independent uniquely owned HTTP captures')
        self.used_captures.update(matches)

    def remote_spans(self, result):
        changes = []
        for index, step in enumerate(result.get('steps') or []):
            output = step.get('output')
            argv = step.get('argv', [])
            if not isinstance(output, str) or self.normal.root not in output:
                continue
            # Only Git's remote diagnostic line is admitted, never commit messages or file output.
            if not (argv[:2] in (['git', 'push'], ['git', 'fetch'])):
                continue
            remote = self.normal.root + '/origin.git'
            matches = self.remote_matches(output)
            if not matches:
                continue
            self.remote_allocation()
            records = [json.loads(line) for line in self.observations.get('git/git-commands.jsonl', b'').splitlines()]
            witnesses = [r for r in records if r.get('argv') == argv and r.get('exit_code') == step.get('exit_code')
                         and r.get('stdout', '') + r.get('stderr', '') == output]
            if len(witnesses) != 1:
                raise AssertionError('git_do remote output lacks one exact independent command result')
            raw = output.encode()
            spans = [(len(output[:m.start(1)].encode()), len(output[:m.end(1)].encode()), b'<ROOT>/origin.git')
                     for m in matches]
            replacement = replace_spans(raw, spans).decode()
            changes.append((('data', 'steps', index, 'output'), replacement, 'git-do-allocated-remote'))
        return changes

    def failure_message(self, outcome, substitutions):
        if self.case_id != 'T52.conflict-lease.lease':
            return []
        steps = outcome['data'].get('steps') or []
        failed = [(index, step) for index, step in enumerate(steps) if step.get('exit_code') != 0]
        if not failed:
            return []
        if len(failed) != 1 or failed[0][0] != len(steps) - 1:
            raise AssertionError('git_do failure message lacks one final failed step')
        index, step = failed[0]
        projected = [value for location, value, _ in substitutions if location == ('data', 'steps', index, 'output')]
        if not projected:
            return []
        prefix = ' '.join(step['argv']) + ' exited %d: ' % step['exit_code']
        if (outcome.get('ok') is not False or outcome.get('code') != 'step_failed'
                or outcome['data'].get('status') != 'failed'
                or outcome['data'].get('reason') != prefix + step['output'].strip()
                or outcome.get('message') != prefix + step['output'].strip()):
            raise AssertionError('git_do failure message differs from its exact failed-step template')
        return [(location, prefix + projected[0].strip(), 'git-do-allocated-remote')
                for location in (('message',), ('data', 'reason'))]

    def workspace_outcomes(self):
        cases = {'T23.current': ('workspace', 'workspace', False),
                 'T23.preapproved-absolute': ('target', 'target', False),
                 'T23.preapproved-symlink': ('alias', 'target', False)}
        cases.update({'T24.' + change: ('prospective-parent/target', 'prospective-parent/target', True)
                      for change in ('create', 'symlink', 'parent-swap', 'parent-redirect')})
        refusal = self.case_id == 'T03.outside'
        if not refusal and self.case_id not in cases:
            return {}
        name = 'read_file' if refusal else 'request_workspace_access'
        owners = {}
        for finish in self.events:
            data = finish.get('data', {})
            if finish.get('type') != 'tool.finished' or data.get('name') != name:
                continue
            key = self.key(finish)
            starts = [e for e in self.events if e.get('type') == 'tool.started' and self.key(e) == key
                      and e.get('data', {}).get('name') == name]
            accepted = [e for e in self.events if e.get('type') == 'model.accepted'
                        and e.get('run_id') == key[0] and e.get('step') == key[1]]
            roots = [e for e in self.events if e.get('type') == 'run.started' and e.get('run_id') == key[0]]
            if len(starts) != 1 or len(accepted) != 1 or len(roots) != 1:
                raise AssertionError('workspace result lacks unique independent call and root witnesses')
            start, accept, root = starts[0], accepted[0], roots[0]
            call = {field: start['data'].get(field) for field in ('call_id', 'name', 'arguments')}
            if (accept.get('data', {}).get('blocks') != [{'kind': 'tool_call', 'call': call}]
                    or not root['seq'] < accept['seq'] < start['seq'] < finish['seq']
                    or root['data'].get('workspace') != self.normal.root + '/workspace'
                    or start['data'].get('workspace') != root['data']['workspace']):
                raise AssertionError('workspace result call or recorded allocation differs')
            args = json.loads(call['arguments'])
            source, destination, missing = ('outside', 'workspace', False) if refusal else cases[self.case_id]
            if args != {'path': self.normal.root + '/' + source}:
                raise AssertionError('workspace result arguments differ from the allocated fixture path')
            workspace = root['data']['workspace']
            outcome = data.get('outcome')
            if refusal:
                expected = {'workspace': workspace, 'ok': False, 'code': 'invalid_path',
                            'message': 'path must be inside the workspace, ' + workspace,
                            'data': None, 'truncated': False, 'effect': 'none'}
                location, replacement, kind = ('message',), 'path must be inside the workspace, <ROOT>/workspace', 'tool-workspace-refusal'
            else:
                approved = self.normal.root + '/' + destination
                approvals = root['data'].get('approved_workspaces')
                if (not isinstance(approvals, list) or any(not isinstance(path, str) for path in approvals)
                        or approved not in approvals):
                    raise AssertionError('workspace approval lacks independent recorded preapproval')
                expected = {'workspace': workspace, 'ok': True, 'code': 'ok', 'message': '',
                            'data': {'approved_path': approved, 'workspace': workspace, 'missing': missing},
                            'truncated': False, 'effect': 'none'}
                location, replacement, kind = ('data', 'approved_path'), '<ROOT>/' + destination, 'tool-approved-workspace-path'
            if (outcome != expected or key in owners or outcome.get('ok') is not (not refusal)
                    or outcome.get('truncated') is not False
                    or (not refusal and type(outcome['data'].get('missing')) is not bool)):
                raise AssertionError('workspace result differs from its exact owned envelope')
            changes = [(location, replacement, kind)]
            if not refusal:
                changes.append((('data', 'workspace'), '<ROOT>/workspace', kind))
            owners[key] = (outcome, changes)
        return owners

    def workspace_trace(self, raw, normalized, artifact):
        if not self.path_outcomes:
            return normalized
        event = lex(raw).value
        if event.get('type') == 'tool.finished' and self.key(event) in self.path_outcomes:
            return self.outcome(raw, normalized, artifact, self.path_outcomes[self.key(event)], ('data', 'outcome'))
        if event.get('type') != 'model.requested':
            return normalized
        for index in reversed(range(len(event.get('data', {}).get('history') or []))):
            entry = event['data']['history'][index]
            tool = entry.get('tool', {}) if entry.get('kind') == 'tool' else {}
            owners = [value for (run, _, call), value in self.path_outcomes.items()
                      if run == event.get('run_id') and call == tool.get('call_id')
                      and tool.get('name') == ('read_file' if self.case_id == 'T03.outside' else 'request_workspace_access')]
            if len(owners) > 1:
                raise AssertionError('ambiguous workspace result history owner')
            if owners:
                normalized = self.outcome(raw, normalized, artifact, owners[0], ('data', 'history', index, 'tool', 'outcome'))
        return normalized

    def outcome(self, raw, normalized, artifact, owner, path):
        before, after = lex(raw), lex(normalized)
        for key in path:
            before, after = child(before, key), child(after, key)
        if before.value != owner[0]:
            raise AssertionError('tool echo differs from its owned raw outcome')
        changes = []
        for location, replacement, kind in owner[1]:
            left, right = before, after
            for key in location:
                left, right = child(left, key), child(right, key)
            if raw[left.start:left.end] != normalized[right.start:right.end]:
                raise AssertionError('tool slot changed before witnessed normalization')
            if isinstance(replacement, str):
                replacement = string_spans(normalized[right.start:right.end], left.value, replacement)
            self.normal.record(artifact, path + location, normalized[right.start:right.end], replacement,
                               kind, right.start, right.end)
            changes.append((right.start, right.end, replacement))
        return replace_spans(normalized, changes)

    def trace(self, raw, normalized, artifact):
        normalized = self.exec.trace(raw, normalized, artifact)
        normalized = self.arguments.trace(raw, normalized, artifact)
        normalized = self.workspace_trace(raw, normalized, artifact)
        if not self.enabled:
            return normalized
        event = lex(raw).value
        if (event.get('type') == 'tool.finished' and event.get('data', {}).get('name') == 'git_do'
                and self.key(event) in self.outcomes):
            return self.outcome(raw, normalized, artifact, self.outcomes[self.key(event)], ('data', 'outcome'))
        if event.get('type') != 'model.requested':
            return normalized
        # Process in reverse so earlier token offsets remain valid after each replacement.
        history = event.get('data', {}).get('history') or []
        for index in reversed(range(len(history))):
            entry = history[index]
            tool = entry.get('tool', {}) if entry.get('kind') == 'tool' else {}
            owners = [value for (run, _, call), value in self.outcomes.items()
                      if run == event.get('run_id') and call == tool.get('call_id') and tool.get('name') == 'git_do']
            if not owners:
                continue
            if len(owners) != 1:
                raise AssertionError('ambiguous git_do history owner')
            normalized = self.outcome(raw, normalized, artifact, owners[0], ('data', 'history', index, 'tool', 'outcome'))
        return normalized

    def remote_allocation(self):
        if self.remote is not None:
            return self.remote
        remote = self.normal.root + '/origin.git'
        commands = json.loads(self.observations.get('git/independent-commands.json', b'[]'))
        required = [['git', 'init', '-q', '--bare', '-b', 'main', remote],
                    ['git', 'remote', 'add', 'origin', remote]]
        if not all(any(c.get('argv') == wanted and c.get('exit_code') == 0 for c in commands)
                   for wanted in required):
            raise AssertionError('git_do remote lacks independent fixture allocation commands')
        self.remote = remote
        return remote

    def artifact(self, raw, artifact):
        """Normalize only independently certified fixture command slots."""
        if not self.enabled or artifact not in ARTIFACTS:
            return raw
        if raw != self.observations.get(artifact):
            raise AssertionError('Git command artifact differs from retained independent witness')
        if artifact == 'git/recipe-manifest.json':
            return self.recipe_artifact(raw, artifact)
        if artifact.endswith('.jsonl'):
            return self.command_lines(raw, artifact)
        top = lex(raw)
        if not isinstance(top.value, list):
            raise AssertionError('independent Git command artifact must be an array')
        remote = self.normal.root + '/origin.git'
        changes = []
        for index, command in top.children:
            record = command.value
            if not isinstance(record, dict) or set(record) != {'argv', 'exit_code', 'stdout', 'stderr'}:
                raise AssertionError('independent Git command record has an unknown shape')
            argv = record['argv']
            if (not isinstance(argv, list) or not argv or any(not isinstance(arg, str) for arg in argv)
                    or type(record['exit_code']) is not int
                    or any(not isinstance(record[field], str) for field in ('stdout', 'stderr'))):
                raise AssertionError('independent Git command values have an unknown shape')
            slots = []
            if argv in (['git', 'init', '-q', '--bare', '-b', 'main', remote],
                        ['git', 'remote', 'add', 'origin', remote]):
                slots.append((len(argv)-1, '<ROOT>/origin.git'))
            if argv == ['git', 'clone', '-q', remote, self.normal.root + '/other'] and record['exit_code'] == 0:
                slots.extend(((3, '<ROOT>/origin.git'), (4, '<ROOT>/other')))
            if isinstance(argv, list) and len(argv) > 1 and argv[0] == 'git' and argv[1] == '--git-dir=' + remote:
                slots.append((1, '--git-dir=<ROOT>/origin.git'))
            hostile_keys = {'clean': 'filter.evil.clean', 'smudge': 'filter.evil.smudge',
                            'textconv': 'diff.evil.textconv', 'external': 'diff.external', 'fsmonitor': 'core.fsmonitor'}
            key = hostile_keys.get(self.case_id.removeprefix('T40.hostile-programs.'))
            if key and argv == ['git', 'config', key, self.normal.root + '/hostile']:
                commands = top.value
                configured = [c for c in commands if c['argv'] == argv and c['exit_code'] == 0]
                unset = [c for c in commands if c['argv'] == ['git', 'config', '--unset', key] and c['exit_code'] == 0]
                if len(configured) != 2 or len(unset) != 2:
                    raise AssertionError('hostile helper allocation lacks paired fixture configuration commands')
                slots.append((3, '<ROOT>/hostile'))
            for field in ('stdout', 'stderr'):
                replacement = self.remote_output(argv, record[field], field=field, exit_code=record['exit_code'])
                if replacement != record[field]:
                    token = child(command, field)
                    encoded = string_spans(raw[token.start:token.end], token.value, replacement)
                    self.normal.record(artifact, (index, field), raw[token.start:token.end], encoded,
                                       'git-fixture-workspace' if replacement == '<ROOT>/workspace\n' else 'git-do-allocated-remote', token.start, token.end)
                    changes.append((token.start, token.end, encoded))
            for slot, value in slots:
                if value != '<ROOT>/hostile':
                    self.remote_allocation()
                token = child(child(command, 'argv'), slot)
                replacement = go_string(value)
                self.normal.record(artifact, (index, 'argv', slot), raw[token.start:token.end], replacement,
                                   'git-do-allocated-remote', token.start, token.end)
                changes.append((token.start, token.end, replacement))
        return replace_spans(raw, changes)


    def remote_matches(self, output):
        remote = re.escape(self.normal.root + '/origin.git')
        patterns = [r'(?m)^(?:To|From) (' + remote + r')$',
                    r"(?m)^error: failed to push some refs to '(" + remote + r")'$"]
        return [match for pattern in patterns for match in re.finditer(pattern, output)]

    def remote_output(self, argv, output, *, field='stdout', exit_code=0):
        if not isinstance(output, str):
            raise AssertionError('Git command output is not a string')
        if (argv in (['git', 'rev-parse', '--show-toplevel'], INSPECT_PREFIX + ['rev-parse', '--show-toplevel'])
                and field == 'stdout' and type(exit_code) is int and exit_code == 0
                and output == self.normal.root + '/workspace\n'):
            records = json.loads(self.observations.get('git/independent-commands.json', b'[]'))
            seeds = [['git', 'init', '-q', '--object-format=' + algorithm, '-b', 'main'] for algorithm in ('sha1', 'sha256')]
            if not any(record.get('argv') in seeds and record.get('exit_code') == 0 for record in records):
                raise AssertionError('Git workspace diagnostic lacks independent fixture initialization')
            return '<ROOT>/workspace\n'
        if argv[:2] not in (['git', 'push'], ['git', 'fetch']):
            return output
        matches = self.remote_matches(output)
        if not matches:
            return output
        self.remote_allocation()
        raw = output.encode()
        spans = [(len(output[:match.start(1)].encode()), len(output[:match.end(1)].encode()), b'<ROOT>/origin.git')
                 for match in matches]
        return replace_spans(raw, spans).decode()

    def command_lines(self, raw, artifact):
        parts = []
        offset = 0
        for index, line in enumerate(raw.splitlines(keepends=True)):
            top = lex(line)
            record = top.value
            shapes = ({'argv', 'exit_code', 'stdout', 'stderr'}, {'argv', 'barrier'},
                      {'argv', 'exit_code', 'stdout', 'stderr', 'index_snapshot'}) if artifact == 'git/git-commands.jsonl' else ({'argv', 'exit_code', 'output'},)
            if not isinstance(record, dict) or set(record) not in shapes:
                raise AssertionError('recorded Git helper command has an unknown shape')
            if (not isinstance(record['argv'], list) or not record['argv']
                    or record['argv'][0] != ('git' if artifact == 'git/git-commands.jsonl' else 'gh')
                    or any(not isinstance(arg, str) for arg in record['argv'])):
                raise AssertionError('recorded Git helper argv has an unknown shape')
            if 'exit_code' in record and type(record['exit_code']) is not int:
                raise AssertionError('recorded Git helper exit code is not an integer')
            for field in ('stdout', 'stderr', 'output'):
                if field in record and not isinstance(record[field], str):
                    raise AssertionError('recorded Git helper output is not a string')
            if 'barrier' in record and record['barrier'] != 'entered':
                raise AssertionError('recorded Git helper barrier has an unknown value')
            projection = index_projection(record, INSPECT_PREFIX) if artifact == 'git/git-commands.jsonl' else {}
            changes = []
            for field, value in projection.items():
                token = child(top, field)
                replacement = (string_spans(line[token.start:token.end], token.value, value)
                               if field == 'stdout' else go_json(value))
                self.normal.record(artifact, (index, field), line[token.start:token.end], replacement,
                                   INDEX_KIND, offset + token.start, offset + token.end)
                changes.append((token.start, token.end, replacement))
            for field in ('stdout', 'stderr') if 'stdout' in record else ():
                if field in projection:
                    continue
                value = self.remote_output(record['argv'], record[field], field=field, exit_code=record['exit_code'])
                if value == record[field]:
                    continue
                token = child(top, field)
                replacement = string_spans(line[token.start:token.end], token.value, value)
                self.normal.record(artifact, (index, field), line[token.start:token.end], replacement,
                                   'git-fixture-workspace' if value == '<ROOT>/workspace\n' else 'git-do-allocated-remote', offset + token.start, offset + token.end)
                changes.append((token.start, token.end, replacement))
            parts.append(replace_spans(line, changes))
            offset += len(line)
        return b''.join(parts)

    def recipe_artifact(self, raw, artifact):
        top = lex(raw)
        finishes = [event for event in self.events if event.get('type') == 'tool.finished'
                    and event.get('data', {}).get('name') in ('git_do', 'git_inspect')]
        expected = [{field: (event['data']['outcome'].get('data') or {}).get(field)
                     for field in ('steps', 'checks', 'decision')} for event in finishes]
        if top.value != expected:
            raise AssertionError('recipe manifest differs from exact owned raw outcomes')
        changes = []
        for index, command in top.children:
            owner = self.outcomes.get(self.key(finishes[index]))
            for location, value, kind in owner[1] if owner else ():
                if location[:2] != ('data', 'steps'):
                    continue
                token = command
                for key in location[1:]:
                    token = child(token, key)
                replacement = string_spans(raw[token.start:token.end], token.value, value)
                self.normal.record(artifact, (index,) + location[1:], raw[token.start:token.end], replacement,
                                   kind, token.start, token.end)
                changes.append((token.start, token.end, replacement))
        return replace_spans(raw, changes)

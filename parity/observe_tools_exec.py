"""Exec allocation slots certified by fixture inputs and configured invocation env."""
import datetime as dt
import json
import os
import re

from parity.observe import child, lex, replace_spans, string_spans


LITERAL_SOURCE = ('import os,sys,json; print(json.dumps(sys.argv[1:],ensure_ascii=False)); '
                  'print(os.getcwd()); print("stderr sentinel",file=sys.stderr); '
                  'print(json.dumps(dict(os.environ),sort_keys=True))')
LITERAL_ARGV = ['python3', '-c', LITERAL_SOURCE, '*.txt', '$HOME', 'a;b', 'x&&y',
                '$(touch tripwire)', '>', '', '雪']
ALLOW_ENV = ('PATH', 'HOME', 'TMPDIR', 'LANG', 'LC_ALL', 'GOROOT', 'GOPATH', 'GOCACHE', 'GOMODCACHE')


def owns_exec(case_id):
    """Identify fixtures whose helper executable has an independently recorded PATH."""
    return (bool(re.fullmatch(r'(?:T\d{2}|A(?:1[5-9]|2[0-6]|39))\..+', case_id))
            or case_id in ('C15.agents', 'C23.agents-live-wait',
                           'C24.agents-write-recap', 'C25.agents-tree-cost'))


class Exec:
    def __init__(self, normal, events, invocations, case_id):
        self.normal = normal
        self.outcomes = {}
        if not owns_exec(case_id):
            return
        for finish in events:
            data = finish.get('data', {})
            if finish.get('type') != 'tool.finished' or data.get('name') != 'exec':
                continue
            outcome = data.get('outcome', {})
            result = outcome.get('data')
            if not isinstance(result, dict):
                continue
            starts = [e for e in events if e.get('type') == 'tool.started'
                      and self.key(e) == self.key(finish) and e.get('data', {}).get('name') == 'exec']
            if len(starts) != 1 or starts[0]['seq'] >= finish['seq']:
                raise AssertionError('exec allocation lacks one preceding owned call')
            start = starts[0]
            begin, end = [dt.datetime.fromisoformat(e['time'].replace('Z', '+00:00')).timestamp()
                          for e in (start, finish)]
            owners = [i for i in invocations if i['started'] <= begin <= end <= i['ended']]
            if len(owners) != 1:
                raise AssertionError('exec allocation lacks one invocation witness')
            env = owners[0].get('env', {})
            args = json.loads(start['data']['arguments'])
            vectors = [args['argv'], *args.get('then', [])]
            commands = result.get('steps', [result])
            changes = []
            for index, command in enumerate(commands):
                path = ('data', 'steps', index) if 'steps' in result else ('data',)
                if index >= len(vectors) or command.get('argv') != vectors[index]:
                    raise AssertionError('exec command argv differs from configured input')
                program = command.get('resolved_executable')
                allocated = normal.root + '/home/.parity-bin/python3'
                if program == allocated:
                    if (vectors[index][0] != 'python3' or env.get('PATH') != normal.root + '/home/.parity-bin' + os.pathsep + os.defpath):
                        raise AssertionError('exec executable lacks exact configured PATH witness')
                    changes.append((path + ('resolved_executable',), '<ROOT>/home/.parity-bin/python3', 'exec-allocated-executable'))
                if case_id == 'T29.literal-environment':
                    if args != {'argv': LITERAL_ARGV, 'cwd': 'sub'} or len(commands) != 1:
                        raise AssertionError('literal environment fixture inputs changed')
                    expected_env = {key: env[key] for key in ALLOW_ENV if key in env}
                    fixed = {key: 'exec-' + key for key in ('GOROOT', 'GOPATH', 'GOCACHE', 'GOMODCACHE')}
                    if (expected_env != dict(fixed, LANG='C', LC_ALL='C', HOME=normal.root + '/home',
                                             TMPDIR=normal.root + '/tmp', PATH=normal.root + '/home/.parity-bin' + os.pathsep + os.defpath)):
                        raise AssertionError('literal environment lacks exact configured env witness')
                    expected = (json.dumps(LITERAL_ARGV[3:], ensure_ascii=False) + '\n' + normal.root + '/workspace/sub\n'
                                + json.dumps(expected_env, sort_keys=True) + '\n')
                    canonical_env = dict(expected_env, HOME='<ROOT>/home', TMPDIR='<ROOT>/tmp',
                                         PATH='<ROOT>/home/.parity-bin' + os.pathsep + os.defpath)
                    replacement = (json.dumps(LITERAL_ARGV[3:], ensure_ascii=False) + '\n<ROOT>/workspace/sub\n'
                                   + json.dumps(canonical_env, sort_keys=True) + '\n')
                    if command.get('stdout') != expected or command.get('stderr') != 'stderr sentinel\n':
                        raise AssertionError('literal environment stdout differs from independent fixture expectation')
                    changes.append((path + ('stdout',), replacement, 'exec-literal-environment'))
                if re.fullmatch(r'T32\.(?:success-[1-8]|failure-[148])', case_id):
                    source = ('from pathlib import Path; import os,sys; '
                              f'Path("step-{index}").write_text("{index}"); print(os.getcwd()); sys.exit({9 if command.get("exit_code") == 9 else 0})')
                    if (vectors[index] != ['python3', '-c', source] or args.get('cwd') != 'sub'
                            or command.get('stdout') != normal.root + '/workspace/sub\n'):
                        raise AssertionError('exec chain cwd output lacks exact fixture input/output')
                    changes.append((path + ('stdout',), '<ROOT>/workspace/sub\n', 'exec-fixture-cwd'))
            for path, value, _ in list(changes):
                if path[-1] != 'stdout':
                    continue
                command = result
                for part in path[1:-1]:
                    command = command[part]
                if 'stdout_bytes_seen' not in command or command['stdout_bytes_seen'] != len(command['stdout'].encode()):
                    raise AssertionError('exec fixture stdout byte count differs from independently expected bytes')
                changes.append((path[:-1] + ('stdout_bytes_seen',), str(len(value.encode())).encode(), 'exec-fixture-stdout-length'))
            key = self.key(finish)
            if key in self.outcomes:
                raise AssertionError('duplicate exec allocation owner')
            self.outcomes[key] = (outcome, changes)

    @staticmethod
    def key(event):
        return (event.get('run_id'), event.get('step'), event.get('data', {}).get('call_id'))

    def outcome(self, raw, normalized, artifact, owner, path):
        before, after = lex(raw), lex(normalized)
        for key in path:
            before, after = child(before, key), child(after, key)
        if before.value != owner[0]:
            raise AssertionError('exec echo differs from owned raw outcome')
        changes = []
        for location, value, kind in owner[1]:
            left, right = before, after
            for key in location:
                left, right = child(left, key), child(right, key)
            if left.value != right.value:
                raise AssertionError('exec allocation slot changed before witnessed normalization')
            replacement = value if isinstance(value, bytes) else string_spans(normalized[right.start:right.end], left.value, value)
            self.normal.record(artifact, path + location, normalized[right.start:right.end], replacement,
                               kind, right.start, right.end)
            changes.append((right.start, right.end, replacement))
        return replace_spans(normalized, changes)

    def trace(self, raw, normalized, artifact):
        event = lex(raw).value
        if event.get('type') == 'tool.finished' and self.key(event) in self.outcomes:
            return self.outcome(raw, normalized, artifact, self.outcomes[self.key(event)], ('data', 'outcome'))
        if event.get('type') != 'model.requested':
            return normalized
        history = event.get('data', {}).get('history') or []
        for index in reversed(range(len(history))):
            entry = history[index]
            tool = entry.get('tool', {}) if entry.get('kind') == 'tool' else {}
            owners = [value for (run, _, call), value in self.outcomes.items()
                      if run == event.get('run_id') and call == tool.get('call_id') and tool.get('name') == 'exec']
            if not owners:
                continue
            if len(owners) != 1:
                raise AssertionError('ambiguous exec history owner')
            normalized = self.outcome(raw, normalized, artifact, owners[0], ('data', 'history', index, 'tool', 'outcome'))
        return normalized

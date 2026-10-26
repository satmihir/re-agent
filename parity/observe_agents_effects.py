"""Independent lifecycle assertions and event-driven trace completion witnesses."""

import ctypes
import json
import os
from pathlib import Path
import re
import select
import sys
import time


def delivery_ids(results, expected):
    """Require the exact ordered delivery sequence and unique task identities."""
    assert [item['id'] for item in results] == expected, ('lexical delivery order', expected, results)
    identities = [(item['id'], item['run_id']) for item in results]
    assert len(identities) == len(set(identities)), ('duplicate task delivery', identities)


def ledger_summary(summary, workspace, paths):
    """Bind the bounded ledger to exact effects, including uncertainty after truncation."""
    records = [dict(workspace=str(workspace), path=path, operation='create') for path in paths]
    records.append(dict(workspace=str(workspace), path=paths[0], operation='update'))
    retained, omitted, used = [], 0, 128
    for record in records:
        raw = json.dumps(record, ensure_ascii=False, separators=(',', ':'))
        cost = len(json.dumps(raw, ensure_ascii=False).encode()) + 1
        if used + cost > 8192:
            omitted += 1
        else:
            retained.append(record)
            used += cost
    assert retained and omitted > 0, 'fixture must retain and omit effects'
    assert summary['files'] == retained, summary
    assert summary['omitted'] == omitted, summary
    assert summary['exec_unknown'] is True, ('exec_unknown lost from bounded effects', summary)


class TraceTimeout(AssertionError):
    """The requested trace witness did not appear before its deadline."""


class TraceBarrier:
    """Wait on filesystem notifications, not timing guesses, for flushed trace evidence."""

    def __init__(self, root):
        self.root = Path(root)
        self.watches = {}
        self.queue = None
        if sys.platform == 'linux':
            self.libc = ctypes.CDLL(None, use_errno=True)
            self.fd = self.libc.inotify_init1(os.O_CLOEXEC | os.O_NONBLOCK)
            if self.fd < 0:
                raise OSError(ctypes.get_errno(), 'inotify_init1')
        else:
            self.queue = select.kqueue()
            self.fd = self.queue.fileno()
        self._watch()

    def _watch(self):
        for path in [self.root, *self.root.rglob('*')]:
            if not path.is_dir() or path in self.watches:
                continue
            if self.queue is None:
                watch = self.libc.inotify_add_watch(self.fd, os.fsencode(path), 0x100 | 0x80 | 0x2 | 0x8)
                if watch < 0:
                    raise OSError(ctypes.get_errno(), str(path))
            else:
                watch = os.open(path, os.O_RDONLY)
                self.queue.control([select.kevent(watch, filter=select.KQ_FILTER_VNODE,
                    flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR,
                    fflags=select.KQ_NOTE_WRITE | select.KQ_NOTE_EXTEND)], 0)
            self.watches[path] = watch
        # Directory notifications detect new trace files, while file notifications
        # detect the later append containing run.finished on BSD/macOS too.
        if self.queue is not None:
            for path in self.root.rglob('*.jsonl'):
                if path not in self.watches:
                    watch = os.open(path, os.O_RDONLY)
                    self.queue.control([select.kevent(watch, filter=select.KQ_FILTER_VNODE,
                        flags=select.KQ_EV_ADD | select.KQ_EV_CLEAR,
                        fflags=select.KQ_NOTE_WRITE | select.KQ_NOTE_EXTEND)], 0)
                    self.watches[path] = watch

    def wait(self, predicate, description, timeout=12):
        """Return only once complete JSONL records satisfy the supplied witness."""
        deadline = time.monotonic() + timeout
        while True:
            self._watch()
            records = []
            for path in self.root.rglob('*.jsonl'):
                raw = path.read_bytes()
                for line in raw.splitlines(keepends=True):
                    if line.endswith(b'\n'):
                        records.append(json.loads(line))
            if predicate(records):
                return records
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TraceTimeout(('missing trace witness', description))
            if self.queue is None:
                if select.select([self.fd], [], [], remaining)[0]:
                    os.read(self.fd, 1024 * 1024)
            else:
                self.queue.control(None, 1024, remaining)

    def finished(self, agent):
        """Witness the named child's complete terminal Run record."""
        return self.wait(lambda records: any(record['type'] == 'run.finished'
            and record.get('agent_id', 'root') == agent for record in records), agent + ' run.finished')

    def close(self):
        """Release every filesystem notification descriptor."""
        if self.queue is None:
            os.close(self.fd)
        else:
            for descriptor in self.watches.values():
                os.close(descriptor)
            self.queue.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


def local_read_denied(outcome):
    """Require a local missing-read failure, not another incidental write error."""
    assert outcome['ok'] is False and outcome['code'] == 'invalid_arguments' and outcome['message'] == 'read the file before changing it (read_file records what you have seen)', ('sibling missing_read authorization', outcome)
    assert outcome['effect'] == 'none' and outcome['data'] is None, ('sibling missing_read denial published effects', outcome)


def finish_continuation(stdout):
    """Reject the premature answer even when a broken parent silently joins work."""
    assert b'premature root answer' not in stdout, 'premature root completion omitted finish-rule continuation'
    assert b'root final after data' in stdout, 'missing finish-rule continuation after descendant release'


def recap_slots(ctx, invocation, prefix, text, traces):
    """Bind C24's recap workspace cells to root, child and applied-tool evidence."""
    if Path(ctx.root).name != 'C24.agents-write-recap':
        return []
    args = invocation.get('args', [])
    workspace = str(ctx.root / 'workspace')
    assert '--recap' in args and '--workspace' in args and args[args.index('--workspace') + 1] == workspace, 'agent recap lacks invocation owner'
    events = [json.loads(line) for path, line in traces
              if Path(path).relative_to(ctx.artifacts).parts[0] == prefix + '.traces']
    roots = [event for event in events if event['type'] == 'run.finished'
             and not event.get('agent_id') and event['data'].get('effects')]
    assert len(roots) == 1 and len(roots[0]['data']['effects']) == 1, 'agent recap lacks unique root effect'
    effect = roots[0]['data']['effects'][0]
    assert (effect['agent_id'], effect['path'], effect['operation'], effect['effect'], effect['workspace'], effect['tool']) == ('t1', 'child-output.txt', 'create', 'applied', workspace, 'write_file'), 'agent recap root effect differs'
    children = [event for event in events if event['type'] == 'run.finished'
                and event.get('agent_id') == effect['agent_id'] and event.get('run_id') == effect['run_id']]
    assert len(children) == 1 and children[0].get('parent_id') == 'root' and children[0]['data'].get('effects') == [effect], 'agent recap lacks child effect owner'
    tools = [event for event in events if event['type'] == 'tool.finished'
             and event.get('agent_id') == effect['agent_id'] and event.get('run_id') == effect['run_id']
             and event.get('step') == effect['step'] and event['data']['call_id'] == effect['call_id']]
    assert len(tools) == 1 and tools[0]['data']['name'] == effect['tool'], 'agent recap lacks applied tool owner'
    outcome = tools[0]['data']['outcome']
    assert outcome['ok'] is True and outcome['effect'] == 'applied' and outcome['workspace'] == workspace, 'agent recap tool effect differs'
    assert (outcome['data']['path'], outcome['data']['operation'], outcome['data']['size_bytes'], outcome['data']['after_sha256']) == ('child-output.txt', 'create', 19, 'e874238f2fb80194d6515f461dff569277d5381e04784d8fcd54b78f35b98a9e'), 'agent recap tool publication differs'
    matches = list(re.finditer(r'(?:^|\x1b\[[1-9][0-9]*;[1-9][0-9]*H)(?P<line>  agent [^\n\x1b]+)(?=$|\x1b)', text))
    expected = '  agent t1: create child-output.txt (applied) [' + workspace + ']'
    assert len(matches) == 1 and matches[0]['line'] == expected, 'agent recap display differs from owned effect'
    root = str(ctx.root)
    start = matches[0].start('line') + expected.index('[') + 1
    return [(start, root, '<ROOT>', 'terminal-agent-recap-workspace')]

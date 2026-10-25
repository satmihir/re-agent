"""Independent lifecycle assertions and event-driven trace completion witnesses."""

import ctypes
import json
import os
from pathlib import Path
import select
import sys
import time


def delivery_ids(results, expected):
    """Require the exact ordered delivery sequence and unique task identities."""
    assert [item['id'] for item in results] == expected, ('lexical delivery order', expected, results)
    identities = [(item['id'], item['run_id']) for item in results]
    assert len(identities) == len(set(identities)), ('duplicate task delivery', identities)


def ledger_summary(summary, count, workspace, first_path):
    """Bind the bounded ledger to exact effects, including uncertainty after truncation."""
    assert summary['files'] == [{'workspace': str(workspace), 'path': first_path, 'operation': 'create'}], summary
    assert summary['omitted'] == count, summary
    assert summary['exec_unknown'] is True, ('exec_unknown lost from bounded effects', summary)


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
            assert remaining > 0, ('missing trace witness', description)
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
    assert not outcome['ok'] and outcome['code'] == 'invalid_arguments' and 'read the file before changing it' in outcome['message'], ('sibling missing_read authorization', outcome)


def finish_continuation(stdout):
    """Reject the premature answer even when a broken parent silently joins work."""
    assert b'premature root answer' not in stdout, 'premature root completion omitted finish-rule continuation'
    assert b'root final after data' in stdout, 'missing finish-rule continuation after descendant release'

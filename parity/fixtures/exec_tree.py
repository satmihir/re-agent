"""Process-tree fixture: readiness is a pipe barrier, never a timed sleep."""

import ctypes
import errno
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys


class _StartTime(ctypes.Structure):
    _fields_ = [('seconds', ctypes.c_long), ('microseconds', ctypes.c_int32)]


def _darwin_start(pid):
    # kinfo_proc begins with kp_proc.p_un.__p_starttime (struct timeval).
    # Unlike proc_pidinfo, KERN_PROC_PID still returns this identity for zombies.
    query = ctypes.CDLL(None, use_errno=True).sysctl
    query.argtypes = [ctypes.POINTER(ctypes.c_int), ctypes.c_uint, ctypes.c_void_p,
                      ctypes.POINTER(ctypes.c_size_t), ctypes.c_void_p, ctypes.c_size_t]
    query.restype = ctypes.c_int
    mib = (ctypes.c_int * 4)(1, 14, 1, pid)  # CTL_KERN, KERN_PROC, KERN_PROC_PID
    size = ctypes.c_size_t()
    if query(mib, 4, None, ctypes.byref(size), None, 0) != 0:
        raise OSError(ctypes.get_errno(), 'sysctl KERN_PROC_PID size failed')
    buffer = ctypes.create_string_buffer(size.value)
    if query(mib, 4, buffer, ctypes.byref(size), None, 0) != 0:
        if ctypes.get_errno() == errno.ESRCH:
            return None
        raise OSError(ctypes.get_errno(), 'sysctl KERN_PROC_PID failed')
    if size.value == 0:
        return None
    assert size.value >= ctypes.sizeof(_StartTime)
    start = _StartTime.from_buffer_copy(buffer)
    return [start.seconds, start.microseconds]


def process_state(pid):
    """Read one OS process incarnation, including zombies, independently of the harness."""
    if sys.platform == 'linux':
        try:
            # comm may contain spaces and ')'; fields after its final ')' are numeric.
            fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
        except (FileNotFoundError, ProcessLookupError):
            return None
        return {'ppid': int(fields[1]), 'pgid': int(fields[2]), 'state': fields[0],
                'start': [int(fields[19])]}
    if sys.platform != 'darwin':
        raise RuntimeError('exec process identity requires Linux or Darwin')
    start = _darwin_start(pid)
    if start is None:
        return None
    result = subprocess.run(['/bin/ps', '-o', 'pid=,ppid=,pgid=,stat=', '-p', str(pid)],
                            capture_output=True, check=False, timeout=3)
    assert result.returncode in (0, 1), result.stderr
    if not result.stdout.strip() or _darwin_start(pid) != start:
        return None
    found, ppid, pgid, state = result.stdout.decode().split()
    assert int(found) == pid
    return {'ppid': int(ppid), 'pgid': int(pgid), 'state': state, 'start': start}


def await_cleanup(fd):
    """Only this process reads its cleanup channel; no recycled PID can be signaled."""
    while True:
        select.select([fd], [], [])
        if os.read(fd, 1) == b'K':
            os.kill(os.getpid(), signal.SIGKILL)


def main():
    mode, pipes, escape = sys.argv[1:]
    witness = os.open('exec-ready', os.O_WRONLY)
    manifest = os.open('exec-pids', os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    ready_read, ready_write = os.pipe()

    def record(role):
        cleanup = 'exec-cleanup-' + role
        os.mkfifo(cleanup, 0o600)
        fd = os.open(cleanup, os.O_RDWR | os.O_NONBLOCK)
        value = {'role': role, 'pid': os.getpid(), 'pgid': os.getpgrp(),
                 'start': process_state(os.getpid())['start']}
        os.write(manifest, (json.dumps(value) + '\n').encode())
        return fd

    def descendant(role):
        if role == 'grandchild' and escape == 'escape':
            os.setsid()
        if pipes == 'redirected':
            null = os.open(os.devnull, os.O_WRONLY)
            os.dup2(null, 1)
            os.dup2(null, 2)
            os.close(null)
        term_seen = False

        def term(*_):
            nonlocal term_seen
            if not term_seen:
                term_seen = True
                os.write(witness, b'{"term":true}\n')

        signal.signal(signal.SIGTERM, term)
        cleanup = record(role)
        os.write(ready_write, b'R')
        await_cleanup(cleanup)

    cleanup = record('leader')
    child = os.fork()
    if child == 0:
        os.close(cleanup)
        grandchild = os.fork()
        if grandchild == 0:
            descendant('grandchild')
        descendant('child')
    os.close(ready_write)
    count = 0
    while count < 2:
        chunk = os.read(ready_read, 2 - count)
        if not chunk:
            raise RuntimeError('descendant readiness pipe closed early')
        count += len(chunk)
    os.write(witness, b'{"ready":true}\n')
    print('tree ready', flush=True)
    if mode == 'natural':
        return
    await_cleanup(cleanup)


if __name__ == '__main__':
    main()

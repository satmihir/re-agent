"""Machine-wide publication exclusion and dynamically promoted case workloads."""

from contextlib import contextmanager
import errno
import fcntl
import os
from pathlib import Path
import pwd
import stat


def machine_lock_path():
    """Keep the lock stable across workspaces, reference pins and isolated caches."""
    return Path(pwd.getpwuid(os.getuid()).pw_dir) / '.cache/reagent-parity/capture.lock'


def open_lock(path):
    """Open a stable regular lock inode without following namespace symlinks."""
    for parent in reversed(path.parents):
        if parent.is_symlink():
            raise OSError(errno.ELOOP, 'parity lock namespace contains a symlink', str(parent))
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(path, os.O_CREAT | os.O_RDWR | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    if not stat.S_ISREG(os.fstat(descriptor).st_mode):
        os.close(descriptor)
        raise OSError(errno.EINVAL, 'parity lock is not a regular file', str(path))
    return descriptor


@contextmanager
def capture_lock(shared=False):
    """Share read-only comparisons; refuse overlapping capture/cache publication."""
    path = machine_lock_path()
    descriptor = open_lock(path)
    try:
        try:
            fcntl.flock(descriptor, (fcntl.LOCK_SH if shared else fcntl.LOCK_EX) | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise BlockingIOError(errno.EWOULDBLOCK, 'another parity capture/run holds the machine-wide lock', str(path)) from exc
        # Never unlink the file: waiters must keep referring to the same inode.
        yield
    finally:
        os.close(descriptor)


class CaseWorkload:
    """Hold shared case execution until a fixture requires machine-wide isolation."""

    def __init__(self):
        self.descriptor = None
        self.exclusive = False

    def __enter__(self):
        self.descriptor = open_lock(machine_lock_path().with_name('workload.lock'))
        try:
            fcntl.flock(self.descriptor, fcntl.LOCK_SH)
        except BaseException:
            os.close(self.descriptor)
            self.descriptor = None
            raise
        return self

    def require_exclusive(self):
        """Drop shared ownership before waiting, so simultaneous promotions cannot deadlock."""
        if self.exclusive:
            return
        fcntl.flock(self.descriptor, fcntl.LOCK_UN)
        fcntl.flock(self.descriptor, fcntl.LOCK_EX)
        self.exclusive = True

    def __exit__(self, *_):
        os.close(self.descriptor)
        self.descriptor = None

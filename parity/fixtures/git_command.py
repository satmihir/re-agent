#!/usr/bin/env python3
"""Recorded local Git dispatcher with exact argv failure injection."""
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import zlib

settings = json.loads((Path(__file__).parent / 'settings.json').read_text())
args = sys.argv[1:]
fail = settings.get('fail')
env = {**os.environ, **settings['fixed_env']}
barrier = settings.get('barrier', {})
if args == barrier.get('argv'):
    with Path(settings['git_record']).open('a') as stream:
        stream.write(json.dumps({'argv': ['git', *args], 'barrier': 'entered'}) + '\n')
    with open(barrier['ready'], 'w') as ready:
        ready.write('ready\n')
    with open(barrier['release']) as release:
        release.readline()
inspect_prefix = ['--no-pager', '-c', 'core.fsmonitor=false', '-c', 'core.hooksPath=/dev/null',
                  '-c', 'core.attributesFile=/dev/null', '-c', 'core.quotePath=true', '-c', 'diff.external=',
                  '-c', 'diff.submodule=short', '-c', 'log.showSignature=false', '-c', 'credential.helper=',
                  '-c', 'protocol.allow=never', '-c', 'gc.auto=0', '-c', 'maintenance.auto=false']
snapshot_command = args[:len(inspect_prefix) + 5] == inspect_prefix + ['ls-files', '--stage', '--debug', '-z', '--']
if snapshot_command:
    # Read before Git runs: the certificate must not be derived from its stdout.
    git_dir = Path(env.get('GIT_DIR', '.git'))
    if git_dir.is_file():
        marker = git_dir.read_bytes()
        if marker.startswith(b'gitdir: '):
            git_dir = git_dir.parent / os.fsdecode(marker[8:].rstrip(b'\r\n'))
    index_path = Path(env['GIT_INDEX_FILE']) if 'GIT_INDEX_FILE' in env else git_dir / 'index'
    try:
        raw_index = index_path.read_bytes()
        index_snapshot = base64.b64encode(raw_index).decode('ascii')
        if len(raw_index) > 64 * 1024:
            index_snapshot = {'encoding': 'zlib+base64', 'raw_size': len(raw_index),
                              'raw_sha256': hashlib.sha256(raw_index).hexdigest(),
                              'data': base64.b64encode(zlib.compress(raw_index)).decode('ascii')}
    except (FileNotFoundError, NotADirectoryError):
        index_snapshot = {'missing': True}
if args == fail:
    result = subprocess.CompletedProcess(args, 41, b'', b'fixture git command failure\n')
else:
    result = subprocess.run([settings['real_git'], *args], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
if args == settings.get('dirty_after') and result.returncode == 0:
    Path('proof-dirty').write_text('independent mutation after successful command\n')
record = {'argv': ['git', *args], 'exit_code': result.returncode,
          'stdout': result.stdout.decode('utf-8', 'replace'),
          'stderr': result.stderr.decode('utf-8', 'replace')}
if snapshot_command:
    record['index_snapshot'] = index_snapshot
with Path(settings['git_record']).open('a') as stream:
    stream.write(json.dumps(record, ensure_ascii=False) + '\n')
sys.stdout.buffer.write(result.stdout)
sys.stderr.buffer.write(result.stderr)
sys.exit(result.returncode)

#!/usr/bin/env python3
"""Explicit offline gh fake: scripted PR state and complete argv transcript."""
import json
import os
from pathlib import Path
import subprocess
import sys

settings = json.loads((Path(__file__).parent / 'settings.json').read_text())
args = sys.argv[1:]
state_path = Path(settings['gh_state'])
state = json.loads(state_path.read_text())
branch = subprocess.check_output([settings['real_git'], 'branch', '--show-current'], text=True).strip()
head = subprocess.check_output([settings['real_git'], 'rev-parse', 'HEAD'], text=True).strip()
code, output = 0, ''
if args[:2] == ['pr', 'create']:
    if state.get('fail_create'):
        code, output = 43, 'fixture gh create failure\n'
    else:
        state.update(present=True, state='OPEN', branch=args[args.index('--head') + 1])
        output = 'https://example.invalid/pull/7\n'
elif args[:2] == ['pr', 'comment']:
    state.setdefault('replies', []).append(args[args.index('--body') + 1])
    output = 'https://example.invalid/pull/7#comment-1\n'
elif args[:2] == ['pr', 'view']:
    if not state.get('present'):
        code, output = 1, 'no pull requests found\n'
    elif '--comments' in args:
        output = 'PR #7: fixture review\nreviewer: please address the scoped file\n'
    elif '--jq' in args:
        output = 'https://example.invalid/pull/7\n'
    else:
        fields = args[args.index('--json') + 1].split(',')
        data = {'number': 7, 'state': state.get('state', 'OPEN'),
                'comments': [{'body': 'please address the scoped file'}],
                'headRefOid': state.get('head', head), 'headRefName': state.get('branch', branch),
                'url': 'https://example.invalid/pull/7'}
        output = json.dumps({key: data[key] for key in fields}) + '\n'
else:
    code, output = 44, 'unexpected gh argv\n'
state_path.write_text(json.dumps(state))
with Path(settings['gh_record']).open('a') as stream:
    stream.write(json.dumps({'argv': ['gh', *args], 'exit_code': code, 'output': output}) + '\n')
(sys.stdout if code == 0 else sys.stderr).write(output)
sys.exit(code)

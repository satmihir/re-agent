"""Allocation-root cells in independently witnessed workspace terminal fields."""

from datetime import datetime
import json
from pathlib import Path
import posixpath
import re


KIND = 'terminal-workspace-path'
_TOOLS = ('request_workspace_access', 'switch_workspace')
_ROWS = re.compile(r'(?m)(?:^|\x1b\[[1-9][0-9]*;[1-9][0-9]*H)([^\n\x1b]+)(?=$|\n|\x1b)')
_NOTICE = '! exec mode: commands run as you, in '
_NOTICE_END = ', and can read, write, and use the network'


def _truncate(value, width):
    # These leaves allocate ASCII paths; their markers and ellipses are one cell.
    return value if width <= 0 or len(value) <= width else value[:width - 1] + '…'


def _activity(name, target, ok, columns):
    result = ('approved for this conversation' if name == _TOOLS[0] else 'selected') if ok else (
        'permission_denied: workspace access was not approved')
    prefix = '  ' + ('✓' if ok else '✗') + ' ' + name + ' '
    if columns == 0:
        target = _truncate(target, 160)
    else:
        available = columns - len(prefix) - len(' → ')
        if available < 1:
            target, result = '', ''
        elif len(target) + len(result) > available:
            target = _truncate(target, max(1, available - min(len(result), available // 3)))
            result = _truncate(result, available - len(target)) if available > len(target) else ''
    return prefix + target + (' → ' + result if result else ''), len(prefix), target


def _stamp(event):
    value = datetime.fromisoformat(event['time'].replace('Z', '+00:00'))
    assert value.tzinfo is not None, 'workspace terminal witness lacks timezone'
    return value.timestamp()


def slots(ctx, invocation, prefix, raw_printed_text, traces, columns):
    """Return character slots only after complete field and lifecycle certification."""
    identity = ctx.case.id if getattr(ctx, 'case', None) else Path(ctx.root).name
    consent = identity in ('C44.workspace-allow', 'C44.workspace-deny', 'C44.workspace-escape',
                           'C44.workspace-ctrl-c', 'C44.workspace-ctrl-d')
    if identity not in ('T23.human-consent', 'T26.lifetime') and not consent:
        return []
    root = str(ctx.root)
    assert root.isascii() and root.isprintable() and len(root) >= len('<ROOT>')
    assert type(columns) is int and columns >= 0
    events = []
    for path, line in traces:
        relative = Path(path).relative_to(ctx.artifacts)
        if relative.parts[0] != prefix + '.traces':
            continue
        event = json.loads(line)
        assert relative.parts == (prefix + '.traces', event['run_id'], 'events.jsonl'), 'workspace terminal trace owner differs'
        events.append(event)

    def key(event):
        return event['run_id'], event['step'], event['data']['call_id']

    starts = [e for e in events if e.get('type') == 'tool.started' and e['data'].get('name') in _TOOLS]
    finishes = [e for e in events if e.get('type') == 'tool.finished' and e['data'].get('name') in _TOOLS]
    assert len({key(e) for e in starts}) == len(starts), 'duplicate workspace terminal start'
    assert len({key(e) for e in finishes}) == len(finishes), 'duplicate workspace terminal finish'
    assert {key(e) for e in starts} == {key(e) for e in finishes}, 'workspace terminal lacks start/finish pair'
    if consent:
        assert len(starts) == 1 and starts[0]['data']['name'] == 'switch_workspace', 'C44 consent lacks one workspace switch'
    rows = [(match.start(1), match[1]) for match in _ROWS.finditer(raw_printed_text)]
    activities = [(at, line) for at, line in rows if re.match(r'  [✓✗!–] (?:request_workspace_access|switch_workspace) ', line)]
    assert len(activities) == len(starts), 'workspace terminal activity lacks unique lifecycle owner'
    if not starts:
        assert not any(line.startswith('Location: ') for _, line in rows), 'workspace consent lacks lifecycle owner'
        return []
    assert invocation.get('terminal', {}).get('stdout_tty') is False, 'workspace terminal may contain model reply text'
    assert invocation['terminal']['cols'] == columns, 'workspace terminal width differs from launch config'
    args = invocation['args']
    assert args[0] == 'chat' and '--workspace' in args and args[args.index('--workspace') + 1] == root + '/workspace', 'workspace terminal launch owner differs'
    assert '--plan' not in args and '--read-only' not in args and '--allow-workspace' not in args
    assert invocation.get('trace', ctx.artifacts / (prefix + '.traces')) == ctx.artifacts / (prefix + '.traces')
    assert [i for i, item in enumerate(ctx.invocations, 1) if item is invocation] == [int(prefix.removeprefix('invoke-'))], 'workspace terminal invocation owner differs'
    starts.sort(key=lambda event: (_stamp(event), event['seq']))
    active, result, previous_end = {}, [], 0
    used_locations, used_notices = set(), set()

    def allocated(value):
        assert isinstance(value, str) and value.isascii() and value.isprintable()
        assert value == root or value.startswith(root + '/'), 'workspace terminal path escaped allocation'

    def project(at, path, visible=None):
        allocated(path)
        value = path if visible is None else visible
        count = min(len(root), len(value.removesuffix('…')))
        if count:
            assert value[:count] == root[:count]
            result.append((at, root[:count], '<ROOT>'[:count], KIND))

    for start, (activity_at, activity_line) in zip(starts, activities):
        finish = next(e for e in finishes if key(e) == key(start))
        run = start['run_id']
        headers = [e for e in events if e.get('type') == 'run.started' and e['run_id'] == run]
        accepted = [e for e in events if e.get('type') == 'model.accepted' and (e['run_id'], e['step']) == (run, start['step'])]
        assert len(headers) == len(accepted) == 1, 'workspace terminal lacks accepted call/header'
        header, accept = headers[0], accepted[0]
        assert re.fullmatch(r'[0-9a-f]{16}', run) and re.fullmatch(r'[0-9a-f]{16}', header.get('session_id', '')), 'workspace terminal identity differs'
        assert header.get('session_id') == accept.get('session_id') == start.get('session_id') == finish.get('session_id'), 'workspace terminal Session owner differs'
        assert header['seq'] < accept['seq'] < start['seq'] < finish['seq'], 'workspace terminal lifecycle order differs'
        begin, end = _stamp(start), _stamp(finish)
        assert invocation['started'] <= _stamp(header) <= _stamp(accept) <= begin <= end
        owners = [item for item in ctx.invocations if item['started'] <= begin <= end <= item['ended']]
        assert len(owners) == 1 and owners[0] is invocation, 'workspace terminal lacks unique invocation time owner'
        call = {field: start['data'].get(field) for field in ('call_id', 'name', 'arguments')}
        blocks = [{'kind': 'tool_call', 'call': call}]
        if consent:
            blocks.insert(0, {'kind': 'text', 'text': 'Requesting a workspace.'})
        assert accept['data'].get('blocks') == blocks, 'workspace terminal accepted call differs'
        argument = json.loads(call['arguments'])
        assert set(argument) == {'path'} and isinstance(argument['path'], str)
        name, source = call['name'], start['data']['workspace']
        allocated(source)
        assert source == active.get(run, header['data']['workspace']), 'workspace terminal active workspace differs'
        target = posixpath.normpath(posixpath.join(source, argument['path']))
        allocated(target)
        outcome = finish['data']['outcome']
        assert outcome['workspace'] == source and outcome['effect'] == 'none' and outcome['truncated'] is False, 'workspace terminal outcome workspace/effect differs'
        assert type(outcome['ok']) is bool
        ok = outcome['ok']
        if consent:
            assert argument == {'path': root + '/consent-workspace'}, 'C44 consent target differs'
            assert ok is (identity == 'C44.workspace-allow'), 'C44 consent choice differs'
        if ok:
            assert outcome['code'] == 'ok' and outcome['message'] == ''
            data = outcome['data']
            if name == _TOOLS[0]:
                assert data == {'approved_path': target, 'workspace': source, 'missing': False} and data['missing'] is False, 'workspace terminal approved path differs'
            else:
                assert data['previous_workspace'] == source and data['workspace'] == target and data['changed'] is True, 'workspace terminal transition differs'
                assert data['workspace_state']['kind'] == 'workspace_state' and data['workspace_state']['workspace'] == target, 'workspace terminal state target differs'
                active[run] = target
        else:
            denial = 'workspace access was not approved'
            if consent and identity != 'C44.workspace-deny':
                denial = 'workspace consent was cancelled or unavailable'
            assert outcome['code'] == 'permission_denied' and outcome['message'] == denial and outcome['data'] is None, 'workspace terminal denial differs'
        expected, field, visible = _activity(name, target if ok and name == _TOOLS[1] else argument['path'], ok, columns)
        assert activity_line == expected, 'workspace terminal activity rendering differs'
        if (target if ok and name == _TOOLS[1] else argument['path']).startswith(root):
            project(activity_at + field, target if ok and name == _TOOLS[1] else argument['path'], visible)
        segment = [(at, line) for at, line in rows if previous_end <= at < activity_at]
        locations = [(at, line) for at, line in segment if line.startswith('Location: ')]
        assert len(locations) == int(consent or name == _TOOLS[0] or not ok), 'workspace terminal consent field count differs'
        for at, line in locations:
            assert line == 'Location: ' + json.dumps(target, ensure_ascii=False), 'workspace terminal consent target differs'
            project(at + len('Location: "'), target)
            used_locations.add(at)
        notices = [(at, line) for at, line in segment if line.startswith(_NOTICE)]
        if ok and name == _TOOLS[1]:
            assert len(notices) == 1 and notices[0][1] == _NOTICE + target + _NOTICE_END, 'workspace terminal switch notice differs'
            project(notices[0][0] + len(_NOTICE), target)
            used_notices.add(notices[0][0])
        else:
            # The launch notice is already owned by the startup helper.
            assert all(line == _NOTICE + root + '/workspace' + _NOTICE_END and at < activities[0][0] for at, line in notices), 'workspace terminal unexpected notice'
        previous_end = activity_at + len(activity_line)
    assert {at for at, line in rows if line.startswith('Location: ')} == used_locations, 'workspace terminal unowned consent field'
    assert all(at in used_notices or (at < activities[0][0] and line == _NOTICE + root + '/workspace' + _NOTICE_END)
               for at, line in rows if line.startswith(_NOTICE)), 'workspace terminal unowned switch notice'
    return result

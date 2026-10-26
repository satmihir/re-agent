"""Certify complete PTY replay, then project only independently owned cells.

integrate returns (artifacts, semantic_raw_names, stderr_projections). Ledger
rows are appended through Normalizer.record; raw offsets never enter artifacts.
Redraw cadence is semantic, but every captured cell/style/cursor remains exact.
"""

from datetime import datetime, timezone
import json
from pathlib import Path
import re
import sys

from parity.vt import Terminal


def encoded(value):
    """Return canonical UTF-8 JSON for screen observations."""
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode() + b"\n"


def replay(raw, metadata, *, marks=None):
    """Certify snapshots from the full raw stream and explicit resize/input events."""
    if isinstance(metadata, bytes):
        metadata = json.loads(metadata)
    if not isinstance(raw, bytes) or not isinstance(metadata, dict) or set(metadata) != {"version", "initial_size", "events", "snapshots"} or type(metadata["version"]) is not int or metadata["version"] != 1:
        raise AssertionError("invalid terminal capture contract")
    size = metadata["initial_size"]
    if not isinstance(size, list) or len(size) != 2:
        raise AssertionError("invalid terminal initial size")
    terminal = _Projection(*size, marks=marks or {})
    fed = 0

    def feed(stop):
        nonlocal fed
        while terminal.mark_index < len(terminal.positions) and terminal.positions[terminal.mark_index] < stop:
            position = terminal.positions[terminal.mark_index]
            terminal.feed(raw[fed:position])
            terminal.replacement = terminal.marks[position]
            first = raw[position]
            width = 1 if first < 128 else 2 if first < 224 else 3 if first < 240 else 4
            assert position + width <= stop, 'terminal slot splits a captured boundary'
            terminal.feed(raw[position:position + width])
            terminal.replacement = None
            terminal.mark_index += 1
            fed = position + width
        terminal.feed(raw[fed:stop])
        fed = stop
    shots = metadata["snapshots"]
    if not isinstance(shots, list) or not isinstance(metadata["events"], list):
        raise AssertionError("invalid terminal event or snapshot list")
    by_label = {}
    for shot in shots:
        if not isinstance(shot, dict) or set(shot) != {"label", "offset", "state"} or type(shot["offset"]) is not int or not isinstance(shot["label"], str) or not shot["label"] or shot["label"] in by_label:
            raise AssertionError("invalid or duplicate terminal snapshot")
        by_label[shot["label"]] = shot
    offset = 0
    observed = []
    projected = []
    for event in metadata["events"]:
        if not isinstance(event, dict) or type(event.get("offset")) is not int or not offset <= event["offset"] <= len(raw):
            raise AssertionError("terminal event offsets must be ordered and in bounds")
        feed(event["offset"])
        offset = event["offset"]
        kind = event.get("kind")
        keys = {"resize": {"kind", "offset", "rows", "cols"}, "snapshot": {"kind", "offset", "label"},
                "input": {"kind", "offset", "hex"}, "reply": {"kind", "offset", "hex"}}
        if kind not in keys or set(event) != keys[kind]:
            raise AssertionError("unknown terminal event or fields")
        if kind == "resize":
            terminal.resize(event["rows"], event["cols"])
        elif kind in ("input", "reply"):
            if not isinstance(event["hex"], str):
                raise AssertionError("invalid terminal input encoding")
            data = bytes.fromhex(event["hex"])
            if data.hex() != event["hex"]:
                raise AssertionError("noncanonical terminal input encoding")
            if kind == "reply" and (not terminal.replies or terminal.replies.pop(0) != data):
                raise AssertionError("terminal query reply does not replay")
        else:
            label = event["label"]
            if not isinstance(label, str) or label not in by_label or label in observed:
                raise AssertionError("unmatched terminal snapshot event")
            shot = by_label[label]
            if terminal._partial or terminal._decoder.getstate()[0] or terminal.modes[2026]:
                raise AssertionError("terminal snapshot splits an incomplete update")
            if shot["offset"] != offset or encoded(shot["state"]) != encoded(terminal.snapshot()):
                raise AssertionError("terminal snapshot does not replay exactly: " + label)
            observed.append(label)
            projected.append({"label": label, "state": terminal.project()})
    feed(len(raw))
    terminal.complete()
    if terminal.replies or terminal.modes[2026] or observed != list(by_label):
        raise AssertionError("terminal capture omits query replies or snapshot boundaries")
    return {"initial_size": size, "snapshots": projected, "final": terminal.project()}


def observations(ctx):
    """Return exact replay-certified screen artifacts from a live or replay Context."""
    result = {}
    for name in sorted(ctx.observations):
        if not name.endswith(".terminal.json"):
            continue
        raw_name = name.removesuffix(".terminal.json") + ".tty"
        if raw_name not in ctx.observations:
            raise AssertionError("terminal capture is missing raw PTY bytes")
        result[name.removesuffix(".terminal.json") + ".screens.json"] = encoded(replay(ctx.observations[raw_name], ctx.observations[name]))
    return result


class _Projection(Terminal):
    """Keep cell provenance without changing the bytes the emulator interprets."""

    def __init__(self, rows, cols, marks):
        super().__init__(rows, cols)
        self.marks = marks
        self.positions = sorted(marks)
        self.mark_index = 0
        self.replacement = None
        self.owned = {}

    def _put(self, ch):
        super()._put(ch)
        if self.replacement is not None:
            assert len(ch) == 1 and (32 <= ord(ch) < 127 or ch in _SPINNER), 'terminal slot must occupy one supported cell'
            col = self.col if self.pending_wrap else self.col - 1
            cell = self.grid[self.row][col]
            # Retaining the object prevents id reuse after a redraw erases it.
            self.owned[id(cell)] = (cell, self.replacement)

    def project(self):
        state = self.snapshot()
        for key, lines in (('grid', self.grid), ('scrollback', self.scrollback)):
            for row, line in enumerate(lines):
                for col, cell in enumerate(line):
                    owned = self.owned.get(id(cell))
                    if owned is not None:
                        state[key][row][col]['text'] = owned[1]
        return state


def _printed(raw):
    """Map literal startup characters through SGR only, not cursor controls."""
    text, origins = [], []
    offset = 0
    for token in re.finditer(rb'\x1b\[[0-9;]*m|[\s\S]', raw):
        if token[0].startswith(b'\x1b['):
            continue
        if token.start() < offset:
            continue
        start = token.start()
        first = raw[start]
        width = 1 if first < 128 else 2 if first < 224 else 3 if first < 240 else 4
        stop = start + width
        ch = raw[start:stop].decode('utf-8')
        offset = stop
        if ch != '\r':
            text.append(ch)
            origins.append(start)
    return ''.join(text), origins


_SPINNER = '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏'


_FLASK = ('      ●      ', '  ▗▄▖   ▗▄▖  ', '   █  •  █   ', '   █ •   █   ',
          '  ▟▘     ▝▙  ', ' ▟▘       ▝▙ ', '▐▌    ❯    ▐▌', '▝▙▄▄▄▄▄▄▄▄▄▟▘')


def _linux_cancelled_restore(ctx, invocation, prefix, raw, metadata, text, origins, owned_traces, headers, owner, announcements):
    """Certify the owned cancellation insertion and its restored empty-prompt caret."""
    restores = list(re.finditer(r'(?m)(?:^|\x1b\[(?:[0-9;]*H|\?2026h))trace: ([^\n\x1b]+)(?=\x1b(?!\[r))', text))
    if not restores:
        return []
    assert len(restores) + len(announcements) == 1, 'duplicate cancellation trace announcement'
    match = restores[0]
    assert match[1] == owner['data']['trace_path'], 'cancellation trace announcement differs from cancelled owner'
    run = owner.get('run_id', '')
    assert re.fullmatch(r'[0-9a-f]{16}', run), 'cancellation trace Run identity differs'
    relative = Path(prefix + '.traces') / run / 'events.jsonl'
    artifact_root = getattr(ctx, 'artifact_root', ctx.artifacts)
    assert match[1] == str(artifact_root / relative), 'cancellation trace path differs from Run allocation'
    starts = [event for event in headers if event.get('run_id') == run]
    assert len(starts) == 1, 'cancellation trace lacks unique run.started owner'
    start = starts[0]
    assert start.get('session_id') == owner.get('session_id') and start.get('session_id'), 'cancellation trace Session differs'
    for event in (start, owner):
        paths = [Path(path).relative_to(ctx.artifacts) for path, line in owned_traces if json.loads(line) == event]
        assert paths == [relative], 'cancellation trace raw owner differs'
    assert start['data']['workspace'] == str(ctx.root / 'workspace'), 'cancellation trace workspace differs'
    model_cancel = Path(ctx.root).name == 'C44.typeahead-cancellation'
    tools = 0 if model_cancel else 1
    assert type(owner['data']['steps']) is int and owner['data']['steps'] == 1, 'cancellation trace run counts differ'
    assert type(owner['data']['tool_calls']) is int and owner['data']['tool_calls'] == tools, 'cancellation trace run counts differ'
    if model_cancel:
        events = [json.loads(line) for _, line in owned_traces]
        requested = [event for event in events if event['type'] == 'model.requested' and event['run_id'] == run]
        assert len(requested) == 1 and requested[0]['step'] == 1, 'cancelled model request lacks unique owner'
        request = requested[0]
        assert request['session_id'] == start['session_id'] and start['seq'] < request['seq'] < owner['seq'], 'cancelled model request scope differs'
        assert start['data']['prompt'] == 'hold this request', 'cancelled model request prompt differs'
        assert not any(event['type'] == 'tool.started' and event['run_id'] == run for event in events), 'cancelled model request started a tool'
    stamps = [datetime.fromisoformat(event['time'].replace('Z', '+00:00')) for event in (start, owner)]
    assert all(stamp.tzinfo is not None for stamp in stamps), 'cancellation trace owner lacks timezone'
    begin, end = [stamp.timestamp() for stamp in stamps]
    invocations = [item for item in ctx.invocations if item['started'] <= begin <= end <= item['ended']]
    assert len(invocations) == 1 and invocations[0] is invocation and start['seq'] < owner['seq'], 'cancellation trace invocation owner differs'

    position = origins[match.start()]
    sync = b'\x1b[?2026h'
    frame_start = position - len(sync)
    frame = re.match(rb'\x1b\[\?2026h\x1b\[([1-9][0-9]*);1Htrace: ' + re.escape(match[1].encode()) +
                     rb'\x1b\[([1-9][0-9]*);([1-9][0-9]*)H\x1b\[\?2026l', raw[max(0, frame_start):])
    assert frame_start >= 0 and frame, 'cancellation trace lacks exact synchronized restore frame'
    screen = Terminal(*metadata['initial_size'])
    offset = 0
    frame_end = frame_start + frame.end()
    for event in metadata['events']:
        if event['kind'] != 'resize':
            continue
        assert not frame_start < event['offset'] < frame_end, 'cancellation trace frame crosses resize'
        if event['offset'] <= frame_start:
            screen.feed(raw[offset:event['offset']])
            screen.resize(event['rows'], event['cols'])
            offset = event['offset']
    screen.feed(raw[offset:frame_start])
    row, restore_row, restore_col = [int(value) for value in frame.groups()]
    assert not screen.modes[2026] and not screen.pending_wrap, 'cancellation trace starts inside an update'
    assert (restore_row, restore_col) == (screen.row + 1, screen.col + 1), 'cancellation trace restore differs from captured caret'
    expected_input = '❯ stale cancelled input' if model_cancel else '❯'
    expected_col = len(expected_input) if model_cancel else 2
    assert screen.col == expected_col and screen.line(screen.row) == expected_input, 'cancellation trace lacks restored input prompt'
    if model_cancel:
        cleared = [shot for shot in metadata['snapshots'] if shot['label'] == 'typeahead-cancelled']
        assert len(cleared) == 1 and cleared[0]['offset'] >= frame_end, 'cancelled typeahead lacks cleared prompt witness'
        state = cleared[0]['state']
        cursor = state['cursor']
        line = ''.join(cell['text'] for cell in state['grid'][cursor['row']]).rstrip()
        assert line == '❯' and cursor['col'] == 2, 'cancelled typeahead was not discarded'
    needed = (len('trace: ') + len(match[1]) + screen.cols - 1) // screen.cols
    assert 1 <= row and row + needed < restore_row <= screen.rows, 'cancellation trace insertion overlaps restored prompt'
    return restores


def _slots(ctx, normal, invocation, index, raw, metadata, traces):
    """Bind startup and terminal exit fields to this invocation's raw headers."""
    text, origins = _printed(raw)
    lines = text.split('\n')
    cols = metadata['initial_size'][1]
    prefix = 'invoke-%03d' % index
    headers, finished, events, owned_traces, trace_paths = [], [], [], [], set()
    artifact_root = getattr(ctx, 'artifact_root', ctx.artifacts)
    startup_session = None
    hint_row = None
    for path, line in traces:
        relative = Path(path).relative_to(ctx.artifacts)
        if relative.parts[0] != prefix + '.traces':
            continue
        event = json.loads(line)
        events.append(event)
        owned_traces.append((path, line))
        trace_paths.add(str(artifact_root / relative))
        if event.get('type') == 'run.started' and not event.get('agent_id'):
            headers.append(event)
        elif event.get('type') == 'run.finished' and not event.get('agent_id'):
            finished.append(event)
    if len(finished) > 1:
        def utc(event):
            stamp = datetime.fromisoformat(event['time'].replace('Z', '+00:00'))
            assert stamp.tzinfo is not None, 'terminal summary witness lacks a timezone'
            return stamp.astimezone(timezone.utc)
        finished.sort(key=utc)
        assert len({utc(event) for event in finished}) == len(finished), 'ambiguous terminal summary lifecycle order'
    slots = []

    def slot(start, value, replacement, kind):
        assert (value.isascii() and value.isprintable()) or (kind == 'terminal-progress-spinner' and value in _SPINNER and len(value) == 1), 'non-ASCII terminal allocation requires explicit cell-width support'
        assert len(replacement) <= len(value), 'terminal allocation is shorter than its canonical token'
        padded = replacement.ljust(len(value))
        positions = origins[start:start + len(value)]
        assert text[start:start + len(value)] == value and len(positions) == len(value)
        slots.append((positions, value, padded, kind))

    from parity.observe_agents_effects import recap_slots
    for start, value, replacement, kind in recap_slots(ctx, invocation, prefix, text, traces):
        slot(start, value, replacement, kind)

    def session(start, value):
        witnesses = [event for event in headers if event.get('session_id') == value]
        if headers:
            assert witnesses, 'terminal session has no independently owned run.started header'
            assert all(event['data']['workspace'] == str(ctx.root / 'workspace') for event in witnesses), 'terminal session workspace owner differs'
        else:
            assert not finished and value == startup_session, 'terminal session differs from raw startup identity'
            config = invocation.get('terminal', {})
            assert config.get('rows') == metadata['initial_size'][0] and config.get('cols') == cols and type(config.get('stdout_tty')) is bool, 'startup-only session lacks raw driver launch config'
        slot(start, value, normal.identity(value, 'session'), 'terminal-startup-session')

    def authority(start, line, truncated=False):
        endpoint = invocation.get('env', {}).get('API_PROXY_URL')
        server = getattr(ctx, '_server', None)
        assert endpoint and server, 'terminal proxy lacks independently captured endpoint'
        match = re.fullmatch(r'https?://([^/\s?#]+)(?:/[^\s]*)?', endpoint)
        assert match and match[1] == server.url.split('://', 1)[1] == normal.authority, 'terminal proxy escaped allocated authority'
        expected = 'requests go to ' + endpoint + ' (API_PROXY_URL)'
        if truncated and len(expected) > cols - 17:
            expected = expected[:cols - 18] + '…'
        assert line == expected, 'terminal proxy differs from recorded endpoint'
        at = len('requests go to ') + endpoint.index(match[1])
        visible = min(len(match[1]), len(line.rstrip('…')) - at)
        assert visible > 0, 'terminal proxy authority is not visible'
        slot(start + at, match[1][:visible], '<SERVER>'[:visible], 'allocated-fixture-authority')

    def workspace(start, value, truncated=False):
        expected = str(ctx.root / 'workspace')
        if truncated:
            room = cols - 17
            expected = expected if len(expected) <= room else expected[:room - 1] + '…'
        assert value == expected, 'terminal startup workspace differs from invocation config'
        root = str(ctx.root)
        visible = min(len(root), len(value.rstrip('…')))
        slot(start, root[:visible], '<ROOT>'[:visible], 'terminal-startup-path')

    if lines and lines[0].startswith('re:agent chat · '):
        assert len(lines) >= 3, 'incomplete plain terminal startup'
        row = 1
        if lines[0] != 're:agent chat · scripted':
            match = re.fullmatch(r'session ID: ([0-9a-f]{16})', lines[1])
            assert match, 'invalid terminal startup session line'
            startup_session = match[1]
            session(len(lines[0]) + 1 + len('session ID: '), match[1])
            row = 2
        if lines[row].startswith('requests go to '):
            authority(sum(len(line) + 1 for line in lines[:row]), lines[row])
            row += 1
        match = re.fullmatch(r'workspace (.*?) · .* · [^ ]+ steps, [^ ]+ tool calls per turn', lines[row])
        assert match, 'invalid terminal startup workspace line'
        workspace(sum(len(line) + 1 for line in lines[:row]) + len('workspace '), match[1])
        for row in range(row + 1, min(len(lines), 8)):
            if lines[row] == '/help for commands · Ctrl-D to exit':
                hint_row = row + 1
            notice = re.fullmatch(r'! exec mode: commands run as you, in (.*?), and can read, write, and use the network', lines[row])
            if notice:
                workspace(sum(len(line) + 1 for line in lines[:row]) + notice.start(1), notice[1])
    elif lines and lines[0].startswith(_FLASK[0]):
        assert len(lines) >= 8 and all(lines[row].startswith(art) for row, art in enumerate(_FLASK)), 'invalid styled startup flask'
        match = re.fullmatch(re.escape(_FLASK[0]) + r'   session ID: ([0-9a-f]{16})', lines[0])
        if match:
            startup_session = match[1]
            session(match.start(1), match[1])
        else:
            assert lines[0] == _FLASK[0] and '--scripted' in invocation.get('args', []), 'invalid styled startup session line'
        assert lines[3].startswith(_FLASK[3] + '   '), 'invalid styled workspace prefix'
        workspace(sum(len(line) + 1 for line in lines[:3]) + 16, lines[3][16:], truncated=True)
        if lines[5].startswith(_FLASK[5] + '   requests go to '):
            authority(sum(len(line) + 1 for line in lines[:5]) + 16, lines[5][16:], truncated=True)
        hint_row = 8

    if hint_row is not None and hint_row < len(lines) and lines[hint_row].startswith('resume: '):
        hint = re.fullmatch(r'resume: reagent chat --resume ([0-9a-f]{16})', lines[hint_row])
        assert hint and hint[1] == startup_session, 'terminal startup resume hint differs from session identity'
        session(sum(len(line) + 1 for line in lines[:hint_row]) + hint.start(1), hint[1])

    fresh_footer = None
    if Path(ctx.root).name == 'C16.implement-fresh':
        from parity.observe_cli_sessions import fresh_implementation
        fresh_footer = fresh_implementation(ctx, invocation, metadata, headers, text, origins)
    # A fresh selection can announce the next Session before the final exit hint.
    for footer in re.finditer(r'session ID: ([0-9a-f]{16}) · resume: reagent chat --resume ([0-9a-f]{16})(?=\n|\x1b|$)', text):
        ending = re.fullmatch(r'(?:\n|\x1b\[\?[0-9]+[hl])*', text[footer.end():])
        if ending is None:
            if fresh_footer is not None and footer.span() == fresh_footer.span():
                pass  # The traced picker transition owns this midstream footer.
            elif Path(ctx.root).name == 'T26.lifetime':
                assert invocation['terminal']['stdout_tty'] is False and b'fresh\r' in invocation['stdin'], 'mid-session identity lacks the configured fresh transition'
            else:
                continue
        assert footer[1] == footer[2] and (headers or footer[1] == startup_session), 'terminal resume hint differs from startup identity'
        session(footer.start(1), footer[1])
        session(footer.start(2), footer[2])
    paths = list(re.finditer(r'(?m)^(?:trace: |last trace )([^\n]+)$', text))
    if Path(ctx.root).name == 'C38.interrupt-running-command':
        blocked = list(re.finditer(r'(?m)(?:^|\x1b\[[1-9][0-9]*;[1-9][0-9]*H)error: the last run ended with cancelled; inspect the trace at ([^\n\x1b]+) and use /compact or /reset to continue(?=$|\x1b\[r)', text))
        assert len(blocked) == 1, 'cancelled exec lacks its exact blocked-next-turn diagnostic'
        owners = [event for event in finished if event['data'].get('trace_path') == blocked[0][1]]
        assert len(owners) == 1 and owners[0]['data']['status'] == 'cancelled', 'blocked diagnostic lacks its cancelled run.finished owner'
        assert owners[0]['data']['steps'] == owners[0]['data']['tool_calls'] == 1, 'blocked diagnostic run counts differ'
        announcements = list(re.finditer(r'(?m)(?:^|\x1b\[[1-9][0-9]*;[1-9][0-9]*H)trace: ([^\n\x1b]+)(?=$|\x1b\[r)', text))
        assert len(announcements) <= 1, 'duplicate cancellation trace announcement'
        assert all(match[1] == blocked[0][1] for match in announcements), 'cancellation trace announcement differs from cancelled owner'
        if sys.platform == 'linux':
            announcements += _linux_cancelled_restore(ctx, invocation, prefix, raw, metadata, text, origins,
                                                      owned_traces, headers, owners[0], announcements)
        paths = list({match.start(1): match for match in paths + announcements + blocked
                      if '\x1b' not in match[1]}.values())
    if Path(ctx.root).name == 'C44.typeahead-cancellation':
        owners = [event for event in finished if event['data'].get('status') == 'cancelled']
        assert len(owners) == 1, 'typeahead cancellation lacks unique cancelled run'
        assert owners[0]['data']['steps'] == 1 and owners[0]['data']['tool_calls'] == 0, 'typeahead cancellation run counts differ'
        announcements = list(re.finditer(r'(?m)(?:^|\x1b\[[1-9][0-9]*;[1-9][0-9]*H)trace: ([^\n\x1b]+)(?=$|\x1b\[r)', text))
        assert all(match[1] == owners[0]['data']['trace_path'] for match in announcements), 'typeahead cancellation trace differs'
        if sys.platform == 'linux':
            announcements += _linux_cancelled_restore(ctx, invocation, prefix, raw, metadata, text, origins,
                                                      owned_traces, headers, owners[0], announcements)
        assert len(announcements) == 1, 'typeahead cancellation lacks one trace announcement'
        paths = list({match.start(1): match for match in paths + announcements if '\x1b' not in match[1]}.values())
    for match in paths:
        value = match[1]
        if value == 'not recorded':
            continue
        assert value in trace_paths, 'terminal artifact path has no independently owned raw trace'
        relative = Path(value).relative_to(artifact_root)
        assert len(relative.parts) == 3 and relative.parts[0] == prefix + '.traces' and relative.parts[2] == 'events.jsonl', 'terminal trace path shape differs'
        replacement = '<ARTIFACTS>/' + relative.parts[0] + '/' + normal.identity(relative.parts[1], 'run') + '/events.jsonl'
        slot(match.start(1), value, replacement, 'terminal-artifact-path')

    if Path(ctx.root).name == 'C26.exec-timeout-continues':
        timeouts = list(re.finditer(r'(?m)(?:^|\x1b\[[1-9][0-9]*;[1-9][0-9]*H)  ! exec (?P<command>[^\n\x1b]+) → timed out after (?P<elapsed>[0-9]+\.[0-9])s; effects unknown(?=$|\x1b\[(?:r|[1-9][0-9]*;[1-9][0-9]*H))', text))
        assert len(timeouts) == 1, 'terminal exec timeout lacks one exact activity'
        from parity.observe_derived import Derived
        derived = Derived(normal, owned_traces, [], invocations=ctx.invocations)
        match = timeouts[0]
        configured, duration, _ = derived.exec_timeout_duration(match['command'], invocation)
        seconds = duration // 1000 + (duration % 1000 * 1000000) / 1e9
        whole, tenth = match['elapsed'].split('.')
        assert match['elapsed'] == f'{seconds:.1f}' and configured <= int(whole) * 1000 + int(tenth) * 100 <= configured + 2000, 'terminal exec timeout differs from traced duration'
        slot(match.start('elapsed'), match['elapsed'], '0.0', 'terminal-exec-timeout-elapsed')

    if Path(ctx.root).name in ('C34.typeahead-queue-long-reply', 'C35.resize-during-turn'):
        progress = list(re.finditer(r'(?m)(?:^|\n|\x1b\[[1-9][0-9]*;1H)  (?P<glyph>[' + _SPINNER + r']) waiting for (?P<model>[a-zA-Z0-9.-]+) · API key · step (?P<step>[1-9][0-9]*)(?: of (?P<limit>[1-9][0-9]*))?(?=$|\n|\x1b)', text))
        labels = {'queued-before-reply'}
        if Path(ctx.root).name == 'C35.resize-during-turn':
            labels.add('resized-retained-input')
        shots = [shot for shot in metadata['snapshots'] if shot['label'] in labels]
        assert {shot['label'] for shot in shots} == labels and len(shots) == len(labels), 'terminal progress lacks owned snapshot boundaries'
        marked = set()
        for shot in shots:
            waiting = [row for row in shot['state']['grid'] if re.fullmatch(
                r'  [' + _SPINNER + r'] waiting for [a-zA-Z0-9.-]+ · API key · step [1-9][0-9]*(?: of [1-9][0-9]*)? *',
                ''.join(cell['text'] for cell in row))]
            if not waiting:
                continue
            candidates = [match for match in progress if origins[match.start('glyph')] < shot['offset']]
            assert len(waiting) == 1 and candidates, 'terminal progress frame lacks its exact printed owner'
            match = candidates[-1]
            held = [header for header in headers if header['data'].get('prompt') == 'start']
            assert len(held) == 1, 'terminal progress lacks held-turn header'
            requests = [event for event in events if event.get('type') == 'model.requested'
                        and event.get('run_id') == held[0]['run_id'] and event.get('step') == int(match['step'])
                        and event['data'].get('model') == match['model']]
            assert len(requests) == 1, 'terminal progress lacks unique model request'
            request = requests[0]
            owners = [header for header in headers if (header.get('session_id'), header.get('run_id')) == (request.get('session_id'), request.get('run_id'))]
            assert len(owners) == 1, 'terminal progress lacks run.started owner'
            header = owners[0]
            for event in (header, request):
                paths = [Path(path).relative_to(ctx.artifacts).parts for path, line in owned_traces if json.loads(line) == event]
                assert paths == [(prefix + '.traces', event['run_id'], 'events.jsonl')], 'terminal progress raw trace owner differs'
            assert request['data']['scope'] == {key: request[key] for key in ('session_id', 'run_id', 'step')}, 'terminal progress request scope differs'
            assert header['data']['configured'] == match['model'] and header['data']['auth_mode'] == 'key', 'terminal progress model/auth owner differs'
            assert int(match['limit'] or 0) == header['data']['max_steps'], 'terminal progress step limit differs'
            begin, end = [datetime.fromisoformat(event['time'].replace('Z', '+00:00')).timestamp() for event in (header, request)]
            owners = [item for item in ctx.invocations if item['started'] <= begin <= end <= item['ended']]
            assert len(owners) == 1 and owners[0] is invocation and header['seq'] < request['seq'], 'terminal progress invocation owner differs'
            position = origins[match.start('glyph')]
            screen = Terminal(*metadata['initial_size'])
            offset = 0
            for event in metadata['events']:
                if event['kind'] == 'resize' and event['offset'] <= position:
                    screen.feed(raw[offset:event['offset']])
                    screen.resize(event['rows'], event['cols'])
                    offset = event['offset']
            screen.feed(raw[offset:position])
            row = screen.row
            assert screen.col == 2 and row >= 3, 'terminal spinner is not at progress cell'
            assert screen.line(row - 2) == '─' * screen.cols and screen.line(row - 3).startswith('❯'), 'terminal spinner lacks prompt frame'
            assert screen.line(row - 1).strip().split(' · ')[0] == match['model'] + ' / low', 'terminal spinner status model differs'
            state = shot['state']
            assert ''.join(cell['text'] for cell in state['grid'][row]).startswith('  ' + match['glyph'] + ' waiting for ' + match['model'] + ' · API key · step ' + match['step']), 'terminal spinner does not survive at queued progress cell'
            if position not in marked:
                slot(match.start('glyph'), match['glyph'], '⠋', 'terminal-progress-spinner')
                marked.add(position)

    # Reuse the existing footer-elapsed rule, additionally binding the literal
    # status and counts to ordered run.finished records in this invocation.
    summaries = list(re.finditer(r'(?m)(?:^|\x1b\[[1-9][0-9]*;[1-9][0-9]*H)[✓✗!–] (?P<status>[a-z_]+) · (?P<steps>\d+) steps? · (?P<tools>\d+) tool calls? · [^\n\x1b]+ · (?P<elapsed>\d+\.\ds|\d+m[0-5]\ds)(?=$|\x1b\[(?:r|[1-9][0-9]*;[1-9][0-9]*H))', text))
    for match, event in zip(summaries, finished):
        result = event['data']
        assert (match['status'], int(match['steps']), int(match['tools'])) == (result['status'], result['steps'], result['tool_calls']), 'terminal summary differs from owned run.finished'
        value = match['elapsed']
        seconds = float(value[:-1]) if 'm' not in value else int(value.split('m')[0]) * 60 + int(value.split('m')[1][:-1])
        from parity.invocation_time import elapsed
        assert 0 <= seconds <= elapsed(invocation) + 2, 'terminal elapsed outside invocation bounds'
        slot(match.start('elapsed'), value, '0.0s', 'footer-elapsed')
    assert not summaries or len(summaries) == len(finished), 'terminal summaries lack one-to-one run.finished witnesses'
    from parity.observe_cli_workspace_terminal import slots as workspace_slots
    for start, value, replacement, kind in workspace_slots(ctx, invocation, prefix, text, traces, cols):
        slot(start, value, replacement, kind)
    races = getattr(normal, 'races', None)
    if races is not None and races.record is not None:
        assert Path(ctx.root).name == 'C37.interrupt-shell-command'
        assert text.count(races.footer) == 1, 'shell footer lacks its exact process/trace witness'
        value = races.footer.split(' after ', 1)[1].split(' · ', 1)[0]
        start = text.index(races.footer) + races.footer.index(value)
        slot(start, value, '0.0s', 'cli-shell-elapsed')
    if slots:
        args = invocation.get('args')
        assert args and args[0] == 'chat', 'terminal startup lacks independently captured invocation args'
        at = args.index('--workspace') if '--workspace' in args else -1
        assert at >= 0 and args[at + 1] == str(ctx.root / 'workspace'), 'terminal startup workspace flag owner differs'
        assert invocation.get('trace', ctx.artifacts / (prefix + '.traces')) == ctx.artifacts / (prefix + '.traces'), 'terminal trace allocation owner differs'
    return slots


def integrate(ctx, normal, traces):
    """Return exact screen artifacts, replay-accounted raw names and stderr views."""
    artifacts, semantic, stderr = {}, set(), {}
    observations = ctx.observations
    terminal_names = {name for name in observations if name.endswith('.terminal.json')}
    tty_names = {name for name in observations if name.endswith('.tty')}
    assert tty_names == {name.removesuffix('.terminal.json') + '.tty' for name in terminal_names}, 'unpaired terminal raw evidence'
    for name in sorted(terminal_names):
        prefix = name.removesuffix('.terminal.json')
        match = re.fullmatch(r'invoke-([0-9]{3})', prefix)
        assert match and 1 <= int(match[1]) <= len(ctx.invocations), 'terminal capture has no invocation owner'
        index = int(match[1])
        invocation = ctx.invocations[index - 1]
        raw_name = prefix + '.tty'
        raw = observations[raw_name]
        assert raw == invocation['stderr'], 'terminal raw bytes differ from invocation stderr'
        metadata = json.loads(observations[name])
        exact = replay(raw, metadata)
        inputs = b''.join(bytes.fromhex(event['hex']) for event in metadata['events'] if event['kind'] == 'input')
        assert invocation.get('stdin') == inputs, 'terminal input events differ from captured invocation stdin'
        if 'terminal' in invocation:
            assert metadata['initial_size'] == [invocation['terminal']['rows'], invocation['terminal']['cols']], 'terminal size differs from invocation config'
        slots = _slots(ctx, normal, invocation, index, raw, metadata, traces)
        marks = {}
        screen_name = prefix + '.screens.json'
        for number, (positions, value, replacement, kind) in enumerate(slots):
            for position, ch in zip(positions, replacement):
                assert position not in marks, 'overlapping terminal cell slots'
                marks[position] = ch
            normal.record(screen_name, ('terminal-cell-slot', number), value.encode(), replacement.encode(), kind)
        state = replay(raw, metadata, marks=marks) if marks else exact
        state['events'] = [{key: value for key, value in event.items() if key != 'offset'} for event in metadata['events']]
        artifacts[screen_name] = encoded(state)
        stderr_name = 'invocation-%d.stderr' % index
        stderr[stderr_name] = encoded({'terminal_screens': screen_name})
        for raw_artifact, before in ((raw_name, raw), (name, observations[name]), (stderr_name, raw)):
            normal.record(raw_artifact, ('terminal-state-projection',), before,
                          artifacts[screen_name] if raw_artifact != stderr_name else stderr[stderr_name],
                          'terminal-state-projection')
            normal.ledger[-1].update(classification='S', certificate=screen_name)
        semantic.update((raw_name, name))
    return artifacts, semantic, stderr

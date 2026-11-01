"""Lossless byte oracle. Parsing discovers spans; it never rewrites untouched JSON."""
import base64
import datetime as dt
import difflib
import hashlib
import json
import re
import uuid
from dataclasses import dataclass
from pathlib import Path


def sha256(raw):
    return hashlib.sha256(raw).hexdigest()


def go_string(value):
    return (json.dumps(value, ensure_ascii=False, separators=(',', ':'))
            .replace('<', '\\u003c').replace('>', '\\u003e').replace('&', '\\u0026')
            .replace('\u2028', '\\u2028').replace('\u2029', '\\u2029')).encode('utf-8')


@dataclass
class Token:
    start: int
    end: int
    value: object
    children: list


def lex(raw, opaque=False):
    """Return byte spans; only declared opaque wire/native data may be non-UTF-8."""
    errors = 'surrogateescape' if opaque else 'strict'
    raw.decode('utf-8', errors=errors)

    def load(value):
        return json.loads(value.decode('utf-8', errors=errors),
                          parse_constant=lambda _: (_ for _ in ()).throw(ValueError('nonfinite number')))
    pos = 0
    string_boundary = re.compile(rb'["\\\\\x00-\x1f]')

    def whitespace():
        nonlocal pos
        while pos < len(raw) and raw[pos] in b' \t\n\r':
            pos += 1

    def parse():
        nonlocal pos
        whitespace()
        start = pos
        if pos >= len(raw):
            raise ValueError('incomplete JSON')
        first = raw[pos]
        children = []
        if first == 34:
            pos += 1
            while True:
                boundary = string_boundary.search(raw, pos)
                if boundary is None:
                    raise ValueError('unclosed JSON string')
                pos = boundary.start()
                if raw[pos] == 92:
                    pos += 2
                    continue
                if raw[pos] != 34:
                    raise ValueError('control byte in JSON string')
                pos += 1
                break
            value = load(raw[start:pos])
        elif first in (123, 91):
            obj = first == 123
            close = 125 if obj else 93
            pos += 1
            whitespace()
            if pos < len(raw) and raw[pos] != close:
                while True:
                    key = parse() if obj else None
                    if obj:
                        if not isinstance(key.value, str):
                            raise ValueError('object key is not a string')
                        whitespace()
                        if pos >= len(raw) or raw[pos] != 58:
                            raise ValueError('missing colon')
                        pos += 1
                    item = parse()
                    children.append((key.value if obj else len(children), item))
                    whitespace()
                    if pos < len(raw) and raw[pos] == 44:
                        pos += 1
                        continue
                    break
            if pos >= len(raw) or raw[pos] != close:
                raise ValueError('unclosed JSON container')
            pos += 1
            value = load(raw[start:pos])
        else:
            while pos < len(raw) and raw[pos] not in b' \t\r\n,]}':
                pos += 1
            value = load(raw[start:pos])
        return Token(start, pos, value, children)

    result = parse()
    whitespace()
    if pos != len(raw):
        raise ValueError('trailing JSON')
    return result


def child(token, key):
    found = [v for k, v in token.children if k == key]
    if len(found) != 1:
        raise AssertionError(f'expected exactly one {key!r}, found {len(found)}')
    return found[0]


def replace_spans(raw, changes):
    end = 0
    parts = []
    for start, stop, replacement in sorted(changes):
        if start < end or stop < start:
            raise AssertionError('overlapping substitution spans')
        parts.extend((raw[end:start], replacement))
        end = stop
    parts.append(raw[end:])
    return b''.join(parts)


def encoded_changes(raw, changes):
    """Lift decoded UTF-8 byte spans through one JSON-string escaping layer."""
    needed = sorted({point for start, end, _ in changes for point in (start, end)})
    offsets = {}
    pos, decoded, index = 1, 0, 0
    while index < len(needed):
        if needed[index] == decoded:
            offsets[needed[index]] = pos
            index += 1
            continue
        if pos >= len(raw)-1:
            raise AssertionError('decoded substitution outside JSON string')
        if raw[pos] != 92:
            stop = raw.find(b'\\', pos, len(raw)-1)
            if stop < 0:
                stop = len(raw)-1
            length = stop-pos
            while index < len(needed) and needed[index] <= decoded+length:
                offsets[needed[index]] = pos+needed[index]-decoded
                index += 1
            pos, decoded = stop, decoded+length
            continue
        stop = pos + 2
        length = 1
        if raw[pos+1] == 117:
            stop = pos + 6
            scalar = int(raw[pos+2:stop], 16)
            if 0xD800 <= scalar <= 0xDBFF and raw[stop:stop+2] == b'\\u':
                low = int(raw[stop+2:stop+6], 16)
                if 0xDC00 <= low <= 0xDFFF:
                    scalar = 0x10000 + ((scalar-0xD800) << 10) + low-0xDC00
                    stop += 6
            length = 1 if scalar < 0x80 else 2 if scalar < 0x800 else 3 if scalar < 0x10000 else 4
        decoded += length
        pos = stop
    return replace_spans(raw, [(offsets[start], offsets[end], go_string(value.decode('utf-8'))[1:-1])
                               for start, end, value in changes])


def string_spans(raw, before, after):
    """Preserve untouched lexemes, including inside large embedded JSON bodies."""
    if before == after:
        return raw
    old, new = before.encode('utf-8'), after.encode('utf-8')
    offset = 0
    from .observe_cli_commands import PREAMBLE
    if before.startswith(PREAMBLE) and after.startswith(PREAMBLE):
        offset = len(PREAMBLE.encode())
    handoff = 'Historical conversation supplied by re:agent, encoded as JSON records below.'
    if before.startswith(handoff) and after.startswith(handoff):
        prefix, _ = before.rsplit('\n', 1)
        new_prefix, _ = after.rsplit('\n', 1)
        if prefix != new_prefix:
            raise AssertionError('normalization changed handoff preamble')
        offset = len((prefix+'\n').encode())
    if before.lstrip().startswith(('{', '[')) or offset:
        try:
            left, right = lex(old[offset:]), lex(new[offset:])
        except (ValueError, UnicodeError):
            left = right = None
        if left is not None:
            patches = []

            def walk(a, b):
                if old[offset+a.start:offset+a.end] == new[offset+b.start:offset+b.end]:
                    return
                if a.children or b.children:
                    if [key for key, _ in a.children] != [key for key, _ in b.children]:
                        raise AssertionError('normalization changed embedded JSON shape/order')
                    for (_, x), (_, y) in zip(a.children, b.children):
                        walk(x, y)
                    return
                patches.append((offset+a.start, offset+a.end, new[offset+b.start:offset+b.end]))

            walk(left, right)
            return encoded_changes(raw, patches)
    # Small non-JSON instruction/diagnostic strings use character matching;
    # trim the unchanged prefix/suffix before matching repeated text.
    start = 0
    while start < min(len(before), len(after)) and before[start] == after[start]:
        start += 1
    end = 0
    while end < min(len(before), len(after))-start and before[-end-1] == after[-end-1]:
        end += 1
    stop_a, stop_b = len(before)-end, len(after)-end
    changes = []
    for tag, a, b, c, d in difflib.SequenceMatcher(None, before[start:stop_a], after[start:stop_b], autojunk=False).get_opcodes():
        if tag != 'equal':
            changes.append((len(before[:start+a].encode()), len(before[:start+b].encode()),
                            after[start+c:start+d].encode()))
    return encoded_changes(raw, changes)


def compare(expected, actual):
    """Compare complete artifacts, including order, whitespace and missing events."""
    if expected.keys() != actual.keys():
        raise AssertionError(f'artifact set differs: missing={expected.keys()-actual.keys()} extra={actual.keys()-expected.keys()}')
    for name in expected:
        left, right = expected[name], actual[name]
        if left != right:
            offset = next((i for i, (a, b) in enumerate(zip(left, right)) if a != b), min(len(left), len(right)))
            raise AssertionError(f'{name}: byte {offset}, expected {left[max(0,offset-60):offset+160]!r}, got {right[max(0,offset-60):offset+160]!r}; lengths {len(left)}/{len(right)}')


def check_digest(raw, expected):
    if not re.fullmatch('[0-9a-f]{64}', expected) or sha256(raw) != expected:
        raise AssertionError('SHA-256 does not match independently observed bytes')


def cache_key(request, schemas=None):
    """Compute the prefix from independently frozen original schema bytes."""
    if schemas is None:
        schemas = json.loads(Path(__file__).with_name('fixtures').joinpath('cache_schemas.json').read_text())['schemas']
    raw = request if isinstance(request, bytes) else go_json(request)
    top = lex(raw)
    h = hashlib.sha256()
    for key in ('model', 'instructions'):
        h.update(child(top, key).value.encode() + b'\0')
    for _, tool in child(top, 'tools').children:
        name = child(tool, 'name').value
        h.update(name.encode() + b'\0')
        schema = child(tool, 'input_schema')
        matches = []
        for fixture in schemas.get(name, []):
            original = fixture['raw'].encode()
            check_digest(original, fixture['sha256'])
            if go_json(json.loads(original)) == raw[schema.start:schema.end]:
                matches.append(original)
        if len(matches) != 1:
            raise AssertionError(f'{name}: schema does not uniquely match frozen raw hash input')
        h.update(matches[0] + b'\0')
    history = child(top, 'history')
    if history.children:
        first = history.children[0][1]
        if first.value.get('user') is not None:
            user = child(first, 'user')
            # Preserve original JSON string escapes: invalid ordinary-string
            # bytes have already become one \\ufffd escape per byte here.
            fields = [go_string(key) + b':' + raw[value.start:value.end]
                      for key, value in user.children if key != 'workspace']
            user_bytes = b'{' + b','.join(fields) + b'}'
            fields = [go_string(key) + b':' + (user_bytes if key == 'user' else raw[value.start:value.end])
                      for key, value in first.children]
            h.update(b'{' + b','.join(fields) + b'}')
        else:
            h.update(raw[first.start:first.end])
    return h.hexdigest()[:32]


def go_json(value):
    # dict insertion order comes from the wire; no recursive sort is performed.
    if isinstance(value, str):
        return go_string(value)
    if isinstance(value, dict):
        return b'{' + b','.join(go_string(k)+b':'+go_json(v) for k, v in value.items()) + b'}'
    if isinstance(value, list):
        return b'[' + b','.join(go_json(v) for v in value) + b']'
    return json.dumps(value, separators=(',', ':'), allow_nan=False).encode()


def preview_prefix(raw):
    """Reconstruct a fresh CLI preview's cache input, excluding its snapshot."""
    top = lex(raw)
    if not isinstance(top.value, dict) or 'prompt_cache_key' not in top.value:
        return None
    first = child(top, 'input').children[0][1]
    if first.value.get('role') != 'user':
        raise AssertionError('fresh preview does not begin with a user Entry')
    parts = child(first, 'content').children
    text = child(parts[0][1], 'text')
    user = b'{"text":' + raw[text.start:text.end]
    roster = None
    plan = False
    for _, part in parts[1:]:
        value = child(part, 'text').value
        marker = 'Agent roster when this message was sent, collected by re:agent:\n'
        if value.startswith(marker):
            roster = value[len(marker):].encode()
            lex(roster)
        elif value.startswith('re:agent plan mode is on'):
            plan = True
        elif not value.startswith('Workspace state when this message was sent, collected by re:agent:\n'):
            raise AssertionError('preview contains an unaccounted cache-prefix component')
    if roster is not None:
        user += b',"roster":' + roster
    if plan:
        user += b',"plan":"on"'
    user += b'}'
    tools = []
    for _, tool in child(top, 'tools').children:
        name, schema = child(tool, 'name'), child(tool, 'parameters')
        tools.append(b'{"name":' + raw[name.start:name.end] + b',"input_schema":' + raw[schema.start:schema.end] + b'}')
    model, instructions = child(top, 'model'), child(top, 'instructions')
    return (b'{"model":' + raw[model.start:model.end] + b',"instructions":' + raw[instructions.start:instructions.end]
            + b',"tools":[' + b','.join(tools) + b'],"history":[{"kind":"user","user":' + user + b'}]}')


class Normalizer:
    def __init__(self, root, authority, started=0, ended=float('inf'), artifacts=None):
        self.root = str(root)
        self.artifacts = str(artifacts) if artifacts is not None else None
        self.authority = authority
        self.started, self.ended = started, ended
        self.elapsed_bound = max(0, ended - started)
        self.ids = {'session': {}, 'run': {}}
        self.ledger = []
        self.cache_keys = {}
        self.transport_peers = {}
        self.closed_endpoint = None
        self.closed_error = None

    def record(self, artifact, path, before, after, kind, start=None, end=None):
        self.ledger.append(dict(artifact=artifact, path=list(path), kind=kind,
                                start=start, end=end, before=base64.b64encode(before).decode(),
                                after=base64.b64encode(after).decode(),
                                before_sha256=sha256(before), after_sha256=sha256(after),
                                before_length=len(before), after_length=len(after)))

    def identity(self, value, kind):
        if not re.fullmatch('[0-9a-f]{16}', value):
            raise AssertionError(f'invalid {kind} identity: {value!r}')
        mapping = self.ids[kind]
        return mapping.setdefault(value, f'{kind.upper()}_{len(mapping)+1}')

    def capture_peer(self, attempt, request):
        """Bind a transport peer to its independently captured request attempt."""
        address = request.get('client_address')
        if address is None:
            return
        if (not isinstance(address, (list, tuple)) or len(address) != 2 or address[0] != '127.0.0.1'
                or type(address[1]) is not int or not 1 <= address[1] <= 65535
                or request.get('authority', self.authority) != self.authority):
            raise AssertionError('captured transport peer escaped allocated loopback server')
        key = (attempt.get('run_id'), attempt.get('step'), attempt['data']['attempt'])
        self.transport_peers[key] = f'{address[0]}:{address[1]}'

    def transport_error(self, value, event):
        """Normalize only an exact reset tuple belonging to this captured attempt."""
        if event['type'] == 'api.attempt.finished':
            key = (event.get('run_id'), event.get('step'), event['data'].get('attempt'))
            peer = self.transport_peers.get(key)
        else:
            attempts = [(key[2], peer) for key, peer in self.transport_peers.items()
                        if key[:2] == (event.get('run_id'), event.get('step'))]
            peer = max(attempts)[1] if attempts else None
        if peer and value == f'read tcp {peer}->{self.authority}: read: connection reset by peer':
            return 'read tcp <CLIENT>-><SERVER>: read: connection reset by peer'
        return value

    def startup_banner(self, text):
        """Find only the startup banner or A34's exact witnessed recovery prelude."""
        if text.startswith('re:agent chat · '):
            return 0
        note = ('interrupted session restored; no pending model request or tool was restarted; '
                'send a new message; an in-flight tool has unknown effects; inspect them before further changes\n')
        if (Path(self.root).name in ('A34.live-crash-child-helper', 'A34.finished-undelivered-receipt')
                and text.startswith(note + 're:agent chat · ')):
            return len(note)
        return None

    def register_display_sessions(self, text):
        """Register new identities only inside the startup chat banner."""
        offset = self.startup_banner(text)
        if offset is None:
            return
        lines = text[offset:].split('\n')
        if not lines[0].startswith('re:agent chat · '):
            return
        if lines[0] == 're:agent chat · scripted':
            if len(lines) < 2 or not re.match(r'^workspace '+re.escape(self.root)+r'(?=$|[/\s])', lines[1]):
                raise AssertionError('malformed scripted chat workspace banner')
            return  # Scripted chat has no resumable session ID.
        workspace_line = 2
        if len(lines) > 2 and lines[2].startswith('requests go to '):
            proxy = re.fullmatch(r'requests go to (https?://([^/\s?#]+)(?:/[^\s]*)?) \(API_PROXY_URL\)', lines[2])
            if (proxy is None or proxy[2].rsplit('@', 1)[-1] != self.authority or not self.authority
                    or re.search(r'\([^()]* · API proxy, [^()]+\)$', lines[0]) is None):
                raise AssertionError('startup proxy banner escaped allocated API proxy')
            workspace_line = 3
        if len(lines) <= workspace_line or not re.match(r'^workspace '+re.escape(self.root)+r'(?=$|[/\s])', lines[workspace_line]):
            raise AssertionError('malformed startup chat workspace banner')
        match = re.fullmatch(r'session ID: ([0-9a-f]{16})', lines[1])
        if match is None:
            raise AssertionError('malformed startup stderr session identity')
        if offset and match[1] not in self.ids['session']:
            raise AssertionError('recovery banner lacks independently witnessed session')
        self.identity(match[1], 'session')

    def server_text(self, value, path=()):
        """Replace the allocated authority only in URL/HTTP Host slots."""
        if not self.authority:
            return value
        if path[-1:] == ('host',) and value == self.authority:
            return '<SERVER>'
        prefix = r'(https?://(?:[^/\s"@]+@)?)'
        value = re.sub(prefix + re.escape(self.authority) + r'(?=$|[/\s"?#])',
                       lambda match: match.group(1) + '<SERVER>', value)
        if path[-1:] in (('error',), ('display',)):
            value = value.replace('dial tcp '+self.authority+':', 'dial tcp <SERVER>:')
        return value

    def text(self, value, artifact, path):
        # Only callers at declared path-bearing locations reach this method.
        original = value
        prefixes = {self.root: '<ROOT>'}
        if self.artifacts and self.artifacts != self.root:
            prefixes[self.artifacts] = '<ARTIFACTS>'
        for prefix in sorted(prefixes, key=len, reverse=True):
            value = re.sub(re.escape(prefix) + r'(?=$|[/\s"\\:;,\)])', prefixes[prefix], value)
        value = self.server_text(value, path)
        for kind, mapping in self.ids.items():
            for old, new in mapping.items():
                # References in runner-owned trace/session paths and diagnostics.
                def reference(match):
                    if kind == 'session' and path in ((), ('display',)):
                        start = value.rfind('\n', 0, match.start())+1
                        end = value.find('\n', match.end())
                        line = value[start:end if end >= 0 else len(value)]
                        if line.lower().startswith('session id: '):
                            offset = self.startup_banner(value)
                            startup = offset is not None and value.count('\n', offset, start) == 1
                            footer = re.fullmatch(r'session ID: ([0-9a-f]{16}) · resume: reagent chat --resume \1', line)
                            if not startup and footer is None:
                                return match[0]
                    return new
                value = re.sub(r'(?<=[/ ])' + old + r'(?=$|[/\s.])', reference, value)
        if value != original:
            self.record(artifact, path, original.encode('utf-8', 'surrogateescape'), value.encode('utf-8', 'surrogateescape'), 'allocated-path-or-reference')
        return value

    def response(self, raw, artifact, slot=None):
        """Normalize only fixture allocation spans, retaining JSON/SSE framing."""
        try:
            lex(raw, opaque=True)
        except (ValueError, UnicodeError):
            parts = []
            for line in raw.splitlines(keepends=True):
                match = re.match(rb'(data:[ \t]*)(.*?)(\r?\n)?$', line)
                if match is None:
                    parts.append(line)
                    continue
                prefix, body, ending = match.groups()
                try:
                    lex(body, opaque=True)
                except (ValueError, UnicodeError):
                    parts.append(line)
                else:
                    parts.append(prefix + self.json(body, artifact, ('provider-response',), opaque=True, response_slot=slot) + (ending or b''))
            return b''.join(parts)
        return self.json(raw, artifact, ('provider-response',), opaque=True, response_slot=slot)

    def json(self, raw, artifact, path=(), event=None, opaque=False, response_slot=None):
        top = lex(raw, opaque=opaque)
        changes = []
        if not path and isinstance(top.value, dict) and 'type' in top.value and 'seq' in top.value:
            event = top.value['type']
        # Discover envelope IDs before any nested references.
        for key, token in top.children:
            if key in ('session_id', 'run_id') and event:
                kind = key.split('_')[0]
                self.identity(token.value, kind)

        def ancestors(loc):
            node = top.value
            result = [node]
            for key in loc:
                node = node[key]
                result.append(node)
            return result

        def native(loc):
            nodes = ancestors(loc)
            parent = nodes[-2] if len(nodes) > 1 else None
            if (isinstance(parent, dict) and parent.get('name') in ('switch_workspace', 'request_workspace_access')
                    and loc[-1:] == ('arguments',)):
                return False
            for node in nodes[:-1]:
                if isinstance(node, dict) and node.get('type') == 'tool_use' and node.get('name') in ('switch_workspace', 'request_workspace_access'):
                    return loc[-1:] != ('path',)
            if path[:1] == ('provider-response',) and 'workspace-arguments' not in path:
                return True
            if any(loc[i:i+2] == ('native', 'items') for i in range(len(loc)-1)):
                return True
            if any(isinstance(node, dict) and node.get('role') == 'assistant' for node in nodes[:-1]):
                return True
            return ('blocks' in loc and any(isinstance(node, dict) and node.get('kind') in ('text', 'refusal')
                                            for node in nodes[:-1]))

        def effect_workspace(loc, parent):
            required = {'workspace', 'step', 'call_id', 'tool', 'summary', 'effect'}
            optional = {'agent_id', 'run_id', 'path', 'operation', 'omitted_paths'}
            return (event in ('run.finished', 'model.switch.failed', 'auto.route')
                    and len(loc) == 4 and loc[:2] == ('data', 'effects') and type(loc[2]) is int
                    and loc[-1] == 'workspace' and isinstance(parent, dict)
                    and required <= set(parent) <= required | optional
                    and type(parent['step']) is int and parent['step'] >= 0
                    and all(isinstance(parent[key], str) for key in ('workspace', 'call_id', 'tool', 'summary'))
                    and bool(parent['call_id']) and bool(parent['tool'])
                    and parent['effect'] in ('none', 'applied', 'unknown'))

        def permitted_text(loc, parent):
            if not loc:
                return False
            key = loc[-1]
            if key == 'workspace' and loc[:2] == ('data', 'effects'):
                return False  # Effect records use their complete schema above.
            if key == 'instructions':
                return isinstance(parent, dict) and ('tools' in parent or event == 'run.started')
            if key == 'text' and len(loc) >= 3 and loc[0] == 'system':
                return True
            if key in ('workspace', 'previous_workspace') and isinstance(parent, dict):
                return (parent.get('kind') == 'workspace_state' or 'ok' in parent or 'changed' in parent
                        or (event == 'run.started' and loc == ('data', 'workspace'))
                        or 'current_model' in parent or 'command' in parent
                        or (event == 'tool.started' and loc == ('data', 'workspace'))
                        or 'operation' in parent)
            if key == 'trace_path':
                return event == 'run.finished' and loc == ('data', 'trace_path')
            if key == 'endpoint':
                return event == 'api.attempt.started' and loc == ('data', 'endpoint')
            if key == 'error':
                return event in ('api.attempt.finished', 'model.failed') and loc == ('data', 'error')
            if key == 'text' and isinstance(parent, dict):
                value = parent.get('text', '')
                return (isinstance(value, str) and value.startswith(('Workspace state when this message was sent, collected by re:agent:\n',
                                                                   'Historical conversation supplied by re:agent, encoded as JSON records below.')))
            if key == 'output' and isinstance(parent, dict):
                return parent.get('type') == 'function_call_output'
            if key == 'content' and isinstance(parent, dict):
                return parent.get('type') == 'tool_result'
            if key == 'state' and isinstance(parent, dict):
                return 'questions' in parent and 'model' in parent
            if key == 'arguments' and isinstance(parent, dict):
                return parent.get('name') in ('switch_workspace', 'request_workspace_access')
            if key == 'path':
                return isinstance(parent, dict) and set(parent) == {'path'}
            return False

        def visit(token, loc, parent):
            value = token.value
            last = loc[-1] if loc else None
            replacement = None
            rule = None
            exec_echo = False
            shell_timing = ((artifact == 'cache-input' and loc == ('history', 0, 'shell', 'duration_ms'))
                            or (re.fullmatch(r'trace-[1-9][0-9]*\.jsonl', artifact) and loc in (
                                ('data', 'history', 0, 'shell', 'duration_ms'),
                                ('data', 'initial_history', 0, 'shell', 'duration_ms'))))
            shell_timing = (shell_timing and Path(self.root).name.startswith('C17.')
                            and isinstance(parent, dict) and 'command' in parent)
            exec_timing = ((event == 'tool.finished' and top.value.get('data', {}).get('name') == 'exec'
                            and loc == ('data', 'outcome', 'data', 'duration_ms'))
                           or (not native(loc) and last == 'duration_ms' and isinstance(parent, dict)
                               and 'argv' in parent and 'timeout_ms' in parent
                               and ('tool' in loc or 'steps' in loc or 'output' in path)))
            if event and loc == ('time',):
                if not isinstance(value, str) or not re.fullmatch(r'\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?Z', value):
                    raise AssertionError('trace timestamp is not RFC3339Nano UTC')
                when = dt.datetime.fromisoformat(value.replace('Z', '+00:00')).timestamp()
                if not self.started-2 <= when <= self.ended+2:
                    raise AssertionError('trace timestamp outside process bounds')
                replacement, rule = go_string('<TIME>'), 'trace-time'
            elif event and loc in (('session_id',), ('run_id',)):
                replacement = go_string(self.identity(value, last.split('_')[0]))
                rule = 'typed-identity'
            elif last in ('session_id', 'run_id') and loc in (('data', 'scope', last), ('scope', last)):
                replacement = go_string(self.identity(value, last.split('_')[0]))
                rule = 'typed-scope-identity'
            elif event == 'run.started' and len(loc) == 3 and loc[:2] == ('data', 'approved_workspaces'):
                replacement, rule = go_string(self.text(value, artifact, path+loc)), 'approved-workspace-path'
            elif event == 'run.started' and loc == ('data', 'build'):
                if not isinstance(value, str):
                    raise AssertionError('run build metadata is not a string')
                replacement, rule = go_string('<BUILD>'), 'build-metadata'
            elif event in ('model.switch.finished', 'model.switch.failed') and loc == ('data', 'latency_ms'):
                if type(value) is not int or not 0 <= value <= self.elapsed_bound*1000:
                    raise AssertionError('model switch latency outside invocation bounds')
                replacement, rule = b'0', 'model-switch-latency-ms'
            elif ((event == 'api.attempt.finished' and loc == ('data', 'duration_ms'))
                  or (event == 'auto.route' and loc == ('data', 'router_latency_ms'))
                  or (not native(loc) and last == 'duration_ms' and 'shell' in loc
                      and isinstance(parent, dict) and 'command' in parent)
                  or exec_timing) and isinstance(value, (int, float)):
                if isinstance(value, bool) or not 0 <= value <= 3600000:
                    raise AssertionError('duration out of bounds')
                replacement, rule = b'0', 'duration-ms'
            elif event == 'api.retry.scheduled' and loc == ('data', 'delay_ms'):
                if type(value) is not int or not 0 <= value <= 144000:
                    raise AssertionError('retry delay outside policy bounds')
                replacement, rule = b'0', 'retry-delay-policy'
            elif isinstance(value, str):
                new = value
                if (event == 'api.attempt.started' and loc == ('data', 'endpoint')
                        and value == self.closed_endpoint):
                    new, rule = 'http://<CLOSED_SERVER>/v1/responses', 'case-specific-closed-endpoint'
                elif event in ('api.attempt.finished', 'model.failed') and loc == ('data', 'error'):
                    if value == self.closed_error:
                        authority = self.closed_endpoint.split('/')[2]
                        new, rule = value.replace(authority, '<CLOSED_SERVER>'), 'case-specific-closed-endpoint'
                    else:
                        new = self.transport_error(value, top.value)
                        rule = 'captured-transport-socket'
                        if new == value:
                            new, rule = self.text(value, artifact, path+loc), 'registered-text-span'
                elif event == 'run.finished' and loc == ('data', 'reason'):
                    new, rule = self.server_text(value), 'owned-run-reason-authority'
                elif (not native(loc) and getattr(self, 'commands', None) is not None
                      and (shell := self.commands.shell_text(value, artifact, path + loc, parent)) is not None):
                    new, rule = shell, 'cli-shell-workspace'
                elif not native(loc) and effect_workspace(loc, parent):
                    new, rule = self.text(value, artifact, path+loc), 'effect-record-workspace'
                elif last == 'date' and isinstance(parent, dict) and parent.get('kind') == 'workspace_state' and not native(loc):
                    # The per-case time zone and local-date invariant is also
                    # checked by W08; UTC is the runner's default.
                    date = dt.date.fromisoformat(value)
                    zone = parent.get('time_zone')
                    offsets = {'UTC': 0, 'PDT': -7, 'PST': -8, '-07': -7}
                    if zone in offsets and self.ended != float('inf'):
                        timezone = dt.timezone(dt.timedelta(hours=offsets[zone]))
                        first = dt.datetime.fromtimestamp(self.started, timezone).date()
                        last_date = dt.datetime.fromtimestamp(self.ended, timezone).date()
                        if not first <= date <= last_date:
                            raise AssertionError('snapshot date outside invocation bounds in recorded zone')
                    new, rule = '<DATE>', 'snapshot-date'
                elif last == 'request_body' and event == 'api.attempt.started':
                    # W33's trace is repaired; only captured bytes define the hash.
                    new = self.json(value.encode(), artifact, path+loc+('json',)).decode()
                    rule = 'embedded-request'
                elif last == 'response_body' and event == 'api.attempt.finished':
                    new = self.response(value.encode(), artifact).decode()
                    rule = 'embedded-response'
                elif (path[:1] == ('provider-response',) and self.authority
                      and (loc in (('detail',), ('error', 'message'), ('response', 'error', 'message'))
                           or (response_slot == 'catalog' and len(loc) == 3 and loc[0] == 'models'
                               and type(loc[1]) is int and loc[2] == 'display_name')
                           or (response_slot == 'discovery' and loc in (('issuer',), ('authorization_endpoint',), ('token_endpoint',),
                                       ('revocation_endpoint',), ('models_endpoint',), ('jwks_uri',))
                               and isinstance(top.value, dict) and 'issuer' in top.value))):
                    new, rule = self.server_text(value), 'allocated-fixture-authority'
                elif last == 'prompt_cache_key' and loc == ('prompt_cache_key',) and value in self.cache_keys:
                    new, rule = self.cache_keys[value], 'independently-validated-cache-key'
                elif not native(loc) and permitted_text(loc, parent):
                    # Explicit embedded layers retain their own token ordering.
                    if last in ('output', 'state', 'content', 'arguments') and value.startswith(('{', '[')):
                        layer = 'workspace-arguments' if last == 'arguments' and parent.get('name') in ('switch_workspace', 'request_workspace_access') else 'json'
                        if layer == 'workspace-arguments':
                            try:
                                lex(value.encode())
                            except (ValueError, UnicodeError):
                                return  # Rejected call arguments are literal diagnostic evidence.
                        ledger_start = len(self.ledger)
                        new = self.json(value.encode(), artifact, path+loc+(layer,)).decode()
                        exec_echo = any(row.get('kind') == 'duration-ms' for row in self.ledger[ledger_start:])
                    elif value.startswith('Historical conversation supplied by re:agent, encoded as JSON records below.'):
                        prefix, records = value.rsplit('\n', 1)
                        new = prefix+'\n'+self.json(records.encode(), artifact, path+loc+('handoff',)).decode()
                    else:
                        new = self.text(value, artifact, path+loc)
                        marker = 'Workspace state when this message was sent, collected by re:agent:\n'
                        if marker in new:
                            prefix, snapshot = new.split(marker, 1)
                            decoder = json.JSONDecoder()
                            start = len(snapshot) - len(snapshot.lstrip(' \t\r\n'))
                            _, stop = decoder.raw_decode(snapshot, start)
                            normalized = self.json(snapshot[:stop].encode(), artifact, path+loc+('snapshot',)).decode()
                            new = prefix + marker + normalized + snapshot[stop:]
                    rule = rule or 'registered-text-span'
                if new != value or exec_echo:
                    replacement = string_spans(raw[token.start:token.end], value, new)
            if replacement is not None and (replacement != raw[token.start:token.end] or exec_timing or exec_echo or shell_timing):
                self.record(artifact, path+loc, raw[token.start:token.end], replacement, rule, token.start, token.end)
                if replacement != raw[token.start:token.end]:
                    changes.append((token.start, token.end, replacement))
                return
            for key, item in token.children:
                visit(item, loc+(key,), value)

        visit(top, (), None)
        normalized = replace_spans(raw, changes)
        races = getattr(self, 'races', None)
        if races is not None and path[:1] != ('provider-response',):
            normalized = races.json(raw, normalized, artifact)
            if (races.record is not None and isinstance(top.value, dict)
                    and all(key in top.value for key in ('model', 'instructions', 'tools', 'history'))):
                for index, entry in reversed(list(enumerate(top.value['history']))):
                    if entry.get('kind') == 'shell':
                        normalized = races.shell(raw, normalized, artifact, ('history', index, 'shell'))
        return normalized


def register_closed_endpoint(ctx, normal, events, captures):
    """Certify only W27's separately allocated, deliberately closed loopback port."""
    if Path(ctx.root).name != 'W27.connection-refused':
        return
    starts = [event for event in events if event['type'] == 'api.attempt.started']
    finishes = [event for event in events if event['type'] == 'api.attempt.finished']
    if captures or len(starts) != 1 or len(finishes) != 1 or len(ctx.invocations) != 1:
        raise AssertionError('closed-port case requires one uncaptured attempt')
    start, finish = starts[0], finishes[0]
    endpoint = start['data']['endpoint']
    match = re.fullmatch(r'http://(127\.0\.0\.1:([1-9][0-9]{0,4}))/v1/responses', endpoint)
    if match is None or int(match[2]) > 65535 or match[1] == normal.authority:
        raise AssertionError('closed-port case endpoint is not a separately allocated loopback port')
    expected = f'Post "{endpoint}": dial tcp {match[1]}: connect: connection refused'
    if ((start['run_id'], start.get('step'), start['data']['attempt']) !=
            (finish['run_id'], finish.get('step'), finish['data']['attempt'])
            or start['data']['attempt'] != 1 or finish['data']['http_status'] != 0
            or finish['data'].get('error') != expected
            or any(key in finish['data'] for key in ('response_body', 'body_utf8_replaced'))):
        raise AssertionError('closed-port case lacks its exact owned connection-refused failure')
    environment = ctx.invocations[0].get('env')
    source = 'legacy-trace'
    if environment is not None:
        base = environment.get('REAGENT_OPENAI_BASE_URL')
        if not isinstance(base, str) or base.rstrip('/')+'/responses' != endpoint:
            raise AssertionError('closed-port trace endpoint differs from independent invocation environment')
        source = 'invocation-env'
    normal.closed_endpoint, normal.closed_error = endpoint, expected
    normal.record('closed-endpoint-certificate', ('W27.connection-refused',), endpoint.encode(),
                  b'http://<CLOSED_SERVER>/v1/responses', 'case-specific-closed-port-'+source)
    normal.ledger[-1].update(classification='S', case='W27.connection-refused')


def auth_status_stdout(ctx, normal, invocation, captures, artifact):
    """Normalize CLI auth display only when invocation input certifies its owner."""
    argv = invocation.get('argv')
    if argv is not None:
        certified = list(argv[1:3]) == ['auth', 'status']
    else:
        certified = re.match(r'^W(?:53\.issuer\.(?:models|refresh)|54\.auth-(?:models|refresh))\.', Path(ctx.root).name) is not None
    raw = invocation['stdout']
    if not certified:
        return raw
    lines = raw.splitlines(keepends=True)
    if len(lines) < 3 or not lines[0].startswith(b'Account: ') or lines[1] != b'ChatGPT plan usage: enabled\n':
        return raw
    models, lifetimes = {}, []
    for request in captures:
        response = request.get('response')
        if response is None or not response.completed or not 200 <= response.status < 300:
            continue
        try:
            body = lex(response.body).value
        except (ValueError, UnicodeError):
            continue
        if not isinstance(body, dict):
            continue
        path = request['path'].rstrip('/')
        if request['method'] == 'GET' and path.endswith('/models'):
            for model in body.get('models', []) if isinstance(body.get('models'), list) else []:
                if isinstance(model, dict) and isinstance(model.get('slug'), str) and isinstance(model.get('display_name'), str):
                    models[model['slug']] = model['display_name']
        if request['method'] == 'POST' and path.endswith('/oauth/token'):
            lifetime = body.get('expires_in')
            if type(lifetime) is int and lifetime > 0 and isinstance(body.get('access_token'), str) and body['access_token']:
                lifetimes.append(lifetime)
    changes = []
    expiry_prefix = b'Access token expires: '
    if lifetimes:
        match = re.fullmatch(rb'Access token expires: (\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z)\n', lines[2])
        if match is None:
            raise AssertionError('refreshed auth expiry display is not RFC3339 UTC')
        try:
            when = dt.datetime.fromisoformat(match[1].decode().replace('Z', '+00:00')).timestamp()
        except ValueError as error:
            raise AssertionError('refreshed auth expiry display has an invalid timestamp') from error
        if not any(invocation['started']+lifetime-2 <= when <= invocation['ended']+lifetime+2 for lifetime in lifetimes):
            raise AssertionError('refreshed auth expiry differs from captured lifetime and invocation interval')
        start = len(lines[0])+len(lines[1])+len(expiry_prefix)
        replacement = b'<AUTH-EXPIRY>'
        changes.append((start, start+len(match[1]), replacement))
        normal.record(artifact, ('access_token_expires',), match[1], replacement,
                      'validated-auth-refresh-expiry', start, start+len(match[1]))
    if len(lines) > 3 and lines[3] == b'Available models:\n':
        offset = sum(map(len, lines[:4]))
        for line in lines[4:]:
            for slug, source_name in models.items():
                prefix = ('  '+slug+' (').encode()
                if (line.startswith(prefix) and line.endswith(b')\n')
                        and normal.server_text(source_name) != source_name):
                    before = line[len(prefix):-2]
                    after = normal.server_text(before.decode('utf-8', 'surrogateescape')).encode('utf-8', 'surrogateescape')
                    if before != after:
                        start = offset+len(prefix)
                        changes.append((start, start+len(before), after))
                        normal.record(artifact, ('models', slug, 'display_name'), before, after,
                                      'auth-status-catalog-display-name', start, start+len(before))
                    break
            offset += len(line)
    return replace_spans(raw, changes)


def finish(ctx):
    """Validate raw evidence, normalize declared dynamic fields, retain a ledger."""
    invocations = ctx.invocations
    started = min((min(x['started'], x['ended']) for x in invocations), default=0)
    ended = max((max(x['started'], x['ended']) for x in invocations), default=float('inf'))
    server = getattr(ctx, '_server', None)
    authority = server.url.split('://', 1)[1] if server else ''
    normal = Normalizer(ctx.root, authority, started, ended,
                        artifacts=getattr(ctx, 'artifact_root', ctx.artifacts))
    if invocations and all('monotonic_started' in item or 'monotonic_ended' in item for item in invocations):
        from parity.invocation_time import validate_bounds
        bounds = [validate_bounds(item) for item in invocations]
        normal.elapsed_bound = max(end for _, end in bounds) - min(start for start, _ in bounds)
    from parity.observe_cli_commands import Commands
    normal.commands = Commands(normal, ctx)
    results = {}
    semantic_artifacts = set()
    trace_files = ctx.trace_paths
    traces = []
    requests = []
    cache_inputs = []
    from parity.observe_trace_failures import existing_literal, restore_exchange
    for path in trace_files:
        raw = Path(path).read_bytes()
        if existing_literal(ctx, path, raw):
            results[str(Path(path).relative_to(ctx.artifacts))] = raw
            continue
        lines = raw.splitlines(keepends=True)
        seq = 0
        for line in lines:
            event = lex(line).value
            if event.get('schema_version') != 1 or event['seq'] != seq+1:
                raise AssertionError('trace event missing or reordered')
            seq += 1
            if event['type'] == 'api.attempt.started':
                data = event['data']
                requests.append(data)
            if event['type'] in ('model.requested', 'compaction.requested'):
                data = event['data']
                if all(k in data for k in ('model', 'instructions', 'tools', 'history')):
                    data_token = child(lex(line), 'data')
                    requestspec = line[data_token.start:data_token.end]
                    cache_inputs.append(requestspec)
            traces.append((path, line))
    # Link trace request bodies to independent HTTP captures, not to themselves.
    captures = server.requests if server else []
    from parity.observe_tools import ARTIFACTS, Tools
    from parity.observe_cli_dependencies import WorkspaceEchoes, cancelled_exec_executable
    from parity.observe_cli_races import Races
    observations = getattr(ctx, 'observations', {})
    case_id = Path(ctx.root).name
    tools = Tools(normal, traces, captures, invocations=invocations,
                  case_id=case_id, observations=observations)
    echoes = WorkspaceEchoes(normal, traces, invocations, observations, case_id)
    tools.path_outcomes.update(echoes.path_outcomes)
    tools.path_outcomes.update(cancelled_exec_executable(normal, traces, invocations, observations, case_id))
    normal.json = echoes.json
    events = [lex(line).value for _, line in traces]
    from parity.observe_fixture_scripts import Scripts
    scripts = Scripts(normal, events, case_id, observations)
    normal.json = scripts.json
    normal.races = Races(normal, events, captures, invocations, case_id, observations)
    from parity.observe_agents_context import Agents
    source_root = Path(getattr(ctx, 'artifact_root', ctx.artifacts))
    source_traces = [(source_root / Path(path).relative_to(ctx.artifacts), line) for path, line in traces]
    agents = Agents(normal, source_traces, captures, invocations, case_id, observations, exec_slots=tools.exec)
    normal.json, normal.text = agents.json, agents.text
    capture_order = list(range(len(captures)))
    if agents.enabled:
        source_order = {path: index for index, path in enumerate(agents.trace_paths)}
        traces.sort(key=lambda item: source_order[str(source_root / Path(item[0]).relative_to(ctx.artifacts))])
        capture_order.sort(key=lambda index: agents.capture_slots.get(index, (len(agents.actor_slots), str(index))))
        normal.ledger.append(dict(artifact='agent-causal-correspondence', kind='agent-causal-correspondence',
            classification='S', trace_sources=agents.trace_paths, http_source_order=capture_order,
            logical_slots=[agents.capture_slots.get(index) for index in capture_order]))
    for requestspec in cache_inputs:
        raw_key = cache_key(requestspec)
        canonical_spec = normal.json(requestspec, 'cache-input')
        canonical_key = cache_key(canonical_spec)
        normal.cache_keys[raw_key] = canonical_key
        normal.record('cache-key', (), raw_key.encode(), canonical_key.encode(), 'cache-prefix')
    # Public proxy endpoints need not end in /responses. Identify generative
    # requests by their recorded logical shape, not a path spelling.
    def is_generation(request):
        try:
            body = lex(request['body'], opaque=True).value
        except (ValueError, UnicodeError):
            return False
        return isinstance(body, dict) and 'model' in body and ('input' in body or 'messages' in body)

    generative = [r for r in captures if is_generation(r)]
    register_closed_endpoint(ctx, normal, events, captures)
    remaining = list(generative)
    for attempt in (event for event in events if event['type'] == 'api.attempt.started'):
        data = attempt['data']
        captured = next((request for request in remaining
                         if sha256(request['body']) == data['request_sha256']), None)
        if captured is not None:
            remaining.remove(captured)
            normal.capture_peer(attempt, captured)
            body = captured['body']
        else:
            finishes = [event['data'] for event in events
                        if event['type'] == 'api.attempt.finished'
                        and (event['run_id'], event['step'], event['data']['attempt']) ==
                            (attempt['run_id'], attempt['step'], data['attempt'])]
            if len(finishes) != 1 or finishes[0]['http_status'] != 0 or not finishes[0].get('error'):
                raise AssertionError('uncaptured HTTP attempt lacks matching pre-response transport failure')
            # A failed dial has no server capture. The prepared-body digest still
            # proves exact UTF-8 trace bytes; repaired W33 strings cannot match.
            body = data['request_body'].encode()
        check_digest(body, data['request_sha256'])
        parsed = lex(body, opaque=True).value
        key = parsed.get('prompt_cache_key')
        if key and key not in normal.cache_keys:
            raise AssertionError('wire cache key is not independently derived from logical request')
    restored, remaining = restore_exchange(ctx, normal, traces, captures, remaining)
    results.update(restored)
    if remaining:
        raise AssertionError('HTTP capture has no corresponding traced attempt')
    # Previews intentionally have no model.requested trace. Reconstruct their
    # first Entry independently instead of accepting an unverified cache key.
    for invocation in invocations:
        raw = invocation['stdout']
        if not raw.lstrip().startswith(b'{'):
            continue
        try:
            prefix = preview_prefix(raw)
        except (ValueError, UnicodeError):
            continue
        if prefix is not None:
            key = cache_key(prefix)
            if child(lex(raw), 'prompt_cache_key').value != key:
                raise AssertionError('preview cache key does not match its independent prefix')
            canonical = normal.json(prefix, 'preview-cache-input')
            normal.cache_keys[key] = cache_key(canonical)
    from parity.observe_derived import Derived
    derived = Derived(normal, traces, captures, invocations=invocations, observations=observations)
    # Validate all identities before processing references in stdout/stderr.
    for _, line in traces:
        ev = json.loads(line)
        normal.identity(ev['session_id'], 'session')
        normal.identity(ev['run_id'], 'run')
    for invocation in invocations:
        if 'terminal' not in invocation:
            normal.register_display_sessions(invocation['stderr'].decode('utf-8', 'surrogateescape'))
    from parity.observe_cli_sessions import local_reset
    results.update(local_reset(ctx, normal, invocations, traces))
    from parity.observe_cli_terminal import integrate
    # Replay stores files in a temporary directory, but displayed paths refer to
    # the original allocation. Terminal certificates bind those original paths.
    from types import SimpleNamespace
    artifact_root = Path(getattr(ctx, 'artifact_root', ctx.artifacts))
    terminal_ctx = SimpleNamespace(root=ctx.root, artifacts=artifact_root, artifact_root=artifact_root,
                                   observations=observations, _server=server,
                                   invocations=[dict(item, trace=artifact_root / ('invoke-%03d.traces' % index))
                                                for index, item in enumerate(invocations, 1)])
    terminal_traces = [(artifact_root / Path(path).relative_to(ctx.artifacts), line) for path, line in traces]
    screens, _, terminal_stderr = integrate(terminal_ctx, normal, terminal_traces)
    results.update(screens)
    semantic_artifacts.update(terminal_stderr)
    for i, invocation in enumerate(invocations):
        for channel in ('stdout', 'stderr'):
            raw = invocation[channel]
            name = f'invocation-{i+1}.{channel}'
            if name in terminal_stderr:
                results[name] = terminal_stderr[name]
                continue
            if channel == 'stdout' and raw.lstrip().startswith(b'{'):
                try:
                    value = lex(raw).value
                except (ValueError, json.JSONDecodeError):
                    value = None
                if isinstance(value, dict) and ('input' in value or 'messages' in value):
                    # Preview lacks a model.requested trace. W10 also checks
                    # the independent prefix against the live same-input run.
                    results[name] = normal.json(raw, name)
                    continue
            if channel == 'stdout' and Path(ctx.root).name.startswith('C02.'):
                from parity.observe_cli_args import version_stdout
                results[name] = version_stdout(normal, invocation, name)
                continue
            if channel == 'stdout':
                # Only independently certified auth CLI output is not model text.
                results[name] = auth_status_stdout(ctx, normal, invocation, captures, name)
                continue
            # Process output may contain opaque native bytes; trace JSONL above
            # remains strict. Surrogate escape preserves every untouched byte.
            text = normal.text(raw.decode('utf-8', 'surrogateescape'), name, ())
            if channel == 'stderr':
                if Path(ctx.root).name == 'C04.oversized':
                    from parity.observe_cli_args import oversized_stderr
                    text = oversized_stderr(normal, invocation, name).decode('utf-8', 'surrogateescape')
                text = derived.display(text, name, invocation)
                text = normal.commands.display(text, name, invocation)
                text = normal.races.text(text, name)
            results[name] = text.encode('utf-8', 'surrogateescape')
        results[f'invocation-{i+1}.status'] = str(invocation['returncode']).encode()
    paths = []
    for path, _ in traces:
        if path not in paths:
            paths.append(path)
    for i, path in enumerate(paths):
        name = f'trace-{i+1}.jsonl'
        parts = []
        for _, line in [(p,l) for p,l in traces if p == path]:
            ev = json.loads(line)
            normalized = normal.json(line, name)
            if ev['type'] == 'api.attempt.started':
                tok = lex(normalized)
                data = child(tok, 'data')
                digest = child(data, 'request_sha256')
                captured = derived.request(ev)
                body = normal.json(captured['body'], name+'.captured-request', opaque=True)
                replacement = go_string(sha256(body))
                normal.record(name, ('data','request_sha256'), normalized[digest.start:digest.end], replacement, 'validated-derived-digest', digest.start,digest.end)
                normalized = replace_spans(normalized, [(digest.start,digest.end,replacement)])
            normalized = derived.trace(line, normalized, name)
            normalized = tools.trace(line, normalized, name)
            parts.append(normalized)
        results[name] = b''.join(parts)
    for i, capture_index in enumerate(capture_order):
        request = captures[capture_index]
        prefix = f'http-{i+1}'
        content_type = next((v for k,v in request['headers'].items() if k.lower() == 'content-type'), '')
        if 'application/json' in content_type and request['body']:
            results[prefix+'.body'] = normal.json(request['body'], prefix+'.body', opaque=True)
        else:
            results[prefix+'.body'] = request['body']
        results[prefix+'.target'] = (request['method']+' '+request['path']).encode()
        headers = request['headers']
        # Only language-library User-Agent is outside the HTTP parity contract.
        meaningful = {k.lower(): v for k,v in headers.items() if k.lower() != 'user-agent'}
        if 'content-length' in meaningful:
            if meaningful['content-length'] != str(len(request['body'])):
                raise AssertionError('request Content-Length differs from captured bytes')
            replacement = str(len(results[prefix+'.body']))
            normal.record(prefix+'.headers', ('content-length',), meaningful['content-length'].encode(), replacement.encode(), 'validated-derived-length')
            meaningful['content-length'] = replacement
        for key, value in meaningful.items():
            if key in ('session_id', 'session-id'):
                wire_key = lex(request['body'], opaque=True).value['prompt_cache_key']
                canonical_key = normal.cache_keys[wire_key]
                if key == 'session_id':
                    if value != str(uuid.uuid5(uuid.NAMESPACE_URL, wire_key)):
                        raise AssertionError('plan session_id is not UUIDv5(URL, independently validated cache key)')
                    replacement = str(uuid.uuid5(uuid.NAMESPACE_URL, canonical_key))
                else:
                    if value != wire_key:
                        raise AssertionError('proxy session-id does not match independently validated cache key')
                    replacement = canonical_key
                normal.record(prefix+'.headers', (key,), value.encode(), replacement.encode(), 'cache-header')
                meaningful[key] = replacement
            else:
                meaningful[key] = normal.text(value, prefix+'.headers', (key,))
        results[prefix+'.headers'] = go_json(dict(sorted(meaningful.items())))
        response = request.get('response')
        if response is not None:
            import gzip
            response_name = prefix+'.response'
            slot = None
            if request['method'] == 'GET':
                if request['path'].rstrip('/').endswith('/.well-known/openid-configuration'):
                    slot = 'discovery'
                elif request['path'].rstrip('/').endswith('/models'):
                    slot = 'catalog'
            from parity.observe_delivery import project_delivery
            delivery = project_delivery(Path(ctx.root).name, request, response, invocations, events)
            body = response.body
            response_headers = {k.lower(): v for k, v in response.headers.items()}
            transport = dict(reset=response.reset, close=response.close, completed=response.completed,
                             chunk_lengths=[len(chunk) for chunk in response.chunks])
            if delivery is not None:
                normal.ledger.append(dict(artifact=response_name+'.delivery-certificate', **delivery['proof']))
                for suffix, before, after in (
                        ('.wire-body', body, delivery['body']),
                        ('.headers', go_json(dict(sorted(response_headers.items()))), go_json(dict(sorted(delivery['headers'].items())))),
                        ('.transport', go_json(transport), go_json(delivery['transport']))):
                    name = response_name+suffix
                    normal.record(name, (), before, after, 'registered-fixture-cancel-delivery-projection')
                    semantic_artifacts.add(name)
                body, response_headers, transport = delivery['body'], dict(delivery['headers']), delivery['transport']
            results[response_name+'.wire-body'] = normal.response(body, response_name+'.wire-body', slot=slot)
            from email.utils import parsedate_to_datetime
            for key, value in response_headers.items():
                if key == 'retry-after' and ',' in value:
                    date = parsedate_to_datetime(value)
                    if date.tzinfo is None:
                        raise AssertionError('Retry-After HTTP date lacks a zone')
                    # Near-now dates are allocated by the fixture. Historical
                    # literal dates remain byte-exact, including past-date cases.
                    if started-2 <= date.timestamp() <= ended+65:
                        normal.record(response_name+'.headers', (key,), value.encode(), b'<HTTP-DATE>', 'allocated-retry-date')
                        response_headers[key] = '<HTTP-DATE>'
                else:
                    response_headers[key] = normal.text(value, response_name+'.headers', (key,))
            results[response_name+'.headers'] = go_json(dict(sorted(response_headers.items())))
            results[response_name+'.transport'] = go_json(transport)
            results[response_name+'.status'] = str(response.status).encode()
            # Compressed payload and decoded client boundary are distinct evidence.
            if response_headers.get('content-encoding', '').lower() == 'gzip' and response.completed:
                try:
                    decoded = gzip.decompress(response.body)
                except (OSError, EOFError):
                    # Malformed/truncated gzip is an intentional transport fixture;
                    # never fabricate client-decoded bytes from its wire payload.
                    pass
                else:
                    results[response_name+'.decoded-body'] = normal.response(decoded, response_name+'.decoded-body', slot=slot)
    for name in sorted(ARTIFACTS):
        if name in observations:
            results[name] = tools.artifact(observations[name], name)
    from parity.observe_checkpoint_artifacts import Checkpoints, owns_artifact as checkpoint_artifact
    checkpoint_traces = [(source_root / Path(path).relative_to(ctx.artifacts), line) for path, line in traces]
    if any(checkpoint_artifact(name) for name in observations):
        checkpoints = Checkpoints(normal, ctx, checkpoint_traces, agents=agents)
        for name in sorted(observations):
            if checkpoint_artifact(name):
                results[name] = checkpoints.artifact(name, observations[name])
    projection = 'agents-checkpoints/admission-sharing-projection.json'
    if projection in observations:
        target = 'retained/' + projection
        results[target] = agents.artifact(observations[projection], target)
    for name, raw in results.items():
        normal.ledger.append(dict(artifact=name, raw_normalized_sha256=sha256(raw), normalized_length=len(raw)))
    Path(ctx.artifacts, 'substitutions.json').write_text(json.dumps(normal.ledger, indent=2)+'\n')
    touched = {item['artifact'] for item in normal.ledger if 'kind' in item}
    ctx.artifact_policies = {name: 'S' if name in semantic_artifacts else 'N' if name in touched else 'B' for name in results}
    return results


def encode_artifacts(artifacts):
    return {name: base64.b64encode(raw).decode() for name, raw in artifacts.items()}


def decode_artifacts(encoded):
    return {name: base64.b64decode(raw, validate=True) for name, raw in encoded.items()}

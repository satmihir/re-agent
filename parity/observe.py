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
            while pos < len(raw):
                if raw[pos] == 92:
                    pos += 2
                    continue
                if raw[pos] == 34:
                    pos += 1
                    break
                pos += 1
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
        stop = pos+(6 if raw[pos+1] == 117 else 2)
        if raw[pos+1] == 117 and 0xD800 <= int(raw[pos+2:stop], 16) <= 0xDBFF:
            if raw[stop:stop+2] == b'\\u' and 0xDC00 <= int(raw[stop+2:stop+6], 16) <= 0xDFFF:
                stop += 6
        character = json.loads(b'"'+raw[pos:stop]+b'"')
        decoded += len(character.encode('utf-8', errors='surrogatepass'))
        pos = stop
    return replace_spans(raw, [(offsets[start], offsets[end], go_string(value.decode('utf-8'))[1:-1])
                               for start, end, value in changes])


def string_spans(raw, before, after):
    """Preserve untouched lexemes, including inside large embedded JSON bodies."""
    if before == after:
        return raw
    old, new = before.encode('utf-8'), after.encode('utf-8')
    if before.lstrip().startswith(('{', '[')):
        try:
            left, right = lex(old), lex(new)
        except (ValueError, UnicodeError):
            left = right = None
        if left is not None:
            patches = []

            def walk(a, b):
                if old[a.start:a.end] == new[b.start:b.end]:
                    return
                if a.children or b.children:
                    if [key for key, _ in a.children] != [key for key, _ in b.children]:
                        raise AssertionError('normalization changed embedded JSON shape/order')
                    for (_, x), (_, y) in zip(a.children, b.children):
                        walk(x, y)
                    return
                patches.append((a.start, a.end, new[b.start:b.end]))

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


class Normalizer:
    def __init__(self, root, authority, started=0, ended=float('inf')):
        self.root = str(root)
        self.authority = authority
        self.started, self.ended = started, ended
        self.ids = {'session': {}, 'run': {}}
        self.ledger = []
        self.cache_keys = {}

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

    def text(self, value, artifact, path):
        # Only callers at declared path-bearing locations reach this method.
        original = value
        value = re.sub(re.escape(self.root) + r'(?=$|[/\s"\\:;,\)])', '<ROOT>', value)
        if self.authority:
            value = value.replace(self.authority, '<SERVER>')
        for kind, mapping in self.ids.items():
            for old, new in mapping.items():
                # References in runner-owned trace/session paths and diagnostics.
                value = re.sub(r'(?<=[/ ])' + old + r'(?=$|[/\s.])', new, value)
        if value != original:
            self.record(artifact, path, original.encode(), value.encode(), 'allocated-path-or-reference')
        return value

    def json(self, raw, artifact, path=(), event=None, opaque=False):
        top = lex(raw, opaque=opaque)
        changes = []
        if isinstance(top.value, dict) and 'type' in top.value and 'seq' in top.value:
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
            if any(loc[i:i+2] == ('native', 'items') for i in range(len(loc)-1)):
                return True
            if any(isinstance(node, dict) and node.get('role') == 'assistant' for node in nodes[:-1]):
                return True
            return ('blocks' in loc and any(isinstance(node, dict) and node.get('kind') in ('text', 'refusal')
                                            for node in nodes[:-1]))

        def permitted_text(loc, parent):
            if not loc:
                return False
            key = loc[-1]
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
                return event == 'api.attempt.finished' and loc == ('data', 'error')
            if key == 'text' and isinstance(parent, dict):
                value = parent.get('text', '')
                return (isinstance(value, str) and value.startswith(('Workspace state when this message was sent, collected by re:agent:\n',
                                                                   'Historical conversation supplied by re:agent, encoded as JSON records below.')))
            if key == 'output' and isinstance(parent, dict):
                return parent.get('type') == 'function_call_output'
            if key == 'content' and isinstance(parent, dict):
                return parent.get('type') == 'tool_result'
            if key == 'state' and isinstance(parent, dict):
                return 'criteria' in parent
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
            if event and loc == ('time',):
                if not isinstance(value, str) or not value.endswith('Z'):
                    raise AssertionError('trace timestamp is not UTC')
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
                replacement, rule = go_string('<BUILD>'), 'build-metadata'
            elif ((event == 'api.attempt.finished' and loc == ('data', 'duration_ms'))
                  or (event == 'model.switch.finished' and loc == ('data', 'latency_ms'))
                  or (event == 'auto.route' and loc == ('data', 'router_latency_ms'))
                  or (last == 'duration_ms' and 'shell' in loc)) and isinstance(value, (int, float)):
                if isinstance(value, bool) or not 0 <= value <= 3600000:
                    raise AssertionError('duration out of bounds')
                replacement, rule = b'0', 'duration-ms'
            elif event == 'api.retry.scheduled' and loc == ('data', 'delay_ms'):
                if type(value) is not int or not 0 <= value <= 144000:
                    raise AssertionError('retry delay outside policy bounds')
                replacement, rule = b'0', 'retry-delay-policy'
            elif isinstance(value, str):
                new = value
                if last == 'date' and isinstance(parent, dict) and parent.get('kind') == 'workspace_state' and not native(loc):
                    # The per-case time zone and local-date invariant is also
                    # checked by W08; UTC is the runner's default.
                    dt.date.fromisoformat(value)
                    new, rule = '<DATE>', 'snapshot-date'
                elif last in ('request_body',) and event == 'api.attempt.started':
                    new = self.json(value.encode(), artifact, path+loc+('json',)).decode()
                    rule = 'embedded-request'
                elif last == 'prompt_cache_key' and loc == ('prompt_cache_key',) and value in self.cache_keys:
                    new, rule = self.cache_keys[value], 'independently-validated-cache-key'
                elif not native(loc) and permitted_text(loc, parent):
                    # Explicit embedded layers retain their own token ordering.
                    if last in ('output', 'state', 'content', 'arguments') and value.startswith(('{', '[')):
                        new = self.json(value.encode(), artifact, path+loc+('json',)).decode()
                    else:
                        new = self.text(value, artifact, path+loc)
                        marker = 'Workspace state when this message was sent, collected by re:agent:\n'
                        if marker in new:
                            prefix, snapshot = new.split(marker, 1)
                            decoder = json.JSONDecoder()
                            _, stop = decoder.raw_decode(snapshot)
                            normalized = self.json(snapshot[:stop].encode(), artifact, path+loc+('snapshot',)).decode()
                            new = prefix + marker + normalized + snapshot[stop:]
                    rule = rule or 'registered-text-span'
                if new != value:
                    replacement = string_spans(raw[token.start:token.end], value, new)
            if replacement is not None and replacement != raw[token.start:token.end]:
                self.record(artifact, path+loc, raw[token.start:token.end], replacement, rule, token.start, token.end)
                changes.append((token.start, token.end, replacement))
                return
            for key, item in token.children:
                visit(item, loc+(key,), value)

        visit(top, (), None)
        return replace_spans(raw, changes)


def finish(ctx):
    """Validate raw evidence, normalize declared dynamic fields, retain a ledger."""
    invocations = ctx.invocations
    started = min((x['started'] for x in invocations), default=0)
    ended = max((x['ended'] for x in invocations), default=float('inf'))
    server = getattr(ctx, '_server', None)
    authority = server.url.split('://', 1)[1] if server else ''
    normal = Normalizer(ctx.root, authority, started, ended)
    results = {}
    trace_files = ctx.trace_paths
    traces = []
    requests = []
    for path in trace_files:
        raw = Path(path).read_bytes()
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
                    raw_key = cache_key(requestspec)
                    canonical_spec = normal.json(requestspec, 'cache-input')
                    canonical_key = cache_key(canonical_spec)
                    normal.cache_keys[raw_key] = canonical_key
                    normal.record('cache-key', (), raw_key.encode(), canonical_key.encode(), 'cache-prefix')
            traces.append((path, line))
    # Link trace request bodies to independent HTTP captures, not to themselves.
    captures = server.requests if server else []
    # Public proxy endpoints need not end in /responses. Identify generative
    # requests by their recorded logical shape, not a path spelling.
    def is_generation(request):
        try:
            body = lex(request['body'], opaque=True).value
        except (ValueError, UnicodeError):
            return False
        return isinstance(body, dict) and 'model' in body and ('input' in body or 'messages' in body)

    generative = [r for r in captures if is_generation(r)]
    if len(requests) != len(generative):
        raise AssertionError(f'HTTP/trace attempt count differs: {len(generative)}/{len(requests)}')
    for data, captured in zip(requests, generative):
        check_digest(captured['body'], data['request_sha256'])
        parsed = lex(captured['body'], opaque=True).value
        key = parsed.get('prompt_cache_key')
        if key and key not in normal.cache_keys:
            raise AssertionError('wire cache key is not independently derived from logical request')
    # Validate all identities before processing references in stdout/stderr.
    for _, line in traces:
        ev = json.loads(line)
        normal.identity(ev['session_id'], 'session')
        normal.identity(ev['run_id'], 'run')
    for i, invocation in enumerate(invocations):
        for channel in ('stdout', 'stderr'):
            raw = invocation[channel]
            raw.decode('utf-8', errors='strict')
            name = f'invocation-{i+1}.{channel}'
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
            text = normal.text(raw.decode(), name, ())
            results[name] = text.encode()
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
                body = child(data, 'request_body').value.encode()
                digest = child(data, 'request_sha256')
                derived = go_string(sha256(body))
                normal.record(name, ('data','request_sha256'), normalized[digest.start:digest.end], derived, 'validated-derived-digest', digest.start,digest.end)
                normalized = replace_spans(normalized, [(digest.start,digest.end,derived)])
            parts.append(normalized)
        results[name] = b''.join(parts)
    for i, request in enumerate(captures):
        prefix = f'http-{i+1}'
        content_type = next((v for k,v in request['headers'].items() if k.lower() == 'content-type'), '')
        if 'application/json' in content_type and request['body']:
            results[prefix+'.body'] = normal.json(request['body'], prefix+'.body', opaque=True)
        else:
            results[prefix+'.body'] = request['body']
        results[prefix+'.target'] = (request['method']+' '+request['path']).encode()
        headers = request['headers']
        meaningful = {k.lower(): v for k,v in headers.items() if k.lower() in (
            'content-type','accept-encoding','authorization','anthropic-version','anthropic-beta',
            'x-api-key','session_id','session-id','x-openai-cache-key','chatgpt-account-id','originator')}
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
    for name, raw in results.items():
        normal.ledger.append(dict(artifact=name, raw_normalized_sha256=sha256(raw), normalized_length=len(raw)))
    Path(ctx.artifacts, 'substitutions.json').write_text(json.dumps(normal.ledger, indent=2)+'\n')
    touched = {item['artifact'] for item in normal.ledger if 'kind' in item}
    ctx.artifact_policies = {name: 'N' if name in touched else 'B' for name in results}
    return results


def encode_artifacts(artifacts):
    return {name: base64.b64encode(raw).decode() for name, raw in artifacts.items()}


def decode_artifacts(encoded):
    return {name: base64.b64decode(raw, validate=True) for name, raw in encoded.items()}

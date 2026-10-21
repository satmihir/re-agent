"""Allocated file-path arguments in typed calls, never provider-native strings."""
import re

from parity.observe import child, lex, replace_spans, string_spans


FIELDS = {
    'list_files': {'path', 'offset', 'limit'},
    'read_file': {'path', 'start_line', 'max_lines'},
    'search_text': {'path', 'paths', 'query', 'regex', 'max_results'},
    'write_file': {'path', 'content', 'expected_sha256'},
    'edit_file': {'path', 'expected_sha256', 'old_text', 'new_text', 'edits', 'append_text'},
    'delete_file': {'path', 'expected_sha256', 'recursive'},
}


def call_shape(call):
    return (isinstance(call, dict) and set(call) == {'call_id', 'name', 'arguments'}
            and all(isinstance(value, str) for value in call.values()) and bool(call['call_id']))


def calls(response, prefix):
    if not isinstance(response, dict) or not isinstance(response.get('blocks'), list):
        return
    for index, block in enumerate(response['blocks']):
        if (isinstance(block, dict) and set(block) == {'kind', 'call'}
                and block['kind'] == 'tool_call' and call_shape(block['call'])):
            yield prefix + ('blocks', index, 'call'), block['call']


class Arguments:
    def __init__(self, normal, events, case_id):
        self.normal = normal
        self.owners = {}
        if not re.fullmatch(r'T\d{2}\..+', case_id):
            return
        for event in events:
            if event.get('type') != 'model.accepted':
                continue
            for _, call in calls(event.get('data'), ('data',)):
                if call['name'] not in FIELDS:
                    continue
                key = (event.get('run_id'), call['call_id'])
                if key in self.owners:
                    raise AssertionError('allocated path argument has ambiguous accepted call ownership')
                self.owners[key] = (event.get('step'), event.get('seq'), call)

    def replacement(self, call):
        raw = call['arguments'].encode()
        try:
            top = lex(raw)
        except (ValueError, UnicodeError):
            return raw
        if not isinstance(top.value, dict) or not set(top.value) <= FIELDS[call['name']]:
            return raw
        # The recorded allocation is independent of the supplied path. A textual
        # prefix collision must not turn an unrelated absolute path into a slot.
        changes = []
        for field, token in top.children:
            if field == 'path':
                tokens = [token]
            elif field == 'paths' and call['name'] == 'search_text' and isinstance(token.value, list):
                if any(not isinstance(value, str) for value in token.value):
                    return raw
                tokens = [item for _, item in token.children]
            else:
                continue
            for item in tokens:
                value = item.value
                if not isinstance(value, str):
                    return raw
                root = self.normal.root
                if value == root or value.startswith(root + '/'):
                    replacement = string_spans(raw[item.start:item.end], value, '<ROOT>' + value[len(root):])
                    changes.append((item.start, item.end, replacement))
        return replace_spans(raw, changes)

    def trace(self, raw, normalized, artifact):
        if not self.owners:
            return normalized
        event = lex(raw).value
        locations = []
        if event.get('type') == 'model.accepted':
            locations.extend(calls(event.get('data'), ('data',)))
        elif event.get('type') == 'tool.started':
            data = event.get('data', {})
            if set(data) == {'call_id', 'name', 'arguments', 'workspace'}:
                call = {key: data[key] for key in ('call_id', 'name', 'arguments')}
                if call_shape(call):
                    locations.append((('data',), call))
        elif event.get('type') == 'model.requested':
            for index, entry in enumerate(event.get('data', {}).get('history') or []):
                if isinstance(entry, dict) and set(entry) == {'kind', 'assistant'} and entry['kind'] == 'assistant':
                    locations.extend(calls(entry['assistant'], ('data', 'history', index, 'assistant')))
        before, after = lex(raw), lex(normalized)
        changes = []
        for location, call in locations:
            owner = self.owners.get((event.get('run_id'), call['call_id']))
            if not owner:
                continue
            step, seq, accepted = owner
            if call != accepted:
                raise AssertionError('allocated path argument echo differs from accepted typed call')
            if event['type'] == 'tool.started' and (event.get('step') != step or event.get('seq', 0) <= seq):
                raise AssertionError('allocated path argument start does not follow its accepted call')
            replacement = self.replacement(call)
            if replacement == call['arguments'].encode():
                continue
            path = location + ('arguments',)
            left, right = before, after
            for key in path:
                left, right = child(left, key), child(right, key)
            if left.value != right.value:
                raise AssertionError('allocated path argument changed before typed normalization')
            encoded = string_spans(normalized[right.start:right.end], left.value, replacement.decode())
            self.normal.record(artifact, path, normalized[right.start:right.end], encoded,
                               'tool-allocated-path-argument', right.start, right.end)
            changes.append((right.start, right.end, encoded))
        return replace_spans(normalized, changes)

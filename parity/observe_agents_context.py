"""Typed agent evidence; no payload masks or executable-output projections."""

import json
import re


TASK = re.compile(r'(?:^|\n)CHILD_([a-z0-9-]+)(?:\n|$)')


def actor(raw):
    """Identify the last explicit agent task, never a task quoted in history data."""
    body = json.loads(raw)
    found = 'root'
    for item in body.get('input', body.get('messages', [])):
        if item.get('role') != 'user':
            continue
        for part in item.get('content', []):
            text = part.get('text', '')
            # Only the first task line, optionally following the native task header,
            # identifies a child. Historical projections never start this way.
            if text.startswith('# Agent task\n'):
                text = text.split('Context brief and live workspace paths', 1)[0]
            elif not text.startswith('CHILD_'):
                continue
            matches = TASK.findall(text)
            if matches:
                found = matches[-1]
    return found


def trace_events(ctx, agent=None, kind=None):
    """Read complete root/child traces and filter by their typed envelope identity."""
    result = []
    for path in ctx.trace_paths:
        for line in path.read_bytes().splitlines():
            event = json.loads(line)
            if agent is not None and event.get('agent_id', 'root') != agent:
                continue
            if kind is None or event['type'] == kind:
                result.append(event)
    return result


def outcomes(ctx, agent='root'):
    """Return one outcome per accepted call ID, rejecting duplicate execution."""
    result = {}
    for event in trace_events(ctx, agent, 'tool.finished'):
        data = event['data']
        if data['call_id'] in result:
            raise AssertionError('accepted call executed more than once')
        result[data['call_id']] = data['outcome']
    return result


def requests(ctx, agent='root'):
    """Return logical requests from the matching Session's trace, not response scripts."""
    return [event['data'] for event in trace_events(ctx, agent, 'model.requested')]


def check_delivery(ctx, count, agent='root'):
    """Require once-only source:agent entries after all tool results in their batch."""
    deliveries = trace_events(ctx, agent, 'agent.result')
    assert len(deliveries) == count, (len(deliveries), count)
    identities = [(item['data']['id'], item['data']['run_id']) for item in deliveries]
    assert len(identities) == len(set(identities)), identities
    events = trace_events(ctx, agent)
    for delivery in deliveries:
        earlier = [item for item in events if item['seq'] < delivery['seq']
                   and item['run_id'] == delivery['run_id']
                   and item.get('step') == delivery.get('step')]
        accepted = [item for item in earlier if item['type'] == 'model.accepted']
        if accepted:
            calls = [part['call']['call_id'] for part in accepted[-1]['data']['blocks']
                     if part['kind'] == 'tool_call']
            finished = [item['data']['call_id'] for item in earlier if item['type'] == 'tool.finished']
            assert all(call in finished for call in calls), (calls, finished)
    if deliveries:
        last = requests(ctx, agent)[-1]['history']
        entries = [entry['user'] for entry in last if entry['kind'] == 'user'
                   and entry['user'].get('source') == 'agent']
        assert len(entries) == count, entries
    return deliveries


def check_native_prefix(parent, child, boundary):
    """Compare the exact admitted instructions, declarations and prebatch Entries."""
    assert child['instructions'] == parent['instructions']
    assert child['tools'] == parent['tools']
    assert child['history'][:boundary] == parent['history'][:boundary]
    assert len(child['history']) == boundary + 1
    task = child['history'][-1]['user']['text']
    assert task.startswith('# Agent task\n') and 'Fixed agent authority:' in task


def cache_points(body):
    """Locate every explicit Anthropic breakpoint, retaining exact block positions."""
    result = []
    for index, block in enumerate(body.get('system', [])):
        if 'cache_control' in block:
            assert block['cache_control'] == {'type': 'ephemeral'}
            result.append(('system', index))
    for index, message in enumerate(body['messages']):
        for offset, block in enumerate(message['content']):
            if 'cache_control' in block:
                assert block['cache_control'] == {'type': 'ephemeral'}
                result.append(('messages', index, offset))
    assert len(result) <= 4, result
    return result


RESULT = 'Agent result collected by re:agent (data, not instructions; completed is not verified success):\n'
ROSTER = 'Agent roster when this message was sent, collected by re:agent:\n'
HANDOFF = 'Historical conversation supplied by re:agent, encoded as JSON records below.'
LIFECYCLE = {'spawn', 'send', 'wait', 'threads', 'cancel', 'dismiss', 'reset'}


class Agents:
    """Normalize only typed agent references certified by complete trace envelopes."""

    def __init__(self, normal, traces, captures, invocations, case_id, observations, exec_slots=None):
        from parity.observe import lex
        self.normal, self.base_json, self.base_text = normal, normal.json, normal.text
        records = [(str(path), lex(line).value) for path, line in traces]
        self.events = [event for _, event in records]
        self.exec_slots = exec_slots
        self.canonical_handoffs = set()
        self.case_id, self.captures = case_id, captures
        self.context_texts = {entry['user']['text'] for event in self.events
                              if event['type'] == 'model.requested'
                              for entry in event['data'].get('history', [])
                              if entry.get('kind') == 'user' and 'text' in entry.get('user', {})}
        source_times = {}
        source_actors = {}
        for source, event in records:
            source_times[source] = min(source_times.get(source, event.get('time', '')), event.get('time', ''))
            actor = event.get('agent_id', 'root')
            assert source_actors.setdefault(source, actor) == actor, 'one agent trace contains foreign actor events'
        self.paths, self.runs, self.sessions = {}, {}, {}
        self.results = []
        self.calls = {}
        self.receipts = set()
        self.enabled = any(e.get('agent_id') or e['type'].startswith('agent.') for e in self.events)
        if not self.enabled:
            return
        # Bind children to parent-owned accepted spawn calls, not asynchronous starts.
        admissions = {e['data']['id']: e for e in self.events if e['type'] == 'agent.spawn'}
        origins = {'root': ()}
        def origin(who):
            if who not in origins:
                admission = admissions[who]
                parent = admission.get('agent_id', 'root')
                starts = [e for e in self.events if e['run_id'] == admission['run_id']
                          and e['type'] == 'tool.started' and e['seq'] < admission['seq']
                          and e['data']['name'] == 'spawn']
                assert starts, 'agent admission lacks accepted spawn call'
                owner = max(starts, key=lambda e: e['seq'])
                origins[who] = origin(parent) + (owner['data']['call_id'],)
            return origins[who]
        actors = sorted({e.get('agent_id', 'root') for e in self.events}, key=origin)
        self.actor_slots = {who: index for index, who in enumerate(actors)}
        self.trace_paths = []
        for who in actors:
            own = sorted((e for e in self.events if e.get('agent_id', 'root') == who),
                         key=lambda e: (e.get('time', ''), e['seq']))
            for event in own:
                for field, kind in [('session_id', 'session'), ('run_id', 'run')]:
                    value = event[field]
                    if not re.fullmatch('[0-9a-f]{16}', value):
                        raise AssertionError('agent trace has malformed envelope identity')
                    normal.identity(value, kind)
                self.sessions.setdefault(who, set()).add(event['session_id'])
                self.runs.setdefault(who, set()).add(event['run_id'])
            sources = {source for source, actor in source_actors.items() if actor == who}
            self.paths[who] = sources
            self.trace_paths.extend(sorted(sources, key=source_times.__getitem__))
        self.capture_slots = {}
        for index, capture in enumerate(captures):
            matches = [e for e in self.events if e['type'] == 'api.attempt.started'
                       and e['data']['request_body'].encode() == capture['body']]
            if len(matches) == 1:
                owner = matches[0]
                self.capture_slots[index] = (self.actor_slots[owner.get('agent_id', 'root')],
                                             self.normal.ids['run'][owner['run_id']],
                                             owner['step'], owner['data']['attempt'])
        for event in self.events:
            data = event['data']
            if event['type'] == 'tool.finished' and data['name'] in LIFECYCLE:
                self.calls.setdefault(data['call_id'], []).append(data['outcome'])
            if event['type'] == 'agent.result':
                assert data['run_id'] in self.runs.get(data['id'], set()), 'agent result lacks child Run envelope'
                self.results.append(data)
                self.receipts.add((data['id'], event['run_id'], event['step']))
            if event['type'] == 'agent.spawn':
                assert data['parent_id'] == event.get('agent_id', 'root'), 'agent admission parent differs'
                self.validate_path(data['id'], data.get('trace_path'))
        # Link captured requests to the independently recorded exact HTTP body.
        bodies = {e['data']['request_body'].encode() for e in self.events if e['type'] == 'api.attempt.started'}
        for capture in captures:
            if capture['body'] and ('/responses' in capture['path'] or '/messages' in capture['path']):
                assert capture['body'] in bodies, 'agent HTTP capture lacks traced request'

    def validate_path(self, who, value):
        if value is not None:
            assert value in self.paths.get(who, set()), 'agent trace reference has no matching source file'

    def trace_reference(self, who, value, artifact, path):
        self.validate_path(who, value)
        normalized = self.base_text(value, artifact, path)
        if who != 'root':
            match = re.search(r'/agent-([0-9a-f]{16})/events\.jsonl$', value)
            assert match, 'agent source path lacks allocated trace directory'
            normalized = normalized.replace('/agent-' + match[1] + '/',
                                            '/agent-AGENT_%d/' % self.actor_slots[who])
        return normalized

    def json(self, raw, artifact, path=(), event=None, opaque=False, response_slot=None):
        if ('handoff' in path and path[:1] != ('provider-response',) and not opaque and response_slot is None
                and (artifact, raw) in self.canonical_handoffs):
            return raw
        if getattr(self, '_base_active', False) or path[:1] == ('provider-response',):
            return self.base_json(raw, artifact, path, event, opaque, response_slot)
        if self.exec_slots is not None and 'handoff' not in path:
            raw = self.exec_slots.rewrite(raw, artifact, path)
        if not self.enabled:
            return self.base_json(raw, artifact, path, event, opaque, response_slot)
        rewritten = self._rewrite(raw, artifact, path, opaque)
        self._base_active = True
        try:
            normalized = self.base_json(rewritten, artifact, path, event, opaque, response_slot)
            if 'handoff' in path and not opaque and response_slot is None:
                self.canonical_handoffs.add((artifact, normalized))
            return normalized
        finally:
            self._base_active = False

    def _rewrite(self, raw, artifact, path=(), opaque=False):
        from parity.observe import child, encoded_changes, go_string, lex, replace_spans, string_spans
        top = lex(raw, opaque=opaque)
        patches = []

        def patch(token, value, loc, kind, json_prefix=None):
            source = raw[token.start:token.end]
            if json_prefix is not None and token.value != value:
                assert token.value.startswith(json_prefix) and value.startswith(json_prefix)
                decoder = json.JSONDecoder()
                _, before_end = decoder.raw_decode(token.value, len(json_prefix))
                _, after_end = decoder.raw_decode(value, len(json_prefix))
                assert token.value[before_end:] == value[after_end:], 'agent embedded suffix changed'
                before = token.value[len(json_prefix):before_end].encode()
                after = value[len(json_prefix):after_end].encode()
                offset, changes = len(json_prefix.encode()), []

                def leaves(left, right):
                    if before[left.start:left.end] == after[right.start:right.end]:
                        return
                    assert [k for k, _ in left.children] == [k for k, _ in right.children], 'agent embedded shape/order changed'
                    if left.children:
                        for (_, a), (_, b) in zip(left.children, right.children):
                            leaves(a, b)
                        return
                    changes.append((offset + left.start, offset + left.end, after[right.start:right.end]))

                leaves(lex(before), lex(after))
                replacement = encoded_changes(source, changes)
            else:
                replacement = string_spans(source, token.value, value) if isinstance(value, str) else go_string(value)
            if replacement != raw[token.start:token.end]:
                self.normal.record(artifact, path + loc, raw[token.start:token.end], replacement,
                                   kind, token.start, token.end)
                patches.append((token.start, token.end, replacement))

        def identity(token, loc, who=None):
            value = token.value
            known = self.runs.get(who, set()) if who else set().union(*self.runs.values())
            assert value in known, 'agent reference has no matching Run envelope'
            patch(token, self.normal.ids['run'][value], loc, 'agent-envelope-run-reference')

        def thread(token, loc):
            obj = token.value
            if not isinstance(obj, dict) or 'id' not in obj:
                return
            who = obj['id']
            if 'trace_path' in obj:
                source = child(token, 'trace_path')
                self.validate_path(who, source.value)
                patch(source, self.trace_reference(who, source.value, artifact, path + loc + ('trace_path',)),
                      loc + ('trace_path',), 'agent-source-trace-path')
            if 'delivery_run_id' in obj:
                assert (who, obj['delivery_run_id'], obj['delivery_step']) in self.receipts, 'agent receipt lacks delivery'
                identity(child(token, 'delivery_run_id'), loc + ('delivery_run_id',))

        def result(token, loc):
            obj = token.value
            assert obj in self.results, 'agent delivery differs from witnessed agent.result'
            identity(child(token, 'run_id'), loc + ('run_id',), obj['id'])
            summary = child(token, 'result')
            if 'trace_path' in summary.value:
                source = child(summary, 'trace_path')
                self.validate_path(obj['id'], source.value)
                patch(source, self.trace_reference(obj['id'], source.value, artifact, path + loc + ('result', 'trace_path')),
                      loc + ('result', 'trace_path'), 'agent-result-source-trace-path')
            effects(child(token, 'effects'), loc + ('effects',))

        def effects(token, loc):
            # RunResult carries exact per-call agent provenance; summaries do not.
            if isinstance(token.value, list):
                for key, item in token.children:
                    if isinstance(item.value, dict) and 'run_id' in item.value and 'agent_id' in item.value:
                        identity(child(item, 'run_id'), loc + (key, 'run_id'), item.value['agent_id'])
            elif isinstance(token.value, dict):
                for key, item in token.children:
                    if key == 'files':
                        for index, record in item.children:
                            if 'workspace' in record.value:
                                source = child(record, 'workspace')
                                patch(source, self.base_text(source.value, artifact, path + loc + (key, index, 'workspace')),
                                      loc + (key, index, 'workspace'), 'agent-delivered-effect-workspace')
                    if key in ('origins', 'origin', 'records'):
                        effects(item, loc + (key,))
                if 'run_id' in token.value and 'agent_id' in token.value:
                    identity(child(token, 'run_id'), loc + ('run_id',), token.value['agent_id'])

        def outcome(token, loc):
            if not token.value.get('ok') or 'data' not in token.value:
                return
            data = child(token, 'data')
            if isinstance(data.value, list):
                for key, item in data.children:
                    thread(item, loc + ('data', key))
            elif isinstance(data.value, dict):
                thread(data, loc + ('data',))
                for key in ('statuses', 'agents', 'threads'):
                    if key in data.value:
                        for index, item in child(data, key).children:
                            thread(item, loc + ('data', key, index))

        def preview(full, visible, loc, mode):
            from parity.observe import go_json
            original = go_json(full)
            prefix = visible.encode()
            assert len(prefix) <= 512 and original.startswith(prefix), 'agent router preview lacks exact source prefix'
            if mode == 'outcome-data':
                rewritten = self._rewrite(go_json({'ok': True, 'data': full}), artifact, path + loc + ('outcome',))
                token = child(lex(rewritten), 'data')
                rewritten = rewritten[token.start:token.end]
            else:
                rewritten = self._rewrite(original, artifact, path + loc + (mode,))
            changes = []
            def leaves(left, right, location=()):
                if left.children:
                    assert [k for k, _ in left.children] == [k for k, _ in right.children]
                    for (key, a), (_, b) in zip(left.children, right.children):
                        leaves(a, b, location + (key,))
                elif left.end <= len(prefix) and original[left.start:left.end] != rewritten[right.start:right.end]:
                    changes.append((left.start, left.end, rewritten[right.start:right.end]))
                elif (self.case_id == 'W39.agent' and mode == 'result'
                      and location == ('result', 'trace_path') and left.start < len(prefix) < left.end):
                    # A router preview can end exactly at the witnessed directory's slash.
                    source = full['result']['trace_path']
                    self.validate_path(full['id'], source)
                    directory = go_string(source.removesuffix('events.jsonl'))[:-1]
                    if source.endswith('/events.jsonl') and prefix[left.start:] == directory:
                        normalized = self.trace_reference(full['id'], source, artifact, path + loc + location)
                        changes.append((left.start, len(prefix), go_string(normalized.removesuffix('events.jsonl'))[:-1]))
            leaves(lex(original), lex(rewritten))
            return replace_spans(prefix, changes).decode()

        def embedded(token, loc, prefix, role):
            text = token.value
            before, payload = text.split(prefix, 1)
            decoder = json.JSONDecoder()
            try:
                obj, stop = decoder.raw_decode(payload)
            except json.JSONDecodeError:
                from parity.observe import go_json
                sources = [value for value in self.results if go_json(value).startswith(payload.encode())]
                assert role == 'result' and len(sources) == 1 and len(text.encode()) <= 512, 'partial agent data lacks unique delivery witness'
                normalized = preview(sources[0], payload, loc, role)
                patch(token, before + prefix + normalized, loc, 'agent-exact-delivery-preview')
                return
            if role == 'result':
                assert obj in self.results, 'source:agent JSON differs from witnessed delivery'
            normalized = self._rewrite(payload[:stop].encode(), artifact, path + loc + (role,)).decode()
            patch(token, before + prefix + normalized + payload[stop:], loc, 'agent-embedded-' + role,
                  json_prefix=before + prefix)

        def walk(token, loc, user_text=False):
            obj = token.value
            if isinstance(obj, dict):
                if obj.get('role') == 'assistant' or obj.get('kind') == 'assistant' or obj.get('type') in ('tool_use', 'function_call'):
                    return
                user_text = user_text or obj.get('role') == 'user' or obj.get('kind') == 'user'
                if loc == () and obj.get('type') == 'run.finished' and 'trace_path' in obj['data']:
                    source = child(child(token, 'data'), 'trace_path')
                    patch(source, self.trace_reference(obj.get('agent_id', 'root'), source.value,
                          artifact, path + ('data', 'trace_path')), ('data', 'trace_path'), 'agent-run-source-trace-path')
                if loc == () and obj.get('type') == 'agent.result':
                    result(child(token, 'data'), ('data',))
                    return
                if loc == () and obj.get('type') in ('agent.spawn', 'agent.send'):
                    thread(child(token, 'data'), ('data',))
                    return
                if loc == () and obj.get('type') == 'tool.finished' and obj['data']['name'] in LIFECYCLE:
                    outcome(child(child(token, 'data'), 'outcome'), ('data', 'outcome'))
                    return
                if obj.get('kind') == 'tool' and obj.get('tool', {}).get('call_id') in self.calls:
                    item = child(token, 'tool')
                    actual = obj['tool']['outcome']
                    if actual not in self.calls[obj['tool']['call_id']]:
                        assert actual.get('truncated') and isinstance(actual.get('data'), str), 'historical agent tool result differs'
                        from parity.observe import go_json
                        candidates = [v for v in self.calls[obj['tool']['call_id']]
                                      if go_json(v.get('data')).startswith(actual['data'].encode())]
                        assert len(candidates) == 1, 'agent tool preview lacks unique original outcome'
                        source = child(child(item, 'outcome'), 'data')
                        normalized = preview(candidates[0]['data'], actual['data'], loc, 'outcome-data')
                        patch(source, normalized, loc + ('tool', 'outcome', 'data'), 'agent-exact-tool-preview')
                        return
                    outcome(child(item, 'outcome'), loc + ('tool', 'outcome'))
                    return
                if set(('id', 'run_id', 'result', 'effects')) <= set(obj) and path[-1:] == ('result',):
                    result(token, loc)
                    return
                if obj.get('kind') == 'agent_roster' and 'threads' in obj:
                    for key, item in child(token, 'threads').children:
                        thread(item, loc + ('threads', key))
                    return
                if obj.get('type') in ('function_call_output', 'tool_result'):
                    key = 'output' if obj['type'] == 'function_call_output' else 'content'
                    call_id = obj.get('call_id', obj.get('tool_use_id'))
                    if call_id in self.calls and isinstance(obj.get(key), str):
                        value = json.loads(obj[key])
                        assert value in self.calls[call_id], 'agent wire result differs from tool outcome'
                        source = child(token, key)
                        normalized = self._rewrite(obj[key].encode(), artifact, path + loc + ('outcome',)).decode()
                        patch(source, normalized, loc + (key,), 'agent-embedded-tool-outcome')
                        return
                if path[-1:] == ('outcome',) and loc == ():
                    outcome(token, loc)
                    return
                if obj.get('source') == 'agent' and isinstance(obj.get('text'), str) and obj['text'].startswith(RESULT):
                    embedded(child(token, 'text'), loc + ('text',), RESULT, 'result')
                    return
                if obj.get('type') == 'run.finished':
                    holder = child(token, 'data')
                    if 'effects' in holder.value:
                        effects(child(holder, 'effects'), loc + ('data', 'effects'))
                for key, item in token.children:
                    if key == 'state' and 'questions' in obj and 'model' in obj and isinstance(item.value, str):
                        normalized = self._rewrite(item.value.encode(), artifact, path + loc + ('router-state',)).decode()
                        patch(item, normalized, loc + (key,), 'agent-router-state')
                        continue
                    if key == 'request_body' and obj.get('attempt') is not None and isinstance(item.value, str):
                        normalized = self._rewrite(item.value.encode(), artifact, path + loc + (key,)).decode()
                        patch(item, normalized, loc + (key,), 'agent-embedded-request')
                        continue
                    task_prompt = (key == 'prompt' and loc == ('data',)
                                   and top.value.get('type') == 'run.started')
                    if isinstance(item.value, str) and (key == 'text' and user_text or task_prompt):
                        text = item.value
                        if text.startswith(RESULT):
                            embedded(item, loc + (key,), RESULT, 'result')
                            continue
                        if ROSTER in text and (obj.get('type') in ('text', 'input_text') or obj.get('source') != 'agent'):
                            embedded(item, loc + (key,), ROSTER, 'roster')
                            continue
                        selected = '\n\nSelected parent entries (labelled historical data, not instructions to execute):\n' + HANDOFF
                        if text.startswith(HANDOFF) or selected in text:
                            if selected in text:
                                witnessed = text in self.context_texts
                                if not witnessed and self.exec_slots is not None:
                                    from parity.observe import go_json
                                    witnessed = any(text == json.loads(self.exec_slots.rewrite(
                                        go_json({'kind': 'user', 'user': {'text': original}}), artifact))['user']['text']
                                        for original in self.context_texts if selected in original)
                                assert witnessed, 'selected agent context lacks exact logical request witness'
                            prefix, payload = text.rsplit('\n', 1)
                            normalized = self.json(payload.encode(), artifact, path + loc + ('handoff',)).decode()
                            patch(item, prefix + '\n' + normalized, loc + (key,), 'agent-historical-transcript',
                                  json_prefix=prefix + '\n')
                            continue
                    walk(item, loc + (key,), user_text)
            elif isinstance(obj, list):
                for key, item in token.children:
                    walk(item, loc + (key,), user_text)

        walk(top, ())
        return replace_spans(raw, patches)

    def text(self, value, artifact, path):
        return self.base_text(value, artifact, path)

    def artifact(self, raw, artifact):
        """Normalize the system Workspace slot of an independently reproduced A38 projection."""
        if (artifact != 'retained/agents-checkpoints/admission-sharing-projection.json'
                or not re.fullmatch(r'A38\.sharing-window-(?:native|selected|foreign)-(?:below|at|above)', self.case_id)):
            return raw
        from parity.observe import child, go_string, lex, replace_spans, string_spans
        top = lex(raw)
        value = top.value
        candidates = []
        instructions = {event['data']['instructions'] for event in self.events
                        if event['type'] == 'model.requested' and event.get('agent_id', 'root') == 'root'
                        and 'instructions' in event['data']}
        # The fixture's witness invocation sends parent, child, parent requests;
        # the boundary invocation can resend an identical child body afterward.
        for capture in self.captures[1:2]:
            if not capture.get('body'):
                continue
            try:
                body = json.loads(capture['body'])
            except (ValueError, UnicodeError):
                continue
            if not isinstance(body, dict) or not isinstance(body.get('input'), list) or not body['input']:
                continue
            content = body['input'][-1].get('content', [])
            rosters = [part for part in content if isinstance(part.get('text'), str) and part['text'].startswith(ROSTER)]
            if len(rosters) != 1:
                continue
            rosters[0]['text'] = ROSTER + '{"kind":"agent_roster","threads":[]}'
            choices = [body.get('instructions')] if '-native-' in self.case_id else instructions
            for prompt in choices:
                body['instructions'] = prompt
                if body == value:
                    candidates.append(capture)
        assert len(candidates) == 1, 'agent sharing projection lacks unique independent request witness'
        source = child(lex(raw), 'instructions')
        workspace = self.normal.root + '/workspace'
        witnesses = [event for event in self.events if event['type'] == 'run.started'
                     and event.get('data', {}).get('workspace') == workspace]
        assert witnesses, 'agent sharing projection lacks owned workspace envelope'
        line = '\nWorkspace: ' + workspace + '\n'
        assert source.value.count(line) == 1, 'agent sharing projection Workspace line differs'
        normalized = source.value.replace(line, '\nWorkspace: <ROOT>/workspace\n')
        replacement = string_spans(raw[source.start:source.end], source.value, normalized)
        self.normal.record(artifact, ('instructions',), raw[source.start:source.end], replacement,
                           'agent-sharing-projection-workspace', source.start, source.end)
        changes = [(source.start, source.end, replacement)]
        source_input = child(top, 'input')
        normalized_input = self.json(raw[source_input.start:source_input.end], artifact, ('input',))
        changes.append((source_input.start, source_input.end, normalized_input))
        if 'prompt_cache_key' in value:
            source_key = child(top, 'prompt_cache_key')
            assert source_key.value in self.normal.cache_keys, 'agent projection cache key lacks logical request witness'
            normalized_key = go_string(self.normal.cache_keys[source_key.value])
            self.normal.record(artifact, ('prompt_cache_key',), raw[source_key.start:source_key.end], normalized_key,
                               'independently-validated-cache-key', source_key.start, source_key.end)
            changes.append((source_key.start, source_key.end, normalized_key))
        return replace_spans(raw, changes)

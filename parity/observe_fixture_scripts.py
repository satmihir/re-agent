"""Exact fixture-authored script paths certified by checkpoint and dispatch evidence."""

import json

from parity.observe import child, encoded_changes, lex, replace_spans
from parity.observe_agents_checkpoints import validate


class Scripts:
    def __init__(self, normal, events, case_id, observations):
        self.normal, self.base_json = normal, normal.json
        self.arguments = None
        self.commands = []
        if case_id not in ('C20.persistence-barriers', 'C20.pre-model-save', 'C20.pre-tool-save'):
            return
        state = validate(observations['agents-checkpoints/synthetic-input.checkpoint.json'])
        assert state['id'] == '0123456789abcdef', 'script checkpoint identity differs'
        # Context owns this fixed HOME allocation; replay retains its case root.
        # Do not infer allocations from the script that is about to be rewritten.
        home = normal.root + '/home'
        self.source = "from pathlib import Path; p=Path(%r); p.unlink(); p.mkdir()" % (
            home + '/.cache/reagent/sessions/' + state['id'] + '/checkpoint.json')
        self.arguments = dict(argv=['python3', '-c', self.source], cwd='.')
        self.program = normal.root + '/home/.parity-bin/python3'
        starts = [event for event in events if event['type'] == 'tool.started'
                  and event['data'].get('call_id') == 'barrier-helper']
        assert len(starts) == 1, 'script lacks one accepted dispatch witness'
        start = starts[0]
        assert start['data']['name'] == 'exec' and json.loads(start['data']['arguments']) == self.arguments, 'script differs from authored helper'
        assert start['data']['workspace'] == state['active'] == normal.root + '/workspace', 'script dispatch workspace differs'
        for event in events:
            if (event['type'] == 'tool.finished' and event['run_id'] == start['run_id']
                    and event['data'].get('call_id') == 'barrier-helper'):
                command = event['data']['outcome'].get('data')
                assert isinstance(command, dict) and command.get('argv') == self.arguments['argv'], 'script outcome argv differs'
                assert command.get('resolved_executable') == self.program, 'script executable differs from Context isolated PATH allocation'
                self.commands.append(command)

    def json(self, raw, artifact, path=(), event=None, opaque=False, response_slot=None):
        if self.arguments is None:
            return self.base_json(raw, artifact, path, event, opaque, response_slot)
        changes = []

        def patch(token, replacement, loc, kind='fixture-owned-script-path'):
            self.normal.record(artifact, path + loc, raw[token.start:token.end], replacement,
                               kind, token.start, token.end)
            changes.append((token.start, token.end, replacement))

        top = lex(raw, opaque=opaque)
        calls = []
        if isinstance(top.value, dict):
            if top.value.get('type') == 'tool.started':
                calls.append((child(top, 'data'), ('data',)))
            elif top.value.get('type') == 'model.accepted':
                data = child(top, 'data')
                for index, block in child(data, 'blocks').children:
                    if block.value.get('kind') == 'tool_call':
                        calls.append((child(block, 'call'), ('data', 'blocks', index, 'call')))
                native = data.value.get('native')
                if isinstance(native, dict) and native.get('provider') == 'openai.responses':
                    for index, item in child(child(data, 'native'), 'items').children:
                        if item.value.get('type') == 'function_call':
                            calls.append((item, ('data', 'native', 'items', index)))
            elif path == ('provider-response',) and isinstance(top.value.get('output'), list):
                for index, item in child(top, 'output').children:
                    if item.value.get('type') == 'function_call':
                        calls.append((item, ('output', index)))
            elif top.value.get('type') == 'tool.finished':
                data = child(top, 'data')
                if data.value.get('call_id') == 'barrier-helper' and data.value.get('name') == 'exec':
                    command = child(child(data, 'outcome'), 'data')
                    assert command.value in self.commands, 'script outcome differs from independent command witness'
                    script = child(child(command, 'argv'), 2)
                    start = len(self.source[:self.source.index(self.normal.root)].encode())
                    encoded = encoded_changes(raw[script.start:script.end],
                                              [(start, start + len(self.normal.root.encode()), b'<ROOT>')])
                    patch(script, encoded, ('data', 'outcome', 'data', 'argv', 2))
                    program = child(command, 'resolved_executable')
                    encoded = encoded_changes(raw[program.start:program.end],
                                              [(0, len(self.normal.root.encode()), b'<ROOT>')])
                    patch(program, encoded, ('data', 'outcome', 'data', 'resolved_executable'),
                          'fixture-owned-executable-path')
        for token, loc in calls:
            obj = token.value
            if obj.get('call_id') != 'barrier-helper' or obj.get('name') != 'exec':
                continue
            source = child(token, 'arguments')
            arguments = lex(source.value.encode())
            assert arguments.value == self.arguments, 'embedded script arguments differ from dispatch witness'
            text = child(child(arguments, 'argv'), 2)
            literal = source.value.encode()[text.start:text.end]
            start = len(self.source[:self.source.index(self.normal.root)].encode())
            encoded = encoded_changes(literal, [(start, start + len(self.normal.root.encode()), b'<ROOT>')])
            # Lift only this byte delta, not its containing JSON scalar, through
            # the outer arguments string so both escaping layers stay exact.
            left = 0
            while left < min(len(literal), len(encoded)) and literal[left] == encoded[left]:
                left += 1
            suffix = 0
            while (suffix < min(len(literal), len(encoded)) - left
                   and literal[-suffix - 1] == encoded[-suffix - 1]):
                suffix += 1
            stop = len(literal) - suffix
            replacement_stop = len(encoded) - suffix
            outer = encoded_changes(raw[source.start:source.end],
                                    [(text.start + left, text.start + stop, encoded[left:replacement_stop])])
            patch(source, outer, loc + ('arguments',))
        return self.base_json(replace_spans(raw, changes), artifact, path, event, opaque, response_slot)

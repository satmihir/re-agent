"""Checkpoint-only byte substitutions, bound to retained capture witnesses.

Install after trace/display identity discovery. Call artifact for retained
agents-checkpoints{,-imports}/*.checkpoint.json files; never send them through generic JSON
normalization first. Construction and replay require no live filesystem reads.
"""
import base64
import binascii
from pathlib import Path
import re

from parity.observe import child, lex, replace_spans, sha256
from parity.observe_agents_checkpoints import USAGE, encode, opaque, validate


FIXED_ID = '0123456789abcdef'
FROZEN_V1_SHA256 = 'c259b6393f7b8bb82e25d82781dfe7211d4cdad4ac62bb18662c40a5f37b07b6'
HANDOFF = 'Historical conversation supplied by re:agent, encoded as JSON records below.'


def owns_artifact(name):
    """Identify retained checkpoint bytes that must not also compare as raw B."""
    return name.startswith(('agents-checkpoints/', 'agents-checkpoints-imports/')) and name.endswith('.checkpoint.json')


class Checkpoints:
    def __init__(self, normal, ctx, traces=(), agents=None):
        self.normal = normal
        self.case_id = {'C19.corrupt-checksum': 'A37.corrupt-checksum'}.get(Path(ctx.root).name, Path(ctx.root).name)
        self.agents = agents if agents is not None and agents.enabled and (self.case_id.startswith('A34.') or self.case_id == 'W10.source') else None
        self.roots = {str(ctx.root): '<ROOT>'}
        workspace = str(getattr(ctx, 'workspace', ctx.root))
        if workspace != str(ctx.root):
            self.roots[workspace] = '<ROOT>' + workspace.removeprefix(str(ctx.root)) if workspace.startswith(str(ctx.root) + '/') else '<WORKSPACE>'
        artifact_root = getattr(ctx, 'artifact_root', getattr(ctx, 'artifacts', None))
        if artifact_root is not None and str(artifact_root) != str(ctx.root):
            self.roots[str(artifact_root)] = '<ARTIFACTS>'
        traces = list(traces)
        paths = list(dict.fromkeys(str(path) for path, _ in traces))
        originals = getattr(ctx, 'checkpoint_trace_paths', None)
        if originals is not None:
            assert isinstance(originals, list) and len(originals) == len(paths), 'checkpoint trace allocation count'
            assert all(isinstance(path, str) for path in originals), 'checkpoint trace allocations must be strings'
            assert len(set(originals)) == len(originals), 'duplicate checkpoint trace allocations'
            for path in originals:
                assert isinstance(path, str) and Path(path).is_absolute() and str(Path(path)) == path and '..' not in Path(path).parts, 'invalid checkpoint trace allocation'
                assert any(path.startswith(root + '/') for root in self.roots), 'checkpoint trace allocation escaped capture roots'
            allocations = dict(zip(paths, originals))
        else:
            allocations = dict(zip(paths, paths))
        self.trace_paths = set(allocations.values())
        for path, raw in traces:
            event = lex(raw).value
            for key, kind in (('session_id', 'session'), ('run_id', 'run')):
                if key in event:
                    normal.identity(event[key], kind)
            if event.get('type') == 'run.finished' and event.get('data', {}).get('trace_path'):
                observed = event['data']['trace_path']
                if originals is not None:
                    assert observed == allocations[str(path)], 'checkpoint trace allocation disagrees with trace envelope'
                self.trace_paths.add(observed)
        for invocation in getattr(ctx, 'invocations', ()):
            trace = invocation.get('trace')
            if trace is not None:
                self.trace_paths.add(str(trace))
            normal.register_display_sessions(invocation.get('stderr', b'').decode('utf-8', 'surrogateescape'))
        self.json_normalizer = normal.json

    def _path(self, value, trace=False):
        if not isinstance(value, str):
            return value
        if trace and value not in self.trace_paths:
            return value
        result = value
        owned = False
        for root in sorted(self.roots, key=len, reverse=True):
            if value == root or value.startswith(root + '/'):
                result = self.roots[root] + value[len(root):]
                owned = True
                break
        if not owned and not trace:
            return value
        # Only path components, never arbitrary strings containing an identity.
        for kind, mapping in self.normal.ids.items():
            for identity, replacement in mapping.items():
                if kind != 'session' or identity != FIXED_ID:
                    result = re.sub(r'(?<=/)' + re.escape(identity) + r'(?=/|\.|$)', replacement, result)
        return result

    def _trace(self, value, name, path, who=None):
        if self.agents is not None:
            if who is not None:
                return self.agents.trace_reference(who, value, name, path)
            owners = [actor for actor, sources in self.agents.paths.items() if value in sources]
            if owners:
                assert len(owners) == 1, 'checkpoint trace has ambiguous source owner'
                return self.agents.trace_reference(owners[0], value, name, path)
        return self._path(value, trace=True)

    def _instructions(self, value):
        if not isinstance(value, str) or '\n# Runtime\n\n' not in value:
            return value
        prefix, runtime = value.split('\n# Runtime\n\n', 1)
        runtime, marker, project = runtime.partition('\n# Project instructions (AGENTS.md)\n\n')
        lines = runtime.splitlines(keepends=True)
        for index, line in enumerate(lines):
            if line.startswith('Workspace: '):
                ending = '\n' if line.endswith('\n') else ''
                lines[index] = 'Workspace: ' + self._path(line[len('Workspace: '):].removesuffix('\n')) + ending
        return prefix + '\n# Runtime\n\n' + ''.join(lines) + marker + project

    def _json(self, raw, name, path):
        """Delegate only decoded typed JSON; opaque byte planes never enter here."""
        try:
            top = lex(raw)
        except (ValueError, UnicodeError):
            return raw
        if isinstance(top.value, dict) and {'type', 'seq'} <= set(top.value):
            return raw  # Persisted tool bytes cannot witness a new trace envelope.

        def unbound_scope(token):
            for key, item in token.children:
                if key == 'scope' and isinstance(item.value, dict):
                    for field, kind in (('session_id', 'session'), ('run_id', 'run')):
                        if field in item.value and item.value[field] not in self.normal.ids[kind]:
                            return True
                if unbound_scope(item):
                    return True
            return False

        if unbound_scope(top):
            return raw
        result = self.json_normalizer(raw, name, path=path)
        changes = []

        def roster(token, loc, parent=None):
            if (loc[-1:] == ('trace_path',) and isinstance(parent, dict)
                    and {'id', 'parent_id', 'state', 'task'} <= set(parent)):
                after = self._trace(token.value, name, path + loc,
                                    parent['id'] if token.value in self.trace_paths and self.agents is not None else None)
                if after != token.value:
                    replacement = encode(after)
                    self.normal.record(name, path + loc, result[token.start:token.end], replacement,
                                       'checkpoint-roster-witnessed-trace', token.start, token.end)
                    changes.append((token.start, token.end, replacement))
                return
            for key, item in token.children:
                roster(item, loc + (key,), token.value)

        roster(lex(result), ())
        return replace_spans(result, changes)

    def artifact(self, name, raw):
        if not owns_artifact(name):
            return raw
        try:
            top = lex(raw)
        except (ValueError, UnicodeError):
            if self.case_id == 'A37.corrupt-json' and raw == b'{':
                return raw
            raise AssertionError('checkpoint artifact is not JSON')
        # Legacy gob is a frozen migration input, not a substitute-able JSON state.
        if top.value.get('version') == 1:
            assert [key for key, _ in top.children] == ['version', 'id', 'state', 'sha256'], 'legacy envelope order'
            assert raw == encode(top.value), 'noncanonical legacy envelope'
            assert sha256(opaque(top.value['state'])) == top.value['sha256'], 'legacy checksum mismatch'
            if name == 'agents-checkpoints-imports/frozen-v1.checkpoint.json':
                assert sha256(raw) == FROZEN_V1_SHA256, 'frozen legacy input differs from pinned bytes'
            # Relocated gob is still exact B: a v2 migration is not an inverse
            # proof of every gob byte. Root-independent v1 fixtures need no mask.
            self.normal.record(name, (), raw, raw, 'checkpoint-legacy-exact-bytes')
            return raw

        state_token = child(top, 'state')
        state_raw = raw[state_token.start:state_token.end]
        digest = sha256(state_raw)
        # The corrupt fixture has one precisely defined delta. Validate a copy
        # with only that delta undone; never silently repair the retained input.
        damage = None
        checked = raw
        digest_token = child(top, 'sha256')
        if self.case_id == 'A37.corrupt-checksum':
            assert top.value['sha256'] == '0' + digest, 'unexpected checksum corruption delta'
            damage = 'checksum-prefix-zero'
            checked = replace_spans(raw, [(digest_token.start, digest_token.end, encode(digest))])
        elif self.case_id == 'A37.corrupt-version':
            assert top.value['version'] == 99, 'unexpected version corruption delta'
            version = child(top, 'version')
            checked = replace_spans(raw, [(version.start, version.end, b'2')])
            damage = 'envelope-version-99'
        elif self.case_id in ('A37.corrupt-envelope-id', 'A37.corrupt-state-id'):
            assert top.value['id'] == 'fedcba9876543210' and state_token.value['id'] == FIXED_ID, 'unexpected identity corruption delta'
            identity = child(top, 'id')
            checked = replace_spans(raw, [(identity.start, identity.end, encode(FIXED_ID))])
            damage = 'envelope-id-fedcba9876543210'
        validate(checked)
        # These containing structs carry paths but are not history Entry shapes.
        thread = tuple('id parent_id name state task steps cost trace_path authority context_mode effects_unknown'.split())
        shapes = {'prompt': ('text', 'project'), 'handoff': ('text', 'plan', 'entries', 'native_items'),
                  'agent_roster': thread, 'agent_subtree': ('threads', 'total_cost', 'total_agents', 'next_offset', 'accounting_complete'),
                  'threads': thread + ('depth',), 'cost': USAGE, 'total_cost': USAGE}

        def containing_order(token, shape):
            if isinstance(token.value, dict):
                if shape in shapes:
                    keys = [key for key, _ in token.children]
                    assert keys == [key for key in shapes[shape] if key in keys], ('checkpoint containing-struct order', shape)
                for key, item in token.children:
                    containing_order(item, key)
            elif isinstance(token.value, list):
                for _, item in token.children:
                    containing_order(item, shape)

        containing_order(state_token, 'state')
        changes = []

        def patch(token, value, path, kind):
            before = raw[token.start:token.end]
            after = encode(value)
            if before != after:
                changes.append((token.start, token.end, after))
                self.normal.record(name, path, before, after, kind, token.start, token.end)

        def decoded(token, path):
            try:
                data = opaque(token.value)
            except (AssertionError, ValueError, binascii.Error):
                # Deliberately invalid raw-base64 fixture remains literal.
                if self.case_id == 'A37.corrupt-raw-base64':
                    assert token.value == 'not base64!', 'unexpected base64 corruption'
                    return
                raise
            if data is None:
                return
            result = self._json(data, name, path + ('decoded',))
            if result != data:
                patch(token, base64.b64encode(result).decode(), path, 'checkpoint-typed-base64')

        def walk(token, path, parent=None):
            value = token.value
            if path in (('state', 'id'), ('id',)):
                if value != FIXED_ID and value in self.normal.ids['session']:
                    patch(token, self.normal.ids['session'][value], path, 'checkpoint-witnessed-session')
                return
            if path in (('state', 'launch'), ('state', 'active')):
                patch(token, self._path(value), path, 'checkpoint-owned-workspace')
                return
            if path == ('state', 'last_trace') or path[-1:] == ('trace_path',):
                who = parent['id'] if isinstance(parent, dict) and {'id', 'parent_id', 'state', 'task'} <= set(parent) else None
                patch(token, self._trace(value, name, path, who), path, 'checkpoint-witnessed-trace')
                return
            if (self.agents is not None and len(path) == 5 and path[:2] == ('state', 'history')
                    and type(path[2]) is int and path[-2:] == ('user', 'text')
                    and (parent.get('source') == 'agent' or (self.case_id == 'W10.source'
                         and name == 'agents-checkpoints/agent-cache-anchor-1.checkpoint.json' and path[2] == 0))):
                # W10's second anchor intentionally omits source. Preserve its
                # user Entry shape; both forms still require the exact delivery.
                wrapper = encode({'kind': 'user', 'user': parent})
                result = self._json(wrapper, name, path)
                patch(token, lex(result).value['user']['text'], path, 'checkpoint-agent-delivery')
                return
            if path == ('state', 'prompt', 'text'):
                patch(token, self._instructions(value), path, 'checkpoint-runtime-workspace')
                return
            if path == ('state', 'handoff', 'text') and isinstance(value, str) and value.startswith(HANDOFF):
                wrapper = encode({'text': value})
                result = self._json(wrapper, name, path)
                patch(token, lex(result).value['text'], path, 'checkpoint-handoff-transcript')
                return
            if path[:3] == ('state', 'gc_state', 'paths') and len(path) == 4:
                patch(token, self._path(value), path, 'checkpoint-gc-owned-path')
                return
            if path[-2:] in (('user', 'workspace'), ('user', 'roster'), ('pending_submission', 'workspace'), ('pending_submission', 'roster'), ('outcome', 'data')) or (len(path) == 5 and path[:3] == ('state', 'gc_state', 'inherited') and path[-1] == 'data'):
                decoded(token, path)
                return
            if path[-2:] == ('shell', 'duration_ms'):
                assert type(value) is int and 0 <= value <= self.normal.elapsed_bound * 1000, 'checkpoint shell duration outside capture interval'
                patch(token, 0, path, 'checkpoint-shell-duration-ms')
                return
            if path[-2:] == ('outcome', 'workspace') or (len(path) == 5 and path[:3] == ('state', 'gc_state', 'inherited') and path[-1] == 'workspace') or path[-2:] == ('shell', 'workspace'):
                patch(token, self._path(value), path, 'checkpoint-outcome-workspace')
                return
            # Native.Items, user prose, summary text, and provider arguments are
            # intentionally not inferred to be path-bearing.
            for key, item in token.children:
                walk(item, path + (key,), value)

        walk(child(top, 'id'), ('id',))
        walk(state_token, ('state',))
        substituted = replace_spans(raw, changes)
        new_state = child(lex(substituted), 'state')
        new_digest = sha256(substituted[new_state.start:new_state.end])
        if digest != new_digest:
            patch(digest_token, ('0' if damage == 'checksum-prefix-zero' else '') + new_digest, ('sha256',), 'checkpoint-derived-state-sha256')
        result = replace_spans(raw, changes)
        # Prove the exact inverse using source spans, not JSON roundtripping.
        inverse = []
        shift = 0
        for start, end, replacement in sorted(changes):
            inverse.append((start + shift, start + shift + len(replacement), raw[start:end]))
            shift += len(replacement) - (end - start)
        assert replace_spans(result, inverse) == raw, 'checkpoint substitution is not reversible'
        self.normal.record(name, (), raw, result, 'checkpoint-linkage-inverse')
        self.normal.ledger[-1].update(raw_state_sha256=digest, substituted_state_sha256=new_digest,
                                    raw_checksum_valid=damage != 'checksum-prefix-zero',
                                    corruption_delta=damage, inverse_verified=True)
        return result

"""Narrow build-metadata exception for the version command, never model replies."""

import re


def version_suffix(raw):
    """Validate stable product bytes and a nonempty, single-line build suffix."""
    match = re.fullmatch(rb'reagent ([^\x00-\x20\x7f][^\r\n\x00]*)\n', raw)
    assert match is not None, ('invalid version output', raw)
    assert match[1].strip(), 'missing build metadata'
    return match.span(1)


def version_stdout(normal, invocation, artifact):
    """Replace only the version suffix after successful invocation validation."""
    raw = invocation['stdout']
    if invocation.get('args', [])[:1] not in (['version'], ['--version']):
        return raw
    assert invocation['returncode'] == 0 and invocation['stderr'] == b''
    start, end = version_suffix(raw)
    replacement = b'<BUILD>'
    normal.record(artifact, ('build',), raw[start:end], replacement,
                  'cli-version-build-metadata', start, end)
    return raw[:start] + replacement + raw[end:]


def oversized_stderr(normal, invocation, artifact):
    """Certify the rejected request length against a same-shape public preview."""
    raw = invocation['stderr']
    probe = invocation.get('cli_oversized_probe')
    if probe is None:
        return raw
    from .observe import child, lex
    prompt = invocation['cli_oversized_prompt']
    assert prompt == b'x' * (11 << 20)
    assert probe['returncode'] == 0 and probe['stderr'] == b''
    assert invocation['returncode'] == 1 and invocation['stdout'] == b''
    assert '--show-context' in probe['args'] and '--show-context' in invocation['args']
    before = probe['stdout']
    assert before.endswith(b'\n')
    body = lex(before)
    submitted = child(child(child(child(body, 'input'), 0), 'content'), 0)
    assert child(submitted, 'text').value == 'x'
    delta = len(prompt) - 1
    expected = len(before) - 1 + delta
    match = re.fullmatch(rb'error: request is ([0-9]+) bytes, over the 10485760 byte limit; narrow the task or start a new run\n', raw)
    assert match is not None and int(match[1]) == expected, ('uncertified oversized request', raw, expected)
    canonical = normal.json(before, artifact+'.public-preview')
    replacement = str(len(canonical) - 1 + delta).encode()
    start, end = match.span(1)
    normal.record(artifact, ('request_bytes',), match[1], replacement,
                  'cli-validated-preview-request-length', start, end)
    return raw[:start] + replacement + raw[end:]

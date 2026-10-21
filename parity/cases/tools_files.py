"""T01–T22: argument, workspace-file and guarded-publication boundaries.

Expected bytes, hashes, line numbers and removal ledgers are computed from the
fixture, never copied from an executable result. Each parameter is selectable.
"""

import errno
import hashlib
import json
import os
from pathlib import Path
import stat
import sys

from parity.model import Case
from parity.cases.tools_support import drive, keep_file


MIB = 1 << 20
READ_FIRST = 'read the file before changing it (read_file records what you have seen)'
DIGEST_ERROR = 'expected_sha256 must be a 64-character digest, or omitted to use the version you last read or wrote'
NOT_FOUND = 'old_text does not occur in the file; read the range again before retrying'
TOOLS = ('list_files', 'read_file', 'search_text', 'write_file', 'edit_file', 'delete_file')
BASE = {'list_files': {'path': 'd'}, 'read_file': {'path': 'f'},
        'search_text': {'path': 'f', 'query': 'alpha'},
        'write_file': {'path': 'new', 'content': 'new'},
        'edit_file': {'path': 'f', 'old_text': 'alpha', 'new_text': 'beta'},
        'delete_file': {'path': 'f'}}


def digest(data):
    return hashlib.sha256(data if isinstance(data, bytes) else data.encode()).hexdigest()


def put(ctx, path, data=b'alpha\nbeta\n'):
    target = ctx.workspace / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(data if isinstance(data, bytes) else data.encode())
    return target


def setup(ctx):
    put(ctx, 'f')
    (ctx.workspace / 'd').mkdir()


def envelope(ctx, out, *, code='ok', message='', data=None, effect='none', truncated=False):
    expected = {'workspace': str(ctx.workspace), 'ok': code == 'ok', 'code': code,
                'message': message, 'data': data, 'truncated': truncated, 'effect': effect}
    assert out == expected, (out, expected)
    assert list(out) == list(expected), list(out)


def failure(ctx, out, code, message):
    envelope(ctx, out, code=code, message=message)


def proof(ctx, **values):
    ctx._record('effects/proof.json', json.dumps(values, ensure_ascii=False,
                                               sort_keys=True, separators=(',', ':')).encode())


def one(ctx, tool, args):
    return drive(ctx, [(tool, args)])[0]


def unchanged(ctx, data=b'alpha\nbeta\n'):
    assert (ctx.workspace / 'f').read_bytes() == data
    assert not list(ctx.workspace.rglob('.reagent-*'))


def argument(ctx, tool, shape):
    setup(ctx)
    args = dict(BASE[tool])
    errors = {'array': ('[]', 'arguments must be one JSON object'),
              'null-object': ('null', 'arguments must be one JSON object'),
              'scalar': ('1', 'arguments must be one JSON object'),
              'incomplete': ('{', 'invalid JSON at byte 2: incomplete input'),
              'bad-json': ('{"path":}', 'invalid JSON at byte 9'),
              'trailing': (json.dumps(args) + ' {}', 'arguments must be one JSON object')}
    if shape in errors:
        args, message = errors[shape]
    elif shape == 'unknown':
        args['unrecognized'] = 1
        message = 'unknown field "unrecognized"'
    elif shape == 'wrong-type':
        args['path'] = 1
        message = 'field "path" must be a string'
    elif shape in ('missing', 'null-path'):
        if shape == 'missing':
            args.pop('path')
        else:
            args['path'] = None
        # Struct string fields accept null/missing; the schema is not enforced.
        out = one(ctx, tool, args)
        if tool == 'list_files':
            assert out['ok'] and out['data']['path'] == ''
            assert [x['name'] for x in out['data']['entries']] == ['d', 'f']
        else:
            code, message = ('not_file', 'path is a directory')
            if tool == 'search_text':
                if shape == 'missing':
                    code, message = 'invalid_arguments', 'provide exactly one of path or paths'
                else:
                    assert out['ok'] and out['data']['files_scanned'] == 1
                    unchanged(ctx)
                    return
            elif tool in ('write_file', 'delete_file'):
                code, message = 'invalid_arguments', 'path is not a regular file'
            failure(ctx, out, code, message)
        unchanged(ctx)
        return
    failure(ctx, one(ctx, tool, args), 'invalid_arguments', message)
    unchanged(ctx)
    assert not (ctx.workspace / 'new').exists()


def optional(ctx, tool, field, least, variant):
    setup(ctx)
    args = dict(BASE[tool])
    boolean = field in ('regex', 'recursive', 'replace_all')
    values = {'null': None, 'fraction': 1.5, 'zero': 0, 'negative': -1,
              'overflow': 1 << 80, 'string': '1', 'false': False, 'true': True}
    if variant != 'omitted':
        args[field] = values[variant]
    if field == 'replace_all':
        args = {'path': 'f', 'edits': [{'old_text': 'alpha', 'new_text': 'gamma',
                                       **({field: values[variant]} if variant != 'omitted' else {})}]}
    out = one(ctx, tool, args)
    if variant == 'omitted' or boolean and variant in ('false', 'true') or not boolean and variant == 'zero' and least == 0:
        if tool == 'delete_file' or tool == 'edit_file':
            failure(ctx, out, 'invalid_arguments', READ_FIRST)
        else:
            assert out['ok'], out
            if tool == 'read_file':
                assert out['data']['lines'][0]['number'] == 1
            if tool == 'search_text':
                assert out['data']['complete'] is True
        unchanged(ctx)
        return
    if boolean:
        message = field + (' must be omitted rather than null' if variant == 'null' else ' must be a boolean')
        if field == 'replace_all':
            message = 'edits[0]: replace_all must be a boolean'
    elif variant == 'null':
        message = field + ' must be omitted rather than null'
    elif variant in ('zero', 'negative'):
        message = '%s must be at least %d' % (field, least)
    else:
        message = field + ' must be an integer'
    failure(ctx, out, 'invalid_arguments', message)
    unchanged(ctx)


def paths(ctx, variant):
    setup(ctx)
    if variant in ('.git', '.GiT', '.env', '.ENV', '.env.local', '.EnV.SECRET'):
        failure(ctx, one(ctx, 'read_file', {'path': variant + '/sentinel'}),
                'invalid_path', variant + ' is not readable')
    elif variant == 'parent':
        failure(ctx, one(ctx, 'read_file', {'path': 'd/../f'}),
                'invalid_path', 'path must not contain a .. component')
    elif variant == 'nul':
        failure(ctx, one(ctx, 'read_file', {'path': 'f\0'}),
                'invalid_path', 'path must not contain a NUL byte')
    elif variant == 'outside':
        outside = ctx.root / 'outside'
        outside.write_bytes(b'SECRET')
        failure(ctx, one(ctx, 'read_file', {'path': str(outside)}),
                'invalid_path', 'path must be inside the workspace, ' + str(ctx.workspace))
        assert outside.read_bytes() == b'SECRET'
    elif variant in ('root', 'empty'):
        out = one(ctx, 'list_files', {'path': '.' if variant == 'root' else ''})
        assert out['ok'] and [x['name'] for x in out['data']['entries']] == ['d', 'f']
    else:
        name = {'unicode': 'café-🦀', 'environment': '.environment'}.get(variant, 'f')
        if name != 'f':
            put(ctx, name, b'alpha\nbeta\n')
        path = str(ctx.workspace / name) if variant == 'absolute' else name
        read_expect(ctx, one(ctx, 'read_file', {'path': path}), name, b'alpha\nbeta\n')


def symlinks(ctx, variant):
    setup(ctx)
    outside = ctx.root / 'outside'
    outside.write_bytes(b'SECRET')
    target = {'inside': ctx.workspace / 'f', 'outside': outside,
              'broken': ctx.root / 'absent', 'ancestor': ctx.root / 'outside-dir',
              'root-alias': ctx.workspace}[variant]
    if variant == 'ancestor':
        target.mkdir()
        (target / 'sentinel').write_bytes(b'EXTERNAL')
    (ctx.workspace / 'link').symlink_to(target)
    if variant == 'ancestor':
        # An existing parent reaches publication if resolve loses containment;
        # a missing parent would instead hit createParents' symlink refusal.
        out = one(ctx, 'write_file', {'path': 'link/new', 'content': 'NO'})
        assert (target / 'sentinel').is_file(), 'external ancestor sentinel disappeared'
        assert (target / 'sentinel').read_bytes() == b'EXTERNAL', 'external ancestor sentinel changed'
        assert sorted(path.name for path in target.iterdir()) == ['sentinel'], 'outside ancestor write published'
        failure(ctx, out, 'invalid_path', 'resolved path must be inside the workspace and not withheld')
        proof(ctx, outside_sha256=digest((target / 'sentinel').read_bytes()), published=False)
    elif variant == 'root-alias':
        out = one(ctx, 'read_file', {'path': 'link/f'})
        read_expect(ctx, out, 'link/f', b'alpha\nbeta\n')
    elif variant == 'inside':
        outs = drive(ctx, [('read_file', {'path': 'link'}),
                           ('write_file', {'path': 'link', 'content': 'NO'})])
        read_expect(ctx, outs[0], 'link', b'alpha\nbeta\n')
        failure(ctx, outs[1], 'symlink_target', 'path is a symlink')
    elif variant == 'outside':
        failure(ctx, one(ctx, 'read_file', {'path': 'link'}), 'invalid_path',
                'resolved path must be inside the workspace and not withheld')
    else:
        failure(ctx, one(ctx, 'read_file', {'path': 'link'}), 'not_found', 'no such path in the workspace')
    unchanged(ctx)
    assert outside.read_bytes() == b'SECRET'


def replaced_root(ctx):
    setup(ctx)
    saved = ctx.root / 'saved-workspace'
    code = ("from pathlib import Path; import os; "
            "root=Path.cwd(); os.rename(root, root.parent/'saved-workspace'); root.mkdir(); "
            "(root/'f').write_text('replacement')")
    try:
        outs = drive(ctx, [('read_file', {'path': 'f'}), python(code),
                           ('delete_file', {'path': 'f', 'recursive': True})])
        failure(ctx, outs[-1], 'invalid_path', 'workspace root changed after it was opened')
        assert (saved / 'f').read_bytes() == b'alpha\nbeta\n'
        assert (ctx.workspace / 'f').read_bytes() == b'replacement'
        proof(ctx, original_sha256=digest((saved / 'f').read_bytes()),
              replacement_sha256=digest((ctx.workspace / 'f').read_bytes()))
    finally:
        if saved.exists():
            (ctx.workspace / 'f').unlink(missing_ok=True)
            ctx.workspace.rmdir()
            saved.rename(ctx.workspace)


def invalid_name_vector(root):
    """Certify whether this filesystem can represent the raw invalid UTF-8 name."""
    name = os.fsencode(root) + b'/\xff'
    try:
        fd = os.open(name, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except OSError as exc:
        assert sys.platform == 'darwin' and exc.errno == errno.EILSEQ, (sys.platform, exc)
        assert b'\xff' not in os.listdir(os.fsencode(root))
        return {'raw_name_created': False, 'filesystem_rejection': 'EILSEQ',
                'invalid_utf8_tool_branch_verified': False,
                'gap': 'Darwin filesystem rejects invalid UTF-8 names before list_files can see them'}
    os.close(fd)
    assert b'\xff' in os.listdir(os.fsencode(root))
    return {'raw_name_created': True, 'filesystem_rejection': None,
            'invalid_utf8_tool_branch_verified': True, 'gap': None}


def listing(ctx, variant):
    root = ctx.workspace / 'd'
    root.mkdir()
    put(ctx, 'd/a', b'a')
    (root / 'b').mkdir()
    (root / 'c').symlink_to(root / 'a')
    os.mkfifo(root / 'e')
    put(ctx, 'd/é', b'utf8')
    for name in ('.git', '.env', '.ENV.local'):
        put(ctx, 'd/' + name, b'withheld')
    skipped = 3
    if variant == 'invalid-name':
        certificate = invalid_name_vector(root)
        skipped += int(certificate['raw_name_created'])
        proof(ctx, **certificate)
    entries = [{'path': 'd/' + name, 'name': name, 'kind': kind}
               for name, kind in [('a', 'file'), ('b', 'dir'), ('c', 'symlink'), ('e', 'file'), ('é', 'file')]]
    offset, limit = {'default': (0, None), 'page': (1, 2), 'continuation': (3, 2),
                     'end': (5, None), 'past-end': (99, None), 'invalid-name': (0, None)}[variant]
    args = {'path': 'd', 'offset': offset}
    if limit is not None:
        args['limit'] = limit
    page = entries[offset:][:limit]
    end = offset + len(page)
    envelope(ctx, one(ctx, 'list_files', args), data={'path': 'd', 'entries': page,
             'next_offset': end if end < len(entries) else None, 'skipped_entries': skipped,
             'complete': True}, truncated=end < len(entries))


def lines_of(data):
    return [] if not data else [line.removesuffix('\r') for line in data.decode().removesuffix('\n').split('\n')]


def read_expect(ctx, out, path, data, start=1, maximum=250):
    lines = lines_of(data)
    selected = lines[start - 1:start - 1 + maximum]
    end = start - 1 + len(selected)
    envelope(ctx, out, data={'path': path, 'sha256': digest(data), 'size_bytes': len(data),
             'total_lines': len(lines), 'lines': [{'number': start + i, 'text': line}
                                                for i, line in enumerate(selected)],
             'next_line': end + 1 if end < len(lines) else None, 'eof': end == len(lines)},
             truncated=end < len(lines))


def reading(ctx, variant):
    data = {'empty': b'', 'lf': b'a\nb\n', 'crlf': b'a\r\nb\r\n',
            'no-lf': b'a\nb', 'range': b'a\nb\nc\n', 'eof': b'a\n',
            'bad-range': b'a\n', 'default-page': b'x\n' * 260,
            'large-page': b'x\n' * 500}[variant]
    put(ctx, 'f', data)
    args = {'path': 'f'}
    if variant in ('range', 'eof', 'bad-range'):
        args['start_line'] = 2 if variant != 'eof' else 1
        args['max_lines'] = 1
    if variant == 'large-page':
        args['max_lines'] = 500
    out = one(ctx, 'read_file', args)
    if variant == 'bad-range':
        failure(ctx, out, 'line_out_of_range', 'file has 1 lines')
    else:
        read_expect(ctx, out, 'f', data, args.get('start_line', 1), args.get('max_lines', 250))


def read_refusal(ctx, variant):
    if variant == 'limit':
        data = b'x\n' * (MIB // 2)
        put(ctx, 'f', data)
        read_expect(ctx, one(ctx, 'read_file', {'path': 'f'}), 'f', data)
        return
    code, message = {'nul': ('binary_file', 'file contains NUL bytes'),
        'invalid-utf8': ('invalid_utf8', 'file is not valid UTF-8'),
        'directory': ('not_file', 'path is a directory'),
        'fifo': ('not_file', 'path is not a regular file'),
        'missing': ('not_found', 'no such path in the workspace'),
        'permission': ('permission_denied', 'path is not readable'),
        'large': ('file_too_large', 'file is larger than the 1048576 byte read limit'),
        'huge-line': ('line_too_long', 'line 1 alone does not fit in one result')}[variant]
    data = {'nul': b'a\0b', 'invalid-utf8': b'\xff', 'large': b'x' * (MIB + 1),
            'huge-line': b'x' * 40000, 'permission': b'alpha'}
    target = ctx.workspace / 'f'
    if variant in data:
        put(ctx, 'f', data[variant])
    elif variant == 'directory':
        target.mkdir()
    elif variant == 'fifo':
        os.mkfifo(target)
    if variant == 'permission':
        assert os.geteuid() != 0, 'permission fixture requires a non-root process'
        target.chmod(0)
    calls = [('read_file', {'path': 'f'})]
    if variant == 'huge-line':
        calls.append(('write_file', {'path': 'f', 'content': 'NO'}))
    try:
        outs = drive(ctx, calls)
        failure(ctx, outs[0], code, message)
        if variant == 'huge-line':
            failure(ctx, outs[1], 'invalid_arguments', READ_FIRST)
            assert target.read_bytes() == data[variant]
    finally:
        if variant == 'permission':
            target.chmod(0o600)


def search_expect(ctx, out, data, numbers, *, complete=True, reason=None, message='', truncated=False, texts=None):
    lines = lines_of(data)
    matches = [{'path': 'f', 'line': n, 'text': lines[n - 1] if texts is None else texts[i],
                'preview_truncated': texts is not None} for i, n in enumerate(numbers)]
    envelope(ctx, out, message=message, data={'matches': matches, 'files_scanned': 1,
             'bytes_scanned': len(data), 'skipped_files': 0, 'complete': complete,
             'stop_reason': reason}, truncated=truncated)


def searching(ctx, variant):
    data = 'Alpha\nalpha|beta\ncafé 42\nβeta\n'.encode()
    put(ctx, 'f', data)
    query, regex, numbers = {'literal': ('Alpha', False, [1]), 'case': ('ALPHA', False, []),
        'pipe': ('alpha|beta', False, [2]), 'pipe-hint': ('not|here', False, []),
        'alternation': ('Alpha|βeta', True, [1, 4]), 'anchors': ('^Alpha$', True, [1]),
        'unicode': ('\\p{Greek}+', True, [4]), 'classes': ('[0-9]+', True, [3]),
        'case-flag': ('(?i)^alpha', True, [1, 2]),
        'named-capture': ('(?P<name>café)', True, [3])}[variant]
    out = one(ctx, 'search_text', {'path': 'f', 'query': query, 'regex': regex})
    message = '' if variant != 'pipe-hint' else 'no matches; the query contains "|" but regex is false, so it was searched literally; set regex: true to search alternatives'
    search_expect(ctx, out, data, numbers, message=message)


def search_arguments(ctx, variant):
    put(ctx, 'f', b'alpha\n')
    args = {'path': 'f', 'query': 'alpha'}
    cases = {'empty': ({'query': ''}, 'query must not be empty'),
        'lf': ({'query': 'a\nb'}, 'query must be one line without NUL bytes'),
        'cr': ({'query': 'a\rb'}, 'query must be one line without NUL bytes'),
        'nul': ({'query': 'a\0b'}, 'query must be one line without NUL bytes'),
        'empty-regex': ({'query': 'a*', 'regex': True}, 'pattern matches the empty string, so it would match every line'),
        'both': ({'paths': ['f']}, 'provide exactly one of path or paths'),
        'zero-paths': ({'paths': []}, 'paths must be an array of 1 to 20 strings'),
        'null-paths': ({'paths': None}, 'paths must be an array of 1 to 20 strings'),
        'many-paths': ({'paths': ['f'] * 21}, 'paths must be an array of 1 to 20 strings'),
        'bad-path': ({'paths': ['f', None]}, 'paths[1]: path must be a string')}
    if variant in cases:
        update, message = cases[variant]
        args.update(update)
        if 'paths' in update and variant != 'both':
            args.pop('path')
        failure(ctx, one(ctx, 'search_text', args), 'invalid_arguments', message)
    elif variant in ('one-path', 'twenty-paths', 'duplicates', 'order'):
        put(ctx, 'g', b'alpha\n')
        scope = {'one-path': ['f'], 'twenty-paths': ['f'] * 20,
                 'duplicates': ['f', 'f', '.'], 'order': ['g', 'f']}[variant]
        out = one(ctx, 'search_text', {'paths': scope, 'query': 'alpha'})
        names = ['g', 'f'] if variant == 'order' else ['f', 'g'] if variant == 'duplicates' else ['f']
        envelope(ctx, out, data={'matches': [{'path': n, 'line': 1, 'text': 'alpha',
                 'preview_truncated': False} for n in names], 'files_scanned': len(names),
                 'bytes_scanned': 6 * len(names), 'skipped_files': 0, 'complete': True, 'stop_reason': None})
    else:
        query, reason, position = {'unclosed': ('[', 'unclosed character class', 1),
            'lookahead': ('(?=a)', 'unsupported RE2 construct', 1),
            'backreference': (r'(a)\1', 'invalid escape', 4)}[variant]
        failure(ctx, one(ctx, 'search_text', {'path': 'f', 'query': query, 'regex': True}),
                'invalid_arguments', 'invalid RE2 regex at byte %d: %s' % (position, reason))


def search_skips(ctx, variant):
    put(ctx, 'd/good', b'alpha\n')
    if variant == 'excluded':
        put(ctx, 'd/node_modules/f', b'alpha\n')
        put(ctx, 'd/.env', b'SECRET')
        outs = drive(ctx, [('search_text', {'path': 'd', 'query': 'alpha'}),
                           ('search_text', {'path': 'd/node_modules', 'query': 'alpha'})])
        for out, name in zip(outs, ('d/good', 'd/node_modules/f')):
            envelope(ctx, out, data={'matches': [{'path': name, 'line': 1, 'text': 'alpha',
                     'preview_truncated': False}], 'files_scanned': 1, 'bytes_scanned': 6,
                     'skipped_files': 0, 'complete': True, 'stop_reason': None})
        return
    target = ctx.workspace / 'd/bad'
    code, message = {'binary': ('binary_file', 'file contains NUL bytes'),
        'large': ('file_too_large', 'file is larger than the 1048576 byte read limit'),
        'permission': ('permission_denied', 'path is not readable'),
        'symlink': (None, None)}[variant]
    if variant == 'symlink':
        target.symlink_to(ctx.workspace / 'd/good')
    else:
        target.write_bytes(b'\0' if variant == 'binary' else b'x' * (MIB + 1) if variant == 'large' else b'alpha\n')
    if variant == 'permission':
        assert os.geteuid() != 0
        target.chmod(0)
    try:
        outs = drive(ctx, [('search_text', {'path': 'd', 'query': 'alpha'}),
                           ('search_text', {'path': 'd/bad', 'query': 'alpha'})])
        envelope(ctx, outs[0], data={'matches': [{'path': 'd/good', 'line': 1, 'text': 'alpha',
                 'preview_truncated': False}], 'files_scanned': 1, 'bytes_scanned': 6,
                 'skipped_files': 1, 'complete': False, 'stop_reason': None})
        if code:
            failure(ctx, outs[1], code, message)
        else:
            assert outs[1]['ok'] and outs[1]['data']['matches'][0]['path'] == 'd/bad'
    finally:
        if variant == 'permission':
            target.chmod(0o600)


def search_boundary(ctx, variant):
    if variant == 'exact-max':
        data = b'alpha\nalpha\n'
        put(ctx, 'f', data)
        out = one(ctx, 'search_text', {'path': 'f', 'query': 'alpha', 'max_results': 2})
        search_expect(ctx, out, data, [1, 2], complete=False, reason='max_results')
        return
    data = ('🦀' * 20000 + '\n').encode() if variant == 'multibyte' else b'alpha\n' * 1000
    put(ctx, 'f', data)
    out = one(ctx, 'search_text', {'path': 'f', 'query': '🦀' if variant == 'multibyte' else 'alpha'})
    result = out['data']
    assert result['complete'] is False and result['stop_reason'] == 'result_bytes'
    assert result['files_scanned'] == 1 and result['bytes_scanned'] == len(data)
    assert result['skipped_files'] == 0
    if variant == 'multibyte':
        # shrinkFirst halves encoded bytes until one whole UTF-8 preview fits.
        assert result['matches'] == [{'path': 'f', 'line': 1, 'text': '🦀' * 5000,
                                      'preview_truncated': True}]
        assert out['truncated'] is False
    else:
        matches = result['matches']
        assert 0 < len(matches) < 1000 and out['truncated'] is True
        assert matches == [{'path': 'f', 'line': i + 1, 'text': 'alpha',
                            'preview_truncated': False} for i in range(len(matches))]
        # Independently prove the maximal prefix against the 32 KiB envelope.
        def encoded(value):
            return len(json.dumps(value, ensure_ascii=False, separators=(',', ':')).encode())
        assert encoded(out) <= 32768
        larger = dict(out, data=dict(result, matches=matches + [{'path': 'f', 'line': len(matches) + 1,
                          'text': 'alpha', 'preview_truncated': False}]))
        # fitElements measures the pre-truncation envelope's false flag.
        assert encoded(dict(larger, truncated=False)) > 32768


def writing(ctx, variant):
    if variant in ('empty', 'nonempty', 'nested'):
        name = 'a/b/f' if variant == 'nested' else 'f'
        content = '' if variant == 'empty' else 'café\n'
        out = one(ctx, 'write_file', {'path': name, 'content': content})
        expected = {'operation': 'create', 'path': name, 'before_sha256': None,
                    'after_sha256': digest(content), 'size_bytes': len(content.encode())}
        if variant == 'nested':
            expected['created_dirs'] = ['a', 'a/b']
        envelope(ctx, out, data=expected, effect='applied')
        assert (ctx.workspace / name).read_bytes() == content.encode()
        keep_file(ctx, name)
        return
    put(ctx, 'f', b'alpha\nbeta\n')
    calls = []
    if variant in ('read', 'ranged-read'):
        calls.append(('read_file', {'path': 'f', 'max_lines': 1 if variant == 'ranged-read' else 250}))
    elif variant == 'written':
        calls.append(('write_file', {'path': 'g', 'content': 'alpha\nbeta\n'}))
    name = 'g' if variant == 'written' else 'f'
    calls.append(('write_file', {'path': name, 'content': 'new'}))
    out = drive(ctx, calls)[-1]
    if variant == 'unread':
        failure(ctx, out, 'invalid_arguments', READ_FIRST)
        unchanged(ctx)
    else:
        envelope(ctx, out, data={'operation': 'overwrite', 'path': name,
                 'before_sha256': digest(b'alpha\nbeta\n'), 'after_sha256': digest('new'),
                 'size_bytes': 3}, effect='applied')
        assert (ctx.workspace / name).read_bytes() == b'new'
        keep_file(ctx, name)


def digests(ctx, tool, variant):
    put(ctx, 'f', b'alpha\nbeta\n')
    args = {'path': 'f', **({'content': 'new'} if tool == 'write_file' else
            {'old_text': 'alpha', 'new_text': 'new'} if tool == 'edit_file' else {})}
    values = {'correct': digest(b'alpha\nbeta\n'), 'unknown': '0' * 64,
              'short': 'a' * 63, 'uppercase': digest('alpha').upper(),
              'nonhex': 'g' * 64, 'null': None}
    if variant != 'missing':
        args['expected_sha256'] = values[variant]
    out = one(ctx, tool, args)
    if variant == 'correct':
        assert out['ok'] and out['effect'] == 'applied'
        expected = b'new' if tool == 'write_file' else b'new\nbeta\n'
        assert not (ctx.workspace / 'f').exists() if tool == 'delete_file' else (ctx.workspace / 'f').read_bytes() == expected
    else:
        code, message = ('invalid_arguments', READ_FIRST) if variant == 'missing' else (
            'unknown_digest', 'this is not a digest read_file returned for this file; its current digest is ' + digest(b'alpha\nbeta\n')) if variant == 'unknown' else ('invalid_arguments', DIGEST_ERROR)
        failure(ctx, out, code, message)
        unchanged(ctx)


def python(code):
    return 'exec', {'argv': [sys.executable, '-c', code], 'cwd': '.'}


def stale(ctx, variant):
    put(ctx, 'f', b'alpha\nbeta\n')
    calls = [('read_file', {'path': 'f'})]
    if variant == 'exec-created':
        calls = [python("from pathlib import Path; Path('new').write_text('new')"),
                 ('write_file', {'path': 'new', 'content': 'NO'})]
    elif variant in ('deleted', 'recreate'):
        calls += [python("from pathlib import Path; Path('f').unlink()"),
                  ('write_file', {'path': 'f', 'content': 'new'})]
        if variant == 'recreate':
            calls += [('read_file', {'path': 'f'}), ('write_file', {'path': 'f', 'content': 'new'})]
    else:
        calls += [python("from pathlib import Path; Path('f').write_text('changed')")]
        args = {'path': 'f', 'content': 'NO'}
        if variant == 'explicit':
            args['expected_sha256'] = digest(b'alpha\nbeta\n')
        calls.append(('write_file', args))
    outs = drive(ctx, calls, flags=('--agents',) if variant in ('deleted', 'recreate') else ())
    if variant == 'exec-created':
        failure(ctx, outs[-1], 'invalid_arguments', READ_FIRST)
        assert (ctx.workspace / 'new').read_bytes() == b'new'
    elif variant in ('deleted', 'recreate'):
        failure(ctx, outs[2], 'stale_file', 'the file you read or wrote was deleted; read_file must observe its absence before recreating it')
        if variant == 'recreate':
            failure(ctx, outs[3], 'not_found', 'no such path in the workspace')
            assert outs[4]['ok'] and (ctx.workspace / 'f').read_bytes() == b'new'
        else:
            assert not (ctx.workspace / 'f').exists()
    else:
        message = ('the file has changed since it was read; its current digest is ' + digest('changed')) if variant == 'explicit' else (
            'the file has changed since you last read or wrote it (for example through exec); its current digest is ' + digest('changed') + '. Read the part you will change again, or pass expected_sha256 if you know what changed')
        failure(ctx, outs[-1], 'stale_file', message)
        assert (ctx.workspace / 'f').read_bytes() == b'changed'


def write_refusal(ctx, variant):
    args = {'path': 'new', 'content': 'new'}
    if variant == 'parent-file':
        put(ctx, 'f', b'parent')
        args['path'] = 'f/child'
        code, message = 'io_error', 'path component is not a directory'
    elif variant in ('permission', 'mode'):
        put(ctx, 'd/f', b'old').chmod(0o640)
        args = {'path': 'd/f', 'content': 'new', 'expected_sha256': digest('old')}
        if variant == 'permission':
            assert os.geteuid() != 0
            (ctx.workspace / 'd').chmod(0o555)
        code, message = 'permission_denied', 'path is not readable'
    else:
        values = {'missing': None, 'number': 1, 'null': None, 'nul': 'a\0b',
                  'limit': 'x' * MIB, 'over-limit': 'x' * (MIB + 1)}
        if variant == 'missing':
            args.pop('content')
        else:
            args['content'] = values[variant]
        code, message = ('binary_file', 'content contains NUL bytes') if variant == 'nul' else (
            'file_too_large', 'content exceeds the 1048576 byte limit') if variant == 'over-limit' else ('invalid_arguments', 'content must be a string')
    try:
        out = one(ctx, 'write_file', args)
        if variant in ('mode', 'limit'):
            assert out['ok'] and out['effect'] == 'applied'
            assert (ctx.workspace / args['path']).read_bytes() == args['content'].encode()
            if variant == 'mode':
                assert stat.S_IMODE((ctx.workspace / 'd/f').stat().st_mode) == 0o640
        elif variant == 'parent-file':
            failure(ctx, out, code, message)
            assert (ctx.workspace / 'f').read_bytes() == b'parent'
        else:
            failure(ctx, out, code, message)
            if variant == 'permission':
                assert (ctx.workspace / 'd/f').read_bytes() == b'old'
            else:
                assert not (ctx.workspace / 'new').exists()
        assert not list(ctx.workspace.rglob('.reagent-*'))
    finally:
        if variant == 'permission':
            (ctx.workspace / 'd').chmod(0o755)


def edit(ctx, variant):
    before = b'alpha\nbeta\nalpha\n' if variant in ('ambiguous', 'replace-all', 'identity-ambiguous') else b'alpha\nbeta\n'
    if variant == 'replace-all-nonoverlapping':
        before = b'aaaa\n'
    put(ctx, 'f', before).chmod(0o640)
    args = {'path': 'f', 'old_text': 'alpha', 'new_text': 'gamma'}
    code = message = None
    after = b'gamma\nbeta\n'
    counts, skipped, appended = [1], None, 0
    if variant in ('missing', 'near'):
        args['old_text'] = 'absent' if variant == 'missing' else 'alpha \nbeta'
        code, message = 'edit_not_found', NOT_FOUND if variant == 'missing' else 'old_text does not occur in the file; it matches with different whitespace at line 1 (read that range again and copy it exactly)'
    elif variant in ('ambiguous', 'identity-ambiguous'):
        if variant == 'identity-ambiguous':
            args['new_text'] = 'alpha'
        code, message = 'ambiguous_edit', 'old_text occurs 2 times, at lines 1, 3; include enough surrounding text to make it unique or set replace_all if every occurrence should change'
    elif variant == 'delete-match':
        args['new_text'] = ''
        after = b'\nbeta\n'
    elif variant in ('snapshot', 'atomic', 'overlap', 'all-identity', 'mixed-identity', 'replace-all', 'replace-all-nonoverlapping'):
        edits = [{'old_text': 'alpha', 'new_text': 'beta'}, {'old_text': 'beta', 'new_text': 'end'}]
        after, counts = b'beta\nend\n', [1, 1]
        if variant == 'atomic':
            edits[1]['old_text'] = 'absent'
            code, message = 'edit_not_found', 'edits[1]: ' + NOT_FOUND
        elif variant == 'overlap':
            edits[1]['old_text'] = 'lpha'
            code, message = 'invalid_arguments', 'edits[0] and edits[1]: matched ranges overlap'
        elif variant == 'all-identity':
            edits = [{'old_text': 'alpha', 'new_text': 'alpha'}]
            code, message = 'invalid_arguments', 'every edit changes nothing'
        elif variant == 'mixed-identity':
            edits[0]['new_text'] = 'alpha'
            after, counts = b'alpha\nend\n', [0, 1]
            skipped = ['edits[0] changed nothing and was skipped']
        elif variant == 'replace-all':
            edits = [{'old_text': 'alpha', 'new_text': 'gamma', 'replace_all': True}]
            after, counts = b'gamma\nbeta\ngamma\n', [2]
        elif variant == 'replace-all-nonoverlapping':
            edits = [{'old_text': 'aa', 'new_text': 'x', 'replace_all': True}]
            after, counts = b'xx\n', [2]
        args = {'path': 'f', 'edits': edits}
    elif variant in ('append', 'append-no-lf', 'batch-append'):
        if variant == 'append-no-lf':
            before = b'alpha'
            put(ctx, 'f', before).chmod(0o640)
        args = {'path': 'f', 'append_text': 'TAIL'}
        after, counts, appended = before + b'TAIL', [], 4
        if variant == 'batch-append':
            args['edits'] = [{'old_text': 'alpha', 'new_text': 'gamma'}]
            after, counts = b'gamma\nbeta\nTAIL', [1]
    elif variant in ('empty-append', 'null-append', 'mixed-forms', 'missing-new', 'empty-edits'):
        update, message = {'empty-append': ({'append_text': ''}, 'append_text must be a non-empty string'),
            'null-append': ({'append_text': None}, 'append_text must be a non-empty string'),
            'mixed-forms': ({'edits': [{'old_text': 'alpha', 'new_text': 'x'}]}, 'old_text/new_text and edits[0] cannot be combined'),
            'missing-new': ({}, 'old_text and new_text are both required'),
            'empty-edits': ({'edits': []}, 'edits must be a non-empty array')}[variant]
        if variant == 'missing-new':
            args.pop('new_text')
        if variant == 'empty-edits':
            args = {'path': 'f'}
        args.update(update)
        code = 'invalid_arguments'
    elif variant in ('nul-result', 'large-result'):
        args['new_text'] = '\0' if variant == 'nul-result' else 'x' * MIB
        code, message = ('binary_file', 'the result would contain NUL bytes') if variant == 'nul-result' else ('file_too_large', 'the result would exceed the 1048576 byte limit')
    outs = drive(ctx, [('read_file', {'path': 'f'}), ('edit_file', args)])
    if code:
        failure(ctx, outs[1], code, message)
        unchanged(ctx, before)
    else:
        expected = {'operation': 'update', 'path': 'f', 'changed': True,
                    'before_sha256': digest(before), 'after_sha256': digest(after), 'size_bytes': len(after)}
        if sum(counts):
            expected['edits'] = sum(counts)
        if counts:
            expected['edit_counts'] = counts
        if skipped:
            expected['skipped_edits'] = skipped
        if appended:
            expected['appended_bytes'] = appended
        envelope(ctx, outs[1], data=expected, effect='applied')
        assert (ctx.workspace / 'f').read_bytes() == after
        assert stat.S_IMODE((ctx.workspace / 'f').stat().st_mode) == 0o640
        keep_file(ctx, 'f')


def edit_guard(ctx, variant):
    setup(ctx)
    args = {'path': 'f', 'old_text': 'alpha', 'new_text': 'gamma'}
    calls = []
    code, message = 'invalid_arguments', READ_FIRST
    if variant == 'create':
        args['path'] = 'missing'
        code, message = 'not_found', 'no such path in the workspace'
    elif variant == 'symlink':
        (ctx.workspace / 'link').symlink_to('f')
        args['path'] = 'link'
        code, message = 'symlink_target', 'path is a symlink'
    elif variant == 'large-input':
        put(ctx, 'f', b'x' * (MIB + 1))
        code, message = 'file_too_large', 'file is larger than the 1048576 byte read limit'
    elif variant == 'nul-input':
        put(ctx, 'f', b'alpha\0')
        code, message = 'binary_file', 'file contains NUL bytes'
    elif variant == 'permission':
        assert os.geteuid() != 0
        calls = [('read_file', {'path': 'f'}), python("from pathlib import Path; Path('.').chmod(0o555)")]
        code, message = 'permission_denied', 'path is not readable'
    elif variant == 'stale':
        calls = [('read_file', {'path': 'f'}), python("from pathlib import Path; Path('f').write_text('changed')")]
        args['expected_sha256'] = digest(b'alpha\nbeta\n')
        code, message = 'stale_file', 'the file has changed since it was read; its current digest is ' + digest('changed')
    calls.append(('edit_file', args))
    before = (ctx.workspace / 'f').read_bytes()
    try:
        failure(ctx, drive(ctx, calls)[-1], code, message)
        unchanged(ctx, b'changed' if variant == 'stale' else before)
    finally:
        if variant == 'permission':
            ctx.workspace.chmod(0o755)
    assert not (ctx.workspace / 'missing').exists()


def deleting(ctx, variant):
    data = b'\0\xff' * MIB if variant == 'large-binary' else b'alpha\nbeta\n'
    put(ctx, 'f', data)
    calls = []
    args = {'path': 'f'}
    if variant == 'read':
        calls.append(('read_file', {'path': 'f'}))
    elif variant in ('explicit', 'large-binary'):
        args['expected_sha256'] = digest(data)
    elif variant == 'exec-created':
        (ctx.workspace / 'f').unlink()
        calls.append(python("from pathlib import Path; Path('f').write_text('alpha\\nbeta\\n')"))
    elif variant == 'stale':
        calls += [('read_file', {'path': 'f'}), python("from pathlib import Path; Path('f').write_text('changed')")]
        args['expected_sha256'] = digest(data)
    calls.append(('delete_file', args))
    out = drive(ctx, calls)[-1]
    if variant in ('unread', 'exec-created'):
        failure(ctx, out, 'invalid_arguments', READ_FIRST)
        unchanged(ctx)
    elif variant == 'stale':
        failure(ctx, out, 'stale_file', 'the file has changed since it was read; its current digest is ' + digest('changed'))
        unchanged(ctx, b'changed')
    else:
        envelope(ctx, out, data={'operation': 'delete', 'path': 'f', 'before_sha256': digest(data)}, effect='applied')
        assert not (ctx.workspace / 'f').exists()
        proof(ctx, removed=['f'], before_sha256=digest(data))


def delete_tree(ctx, variant):
    put(ctx, 'tree/a', b'a')
    args = {'path': 'tree', 'recursive': True}
    code = message = None
    outside = ctx.root / 'outside'
    outside.write_bytes(b'SECRET')
    if variant in ('link', 'link-off', 'outside-link'):
        (ctx.workspace / 'link').symlink_to(outside if variant == 'outside-link' else ctx.workspace / 'tree')
        args['path'] = 'link'
        if variant == 'link-off':
            args.pop('recursive')
            code, message = 'symlink_target', 'path is a symlink'
    elif variant == 'directory-off':
        args.pop('recursive')
        code, message = 'invalid_arguments', 'path is not a regular file'
    elif variant == 'root':
        args['path'] = '.'
        code, message = 'invalid_path', 'deletion must stay inside the workspace and cannot remove its root'
    elif variant == 'withheld':
        put(ctx, 'tree/z/.ENV.secret', b'SECRET')
        code, message = 'invalid_path', 'path contains a parent or withheld component'
    elif variant == 'special':
        os.mkfifo(ctx.workspace / 'tree/zfifo')
        code, message = 'invalid_arguments', 'recursive deletion refuses special files'
    elif variant == 'ancestor':
        (ctx.workspace / 'link').symlink_to('tree')
        args['path'] = 'link/a'
        code, message = 'symlink_target', 'recursive deletion refuses symlink ancestors'
    elif variant == 'digest-on-dir':
        args['expected_sha256'] = digest('a')
        code, message = 'invalid_arguments', 'expected_sha256 is only valid for regular files'
    out = one(ctx, 'delete_file', args)
    assert outside.exists(), 'outside symlink target disappeared'
    if code:
        failure(ctx, out, code, message)
        assert (ctx.workspace / 'tree/a').read_bytes() == b'a'
    else:
        removed = ['link'] if args['path'] == 'link' else ['tree/a', 'tree']
        envelope(ctx, out, data={'operation': 'delete', 'path': args['path'], 'recursive': True,
                 'removed_paths': removed, 'removed_count': len(removed), 'omitted': 0}, effect='applied')
        assert not (ctx.workspace / args['path']).exists() and not (ctx.workspace / args['path']).is_symlink()
        if args['path'] == 'link':
            assert (ctx.workspace / 'tree/a').read_bytes() == b'a'
        proof(ctx, removed=removed, outside_sha256=digest(outside.read_bytes()))
    assert outside.read_bytes() == b'SECRET', 'outside symlink target bytes changed'


def recursive(ctx, variant):
    assert os.geteuid() != 0, 'permission fixture requires a non-root process'
    paths = []
    if variant == 'partial-permission':
        paths = ['tree/a-blocked/f', 'tree/z-removed']
    elif variant == 'long-ledger':
        paths = ['tree/%03d-%s' % (i, 'x' * 96) for i in range(120)]
    else:
        paths = ['tree/a/f', 'tree/a/g', 'tree/b/deep/f', 'tree/z']
    for path in paths:
        put(ctx, path, b'fixture')
    # WalkDir's lexical preorder, reversed, is deterministic. The blocked
    # directory is readable/executable, so preflight succeeds but unlink fails.
    plan = sorted({parent.as_posix() for name in paths for parent in
                   [Path(name), *Path(name).parents] if parent.as_posix() != '.'})
    ledger = list(reversed(plan))
    blocked = ctx.workspace / 'tree/a-blocked'
    if variant == 'partial-permission':
        blocked.chmod(0o555)
    try:
        out = one(ctx, 'delete_file', {'path': 'tree', 'recursive': True})
        if variant == 'partial-permission':
            data = {'operation': 'delete', 'path': 'tree', 'recursive': True,
                    'removed_paths': ['tree/z-removed'], 'removed_count': 1, 'omitted': 0,
                    'failed_path': 'tree/a-blocked/f'}
            envelope(ctx, out, code='permission_denied', message='path is not readable', data=data, effect='applied')
            assert (ctx.workspace / 'tree/a-blocked/f').read_bytes() == b'fixture'
            assert (ctx.workspace / 'tree').is_dir() and not (ctx.workspace / 'tree/z-removed').exists()
        else:
            retained, size = [], 0
            for name in ledger:
                width = len(json.dumps(name, ensure_ascii=False).encode()) + 1
                if size + width <= 8192:
                    retained.append(name)
                    size += width
            omitted = len(ledger) - len(retained)
            data = {'operation': 'delete', 'path': 'tree', 'recursive': True,
                    'removed_paths': retained, 'removed_count': len(ledger), 'omitted': omitted}
            envelope(ctx, out, data=data, effect='applied', truncated=omitted > 0)
            assert not (ctx.workspace / 'tree').exists()
        proof(ctx, **data)
    finally:
        if blocked.exists():
            blocked.chmod(0o755)


def collect():
    """Return independently selectable file/argument leaves without registration."""
    cases = []
    def bind(function, *parameters):
        return lambda ctx: function(ctx, *parameters)
    def family(identity, names, function, *parameters):
        for name in names:
            label = {'.GiT': 'case-variant-git', '.ENV': 'case-variant-env'}.get(name, name)
            suffix = '.'.join((*map(str, parameters), label))
            cases.append(Case(identity + '.' + suffix, identity + ': ' + suffix,
                              bind(function, *parameters, name)))
    for tool in TOOLS:
        family('T01', ('array', 'null-object', 'scalar', 'incomplete', 'bad-json', 'trailing',
                        'unknown', 'wrong-type', 'missing', 'null-path'), argument, tool)
    for tool, field, least in [('list_files', 'offset', 0), ('list_files', 'limit', 1),
                               ('read_file', 'start_line', 1), ('read_file', 'max_lines', 1),
                               ('search_text', 'max_results', 1)]:
        # Keep the minimum out of the stable leaf ID.
        for name in ('omitted', 'null', 'fraction', 'zero', 'negative', 'overflow', 'string'):
            cases.append(Case('T02.%s.%s.%s' % (tool, field, name),
                              'optional integer %s.%s: %s' % (tool, field, name),
                              bind(optional, tool, field, least, name)))
    for tool, field in [('search_text', 'regex'), ('delete_file', 'recursive'), ('edit_file', 'replace_all')]:
        for name in ('omitted', 'null', 'fraction', 'zero', 'negative', 'overflow', 'string', 'false', 'true'):
            cases.append(Case('T02.%s.%s.%s' % (tool, field, name),
                              'optional bool %s.%s: %s' % (tool, field, name),
                              bind(optional, tool, field, 0, name)))
    family('T03', ('relative', 'root', 'empty', 'absolute', 'outside', 'parent', 'nul', 'unicode',
                    '.git', '.GiT', '.env', '.ENV', '.env.local', '.EnV.SECRET', 'environment'), paths)
    family('T04', ('inside', 'outside', 'broken', 'ancestor', 'root-alias'), symlinks)
    cases.append(Case('T04.replaced-root', 'recursive deletion refuses a replaced workspace identity', replaced_root))
    family('T05', ('default', 'page', 'continuation', 'end', 'past-end'), listing)
    cases.append(Case('T05.invalid-name', 'invalid UTF-8 directory entry is counted and skipped',
                      bind(listing, 'invalid-name')))
    family('T06', ('empty', 'lf', 'crlf', 'no-lf', 'range', 'default-page', 'large-page', 'eof', 'bad-range'), reading)
    family('T07', ('nul', 'invalid-utf8', 'directory', 'fifo', 'missing', 'permission', 'limit', 'large', 'huge-line'), read_refusal)
    family('T08', ('literal', 'case', 'pipe', 'pipe-hint', 'alternation', 'anchors', 'unicode', 'classes', 'case-flag'), searching)
    family('T09', ('empty', 'lf', 'cr', 'nul', 'empty-regex', 'both', 'zero-paths', 'null-paths', 'many-paths',
                    'bad-path', 'one-path', 'twenty-paths', 'duplicates', 'order', 'unclosed', 'lookahead', 'backreference'), search_arguments)
    family('T09', ('named-capture',), searching)
    family('T10', ('excluded', 'binary', 'large', 'permission', 'symlink'), search_skips)
    family('T11', ('exact-max', 'byte-budget', 'multibyte'), search_boundary)
    family('T12', ('empty', 'nonempty', 'nested', 'unread', 'read', 'ranged-read', 'written'), writing)
    for tool in ('write_file', 'edit_file', 'delete_file'):
        family('T13', ('missing', 'correct', 'unknown', 'short', 'uppercase', 'nonhex', 'null'), digests, tool)
    family('T14', ('implicit', 'explicit', 'exec-created'), stale)
    for name in ('deleted', 'recreate'):
        cases.append(Case('T14.' + name, 'missing-file observation with agents enabled',
                          bind(stale, name), pending='A20 authorization driver dependency: missing-file reread/recreation authorization requires agents enabled; --agents cannot be combined with --scripted'))
    family('T15', ('missing', 'number', 'null', 'nul', 'limit', 'over-limit', 'parent-file', 'permission', 'mode'), write_refusal)
    family('T16', ('unique', 'missing', 'ambiguous', 'near', 'delete-match'), edit)
    family('T17', ('snapshot', 'atomic', 'overlap', 'all-identity', 'mixed-identity', 'replace-all', 'replace-all-nonoverlapping', 'identity-ambiguous'), edit)
    family('T18', ('append', 'append-no-lf', 'batch-append', 'empty-append', 'null-append', 'mixed-forms', 'missing-new', 'empty-edits'), edit)
    family('T19', ('create', 'symlink', 'unread', 'stale', 'large-input', 'nul-input', 'permission'), edit_guard)
    family('T19', ('nul-result', 'large-result'), edit)
    family('T20', ('unread', 'read', 'explicit', 'stale', 'large-binary', 'exec-created'), deleting)
    family('T21', ('directory', 'directory-off', 'link', 'link-off', 'outside-link', 'root', 'withheld',
                    'special', 'ancestor', 'digest-on-dir'), delete_tree)
    family('T22', ('safe', 'long-ledger', 'partial-permission'), recursive)
    return cases

"""Certify guarded ls-files debug output against a pre-command binary index.

index_projection returns only the stdout and index_snapshot field replacements.
The caller must allow the extra helper field, retain the original artifact, and
ledger both replacements as git-index-stat-cache. The snapshot replacement is an
explicit semantic projection: entries/extensions are retained, stat-cache fields
are zeroed, and the verified checksum and zero padding are derived, not payload.
Large projections retain those same parsed rows as checksummed zlib/base64 JSON.
A pre-command missing-index certificate permits only successful empty stdout;
it stays literal because it has no volatile bytes. No unguarded command or
arbitrary stdout is eligible.
"""
import base64
import binascii
import hashlib
import json
import posixpath
import struct
import zlib


KIND = 'git-index-stat-cache'
MAX_INDEX_BYTES = 32 * 1024 * 1024
DEBUG = ['ls-files', '--stage', '--debug', '-z', '--']
EXCLUDES = [':(exclude,glob,icase)**/' + name + suffix
            for name in ('.git', '.env', '.env.*') for suffix in ('', '/**')]
VOLATILE = ('ctime', 'mtime', 'dev', 'ino', 'uid', 'gid')


def decode_snapshot(snapshot):
    """Decode either exact snapshot encoding, bounding decompression and integrity."""
    compressed = isinstance(snapshot, dict)
    if compressed:
        if (set(snapshot) != {'encoding', 'raw_size', 'raw_sha256', 'data'}
                or snapshot['encoding'] != 'zlib+base64' or type(snapshot['raw_size']) is not int
                or not 0 <= snapshot['raw_size'] <= MAX_INDEX_BYTES
                or not isinstance(snapshot['raw_sha256'], str)
                or len(snapshot['raw_sha256']) != 64
                or any(c not in '0123456789abcdef' for c in snapshot['raw_sha256'])):
            raise AssertionError('index snapshot compressed certificate is invalid')
        encoded = snapshot['data']
    else:
        encoded = snapshot
    if not isinstance(encoded, str) or len(encoded) > 4 * ((MAX_INDEX_BYTES + 2) // 3):
        raise AssertionError('index snapshot encoded size or type is invalid')
    try:
        raw = base64.b64decode(encoded, validate=True)
    except (binascii.Error, ValueError, TypeError) as error:
        raise AssertionError('index snapshot is not base64') from error
    if base64.b64encode(raw).decode() != encoded:
        raise AssertionError('index snapshot is not canonical base64')
    if not compressed:
        if len(raw) > MAX_INDEX_BYTES:
            raise AssertionError('index snapshot exceeds byte bound')
        return raw
    inflater = zlib.decompressobj()
    try:
        raw = inflater.decompress(raw, snapshot['raw_size'] + 1)
    except zlib.error as error:
        raise AssertionError('index snapshot compressed stream is invalid') from error
    if (len(raw) != snapshot['raw_size'] or not inflater.eof
            or inflater.unused_data or inflater.unconsumed_tail
            or hashlib.sha256(raw).hexdigest() != snapshot['raw_sha256']):
        raise AssertionError('index snapshot compressed size, stream or checksum differs')
    return raw


def parse_index(encoded):
    """Parse a complete checksummed v2/v3 SHA-1/SHA-256 index independently."""
    raw = decode_snapshot(encoded)
    algorithms = [name for name in ('sha1', 'sha256')
                  if len(raw) >= 12 + hashlib.new(name).digest_size
                  and hashlib.new(name, raw[:-hashlib.new(name).digest_size]).digest()
                  == raw[-hashlib.new(name).digest_size:]]
    if len(algorithms) != 1:
        raise AssertionError('index snapshot checksum is invalid or ambiguous')
    algorithm = algorithms[0]
    width = hashlib.new(algorithm).digest_size
    signature, version, count = struct.unpack_from('>4sII', raw)
    if signature != b'DIRC' or version not in (2, 3):
        raise AssertionError('index snapshot needs DIRC version 2 or 3')
    limit, offset, entries = len(raw) - width, 12, []
    for _ in range(count):
        start = offset
        if offset + 42 + width > limit:
            raise AssertionError('index snapshot entry is truncated')
        stats = struct.unpack_from('>10I', raw, offset)
        offset += 40
        oid = raw[offset:offset + width].hex()
        offset += width
        flags, = struct.unpack_from('>H', raw, offset)
        offset += 2
        extended = 0
        if flags & 0x4000:
            if version != 3 or offset + 2 > limit:
                raise AssertionError('index snapshot extended flags are invalid')
            extended, = struct.unpack_from('>H', raw, offset)
            offset += 2
            if extended & ~0x6000:
                raise AssertionError('index snapshot has unsupported extended flags')
        end = raw.find(b'\0', offset, limit)
        if end < 0:
            raise AssertionError('index snapshot entry lacks a path terminator')
        path = raw[offset:end]
        if (not path or path.startswith(b'/') or any(p in (b'', b'.', b'..') for p in path.split(b'/'))
                or flags & 0xfff != min(len(path), 0xfff)):
            raise AssertionError('index snapshot path or name length is invalid')
        offset = start + ((end + 1 - start + 7) // 8) * 8
        if offset > limit or any(raw[end:offset]):
            raise AssertionError('index snapshot entry padding is invalid')
        if stats[1] >= 1000000000 or stats[3] >= 1000000000:
            raise AssertionError('index snapshot nanoseconds exceed numeric bounds')
        if stats[6] not in (0o100644, 0o100755, 0o120000, 0o160000):
            raise AssertionError('index snapshot mode is unsupported')
        entry = {'path': base64.b64encode(path).decode(), 'oid': oid, 'mode': stats[6],
                 'flags': flags, 'extended_flags': extended, 'ctime': list(stats[:2]),
                 'mtime': list(stats[2:4]), 'dev': stats[4], 'ino': stats[5],
                 'uid': stats[7], 'gid': stats[8], 'size': stats[9]}
        key = (path, (flags >> 12) & 3)
        if entries:
            previous = entries[-1]
            if key <= (base64.b64decode(previous['path']), (previous['flags'] >> 12) & 3):
                raise AssertionError('index snapshot entries are not ordered and unique')
        entries.append(entry)
    extensions = []
    while offset < limit:
        if offset + 8 > limit:
            raise AssertionError('index snapshot extension is truncated')
        signature, size = struct.unpack_from('>4sI', raw, offset)
        offset += 8
        if not 65 <= signature[0] <= 90 or offset + size > limit:
            raise AssertionError('index snapshot required or truncated extension is unsupported')
        extensions.append({'signature': base64.b64encode(signature).decode(),
                           'data': base64.b64encode(raw[offset:offset + size]).decode()})
        offset += size
    return {'version': version, 'object_format': algorithm, 'entries': entries, 'extensions': extensions}


def debug_output(entries):
    """Render Git's exact -z --stage --debug format from independent entries."""
    parts = []
    for entry in entries:
        flags = entry['flags'] & 0xf000 | entry['extended_flags'] << 16
        parts.extend([f"{entry['mode']:06o} {entry['oid']} {(entry['flags'] >> 12) & 3}\t".encode(),
                      base64.b64decode(entry['path']), b'\0',
                      (f"  ctime: {entry['ctime'][0]}:{entry['ctime'][1]}\n"
                       f"  mtime: {entry['mtime'][0]}:{entry['mtime'][1]}\n"
                       f"  dev: {entry['dev']}\tino: {entry['ino']}\n"
                       f"  uid: {entry['uid']}\tgid: {entry['gid']}\n"
                       f"  size: {entry['size']}\tflags: {flags:x}\n").encode()])
    return b''.join(parts).decode('utf-8', 'replace')


def index_projection(record, inspect_prefix):
    """Validate a helper record and return its two explicitly typed projections."""
    argv = record.get('argv')
    prefix = inspect_prefix + DEBUG
    guarded = isinstance(argv, list) and argv[:len(prefix)] == prefix
    if not guarded:
        if 'index_snapshot' in record:
            raise AssertionError('index snapshot belongs to a different command')
        return {}
    if (set(record) != {'argv', 'exit_code', 'stdout', 'stderr', 'index_snapshot'}
            or type(record['exit_code']) is not int or record['exit_code'] != 0
            or not isinstance(record['stderr'], str) or not isinstance(record['stdout'], str)):
        raise AssertionError('index snapshot requires one successful complete guarded result')
    scope = argv[len(prefix):]
    ancestor = (len(scope) == 2 and isinstance(scope[0], str) and scope[0].startswith(':(literal)')
                and scope[1] == ':(exclude,glob)' + scope[0][10:] + '/**')
    if ancestor:
        paths = scope[:1]
    elif len(scope) > len(EXCLUDES) and scope[-len(EXCLUDES):] == EXCLUDES:
        paths = scope[:-len(EXCLUDES)]
    else:
        raise AssertionError('index snapshot command scope is not the inspection protocol')
    for path in paths:
        if not isinstance(path, str):
            raise AssertionError('index snapshot command path is not a string')
        if path == '.':
            continue
        literal = path.removeprefix(':(literal)')
        if (literal == path or not literal or literal.startswith('/')
                or posixpath.normpath(literal) != literal or literal == '..' or literal.startswith('../')):
            raise AssertionError('index snapshot command path is not canonical literal scope')

    def selected(entry):
        path = base64.b64decode(entry['path']).decode('utf-8', 'surrogateescape')
        if ancestor:
            return path == paths[0][10:]
        if any(part.lower() in ('.git', '.env') or part.lower().startswith('.env.') for part in path.split('/')):
            return False
        return any(p == '.' or path == p[10:] or path.startswith(p[10:] + '/') for p in paths)

    if record['index_snapshot'] == {'missing': True} and type(record['index_snapshot']['missing']) is bool:
        if record['stdout'] != '':
            raise AssertionError('missing index certificate requires empty debug output')
        return {}
    snapshot = parse_index(record['index_snapshot'])
    entries, offset = [], 0
    # Git pathspec pruning can omit scoped entries. Preserve omissions literally:
    # only rows actually emitted are zeroed, and every emitted byte must certify.
    for entry in snapshot['entries']:
        if not selected(entry):
            continue
        expected = debug_output([entry])
        if record['stdout'].startswith(expected, offset):
            entries.append(entry)
            offset += len(expected)
    if offset != len(record['stdout']):
        raise AssertionError('ls-files debug output differs from independent index snapshot')
    for entry in snapshot['entries']:
        for field in VOLATILE:
            entry[field] = [0, 0] if field in ('ctime', 'mtime') else 0
    if len(decode_snapshot(record['index_snapshot'])) > 64 * 1024:
        projected = json.dumps(snapshot, sort_keys=True, separators=(',', ':')).encode()
        snapshot = {'encoding': 'zlib+base64-json', 'raw_size': len(projected),
                    'raw_sha256': hashlib.sha256(projected).hexdigest(),
                    'data': base64.b64encode(zlib.compress(projected)).decode()}
    return {'stdout': debug_output(entries), 'index_snapshot': snapshot}

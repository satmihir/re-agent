import base64
import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib

from parity.observe import Normalizer, go_json
from parity.observe_tools import INSPECT_PREFIX, Tools
from parity.observe_tools_index import (DEBUG, EXCLUDES, KIND, MAX_INDEX_BYTES, VOLATILE,
                                       decode_snapshot, index_projection, parse_index)


FIXTURE = Path(__file__).with_name('fixtures') / 'git_command.py'
GIT = shutil.which('git')


class IndexSnapshotTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='parity-index-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        self.env = dict(os.environ, HOME=str(self.root), XDG_CONFIG_HOME=str(self.root),
                        GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull,
                        GIT_TERMINAL_PROMPT='0', LC_ALL='C')
        for key in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_CONFIG_COUNT', 'GIT_OBJECT_DIRECTORY'):
            self.env.pop(key, None)
        self.helper = self.root / 'git-helper'
        self.helper.write_text(FIXTURE.read_text())
        self.log = self.root / 'commands.jsonl'
        self.settings = {'real_git': GIT, 'fixed_env': {}, 'git_record': str(self.log)}
        self.save_settings()

    def save_settings(self):
        (self.root / 'settings.json').write_text(json.dumps(self.settings))

    def git(self, *args, input=None):
        return subprocess.run([GIT, *args], cwd=self.repo, env=self.env, input=input,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True).stdout

    def init(self, algorithm='sha1', version=2):
        self.git('init', '-q', '--object-format=' + algorithm)
        for path in ('plain', 'dir/a\tb\n雪', '.env', 'nested/.ENV.local', '.environment'):
            target = self.repo / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text('independent file content\n')
        self.git('add', '.')
        if version == 3:
            self.git('update-index', '--skip-worktree', 'plain')
            self.git('update-index', '--assume-unchanged', '.environment')
            (self.repo / 'intent').write_text('not yet staged\n')
            self.git('add', '-N', 'intent')
        self.git('update-index', '--index-version=' + str(version))

    def capture(self, paths=('.',), index_path=None):
        argv = INSPECT_PREFIX + DEBUG + list(paths) + EXCLUDES
        index_path = index_path or self.repo / '.git/index'
        before = index_path.read_bytes() if index_path.exists() else None
        result = subprocess.run([sys.executable, str(self.helper), *argv[1:]], cwd=self.repo,
                                env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
        record = json.loads(self.log.read_text().splitlines()[-1])
        self.assertEqual(record['stdout'], result.stdout.decode('utf-8', 'replace'))
        self.assertEqual(record['stderr'], result.stderr.decode('utf-8', 'replace'))
        if before is None:
            self.assertEqual(record['index_snapshot'], {'missing': True})
        else:
            self.assertEqual(decode_snapshot(record['index_snapshot']), before)
        return record

    def test_unborn_and_removed_index_preserve_successful_empty_debug(self):
        self.git('init', '-q')
        record = self.capture()
        self.assertEqual(record['stdout'], '')
        self.assertEqual(index_projection(record, INSPECT_PREFIX), {})
        (self.repo / 'plain').write_text('tracked\n')
        self.git('add', 'plain')
        (self.repo / '.git/index').unlink()
        record = self.capture()
        self.assertEqual(record['stdout'], '')
        self.assertEqual(index_projection(record, INSPECT_PREFIX), {})

    def test_linked_worktree_absolute_and_relative_gitdir_snapshots(self):
        self.init()
        self.git('-c', 'user.name=Index Test', '-c', 'user.email=index@example.invalid',
                 '-c', 'commit.gpgsign=false', 'commit', '-q', '-m', 'fixture')
        target = self.root / 'linked'
        self.git('worktree', 'add', '-q', '-b', 'linked', str(target))
        self.repo = target
        index_path = Path(self.git('rev-parse', '--git-path', 'index').decode().strip())
        record = self.capture(index_path=index_path)
        self.assertIn('plain\0', index_projection(record, INSPECT_PREFIX)['stdout'])
        relative = os.path.relpath(index_path.parent.resolve(), target.resolve())
        (target / '.git').write_text('gitdir: ' + relative + '\n')
        record = self.capture(index_path=index_path)
        self.assertIn('plain\0', index_projection(record, INSPECT_PREFIX)['stdout'])

    def test_real_git_sha1_sha256_v2_v3_complete_output_and_stable_projection(self):
        for algorithm in ('sha1', 'sha256'):
            for version in (2, 3):
                with self.subTest(algorithm=algorithm, version=version):
                    if (self.repo / '.git').exists():
                        shutil.rmtree(self.repo / '.git')
                    self.init(algorithm, version)
                    record = self.capture()
                    projection = index_projection(record, INSPECT_PREFIX)
                    snapshot = projection['index_snapshot']
                    self.assertEqual(snapshot['object_format'], algorithm)
                    self.assertEqual(snapshot['version'], version)
                    self.assertIn('\t', projection['stdout'])
                    self.assertIn('dir/a\tb\n雪\0', projection['stdout'])
                    self.assertNotIn('\t.env\0', projection['stdout'])
                    self.assertNotIn('nested/.ENV.local\0', projection['stdout'])
                    original = parse_index(record['index_snapshot'])
                    for raw, stable in zip(original['entries'], snapshot['entries']):
                        self.assertEqual({k: v for k, v in raw.items() if k not in VOLATILE},
                                         {k: v for k, v in stable.items() if k not in VOLATILE})
                        for field in VOLATILE:
                            self.assertEqual(stable[field], [0, 0] if field in ('ctime', 'mtime') else 0)
                    self.assertEqual(original['extensions'], snapshot['extensions'])
                    self.assertEqual(record['index_snapshot'], base64.b64encode((self.repo / '.git/index').read_bytes()).decode())

    def test_only_volatile_binary_stat_fields_change_the_projection(self):
        self.init(version=3)
        first = self.capture()
        snapshot = parse_index(first['index_snapshot'])
        raw = bytearray(base64.b64decode(first['index_snapshot'])[:-20])
        offset = 12
        for entry in snapshot['entries']:
            for field, value in ((0, 1), (4, 999999999), (8, 2), (12, 999999998),
                                 (16, 0xffffffff), (20, 0xffffffff), (28, 0xffffffff), (32, 0xffffffff)):
                struct.pack_into('>I', raw, offset + field, value)
            length = 62 + (2 if entry['flags'] & 0x4000 else 0) + len(base64.b64decode(entry['path'])) + 1
            offset += (length + 7) // 8 * 8
        (self.repo / '.git/index').write_bytes(bytes(raw) + hashlib.sha1(raw).digest())
        second = self.capture()
        self.assertNotEqual(first['stdout'], second['stdout'])
        self.assertNotEqual(first['index_snapshot'], second['index_snapshot'])
        self.assertEqual(index_projection(first, INSPECT_PREFIX), index_projection(second, INSPECT_PREFIX))

    def test_literal_file_and_directory_scopes_are_independently_selected(self):
        self.init()
        for paths in [(':(literal)plain',), (':(literal)dir',), (':(literal)missing',),
                      (':(literal)dir/a\tb\n雪', ':(literal).environment')]:
            with self.subTest(paths=paths):
                record = self.capture(paths)
                projection = index_projection(record, INSPECT_PREFIX)
                self.assertEqual(record['stdout'].count('\0'), projection['stdout'].count('\0'))

    def test_generated_literal_scope_and_ancestor_exclusion_match_real_git(self):
        self.init()
        for name in ('generated/00', 'generated/01', 'generated/nested/02'):
            target = self.repo / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text('generated content\n')
        self.git('add', 'generated')
        record = self.capture((':(literal)generated/00',))
        # Git may prune this entire scoped inventory with the fixed exclusions.
        # The certificate must preserve that omission rather than fill in rows.
        self.assertEqual(index_projection(record, INSPECT_PREFIX)['stdout'].count('\0'), record['stdout'].count('\0'))
        argv = INSPECT_PREFIX + DEBUG + [':(literal)generated', ':(exclude,glob)generated/**']
        output = self.git(*argv[1:]).decode()
        ancestor = dict(record, argv=argv, stdout=output)
        self.assertEqual(index_projection(ancestor, INSPECT_PREFIX)['stdout'], output)
        self.assertEqual(output, '')

    def test_large_inventory_compression_preserves_raw_and_normalized_budgets(self):
        self.init()
        # Match _inventory: real files precede the generated inventory paths.
        self.git('mv', 'plain', 'a.txt')
        oid = self.git('rev-parse', ':a.txt').strip().decode()
        prefix = 'inventory/' + '/'.join(['a' * 220, 'b' * 220, 'c' * 220, 'd' * 222])
        inventory = ''.join(f'100644 {oid}\t{prefix}{n:05d}\n' for n in range(10000))
        self.git('update-index', '--index-info', input=inventory.encode())
        first = self.capture()
        self.capture((':(literal)a.txt',))
        self.capture((':(literal)a.txt',))
        self.assertEqual(first['index_snapshot']['encoding'], 'zlib+base64')
        original = (self.repo / '.git/index').read_bytes()
        self.assertEqual(decode_snapshot(first['index_snapshot']), original)
        self.assertLess(len(go_json(first['index_snapshot'])), len(original) // 10)
        old_encoding = dict(first, index_snapshot=base64.b64encode(original).decode())
        self.assertEqual(index_projection(first, INSPECT_PREFIX), index_projection(old_encoding, INSPECT_PREFIX))
        raw = self.log.read_bytes()
        normal = Normalizer(str(self.root), '127.0.0.1:34567')
        tools = Tools(normal, [], [], case_id='T38.inventory.scoped-recovery',
                      observations={'git/git-commands.jsonl': raw})
        normalized = tools.artifact(raw, 'git/git-commands.jsonl')
        self.assertLess(len(raw), 32 * 1024 * 1024)
        self.assertLess(len(normalized), 32 * 1024 * 1024)
        for line in normalized.splitlines():
            projected = json.loads(line)['index_snapshot']
            self.assertEqual(projected['encoding'], 'zlib+base64-json')
            payload = zlib.decompress(base64.b64decode(projected['data']))
            self.assertEqual(len(payload), projected['raw_size'])
            self.assertEqual(hashlib.sha256(payload).hexdigest(), projected['raw_sha256'])
            entries = json.loads(payload)['entries']
            self.assertEqual(len(entries), 10005)
            for entry in entries:
                self.assertEqual(entry['ctime'], [0, 0])
                self.assertEqual(entry['ino'], 0)

    def test_changed_output_cannot_be_laundered_by_an_independent_snapshot(self):
        self.init(version=3)
        record = self.capture((':(literal)plain',))
        mutations = [
            lambda r: r.update(stdout=r['stdout'].replace('ctime: ', 'ctime: 1', 1)),
            lambda r: r.update(stdout=r['stdout'].replace('mtime: ', 'mtime: 1', 1)),
            lambda r: r.update(stdout=r['stdout'].replace('ino: ', 'ino: 1', 1)),
            lambda r: r.update(stdout=r['stdout'].replace('dev: ', 'dev: 1', 1)),
            lambda r: r.update(stdout=r['stdout'].replace('uid: ', 'uid: 1', 1)),
            lambda r: r.update(stdout=r['stdout'].replace('gid: ', 'gid: 1', 1)),
            lambda r: r.update(stdout=r['stdout'].replace('100644 ', '100755 ', 1)),
            lambda r: r.update(stdout=r['stdout'].replace('plain\0', 'other\0')),
            lambda r: r.update(stdout=r['stdout'].replace('flags: ', 'flags: 1', 1)),
            lambda r: r.update(stdout=r['stdout'].replace('size: ', 'size: 1', 1)),
            lambda r: r.update(stdout=r['stdout'].replace(' 0\t', ' 1\t', 1)),
            lambda r: r.update(stdout=r['stdout'] + 'arbitrary output\n'),
            lambda r: r.update(stdout=r['stdout'].replace(' ', '  ', 1)),
            lambda r: r.update(stdout=r['stdout'][:7] + '0' * 40 + r['stdout'][47:]),
            lambda r: r['argv'].__setitem__(1, '--pager'),
            lambda r: r['argv'].__setitem__(-len(EXCLUDES)-1, ':(literal)missing'),
            lambda r: r.update(exit_code=True),
            lambda r: r.update(exit_code=1),
            lambda r: r.pop('index_snapshot'),
            lambda r: r.update(extra='unaccounted'),
        ]
        for mutate in mutations:
            forged = copy.deepcopy(record)
            mutate(forged)
            with self.subTest(record=forged), self.assertRaises(AssertionError):
                index_projection(forged, INSPECT_PREFIX)

    def test_unrelated_commands_and_arbitrary_output_remain_exact(self):
        self.init()
        record = self.capture()
        for argv in (['git', 'show'], ['git', 'ls-files', '--stage', '--debug', '-z', '--', '.'],
                     INSPECT_PREFIX + ['show']):
            unrelated = dict(record, argv=argv)
            unrelated.pop('index_snapshot')
            before = copy.deepcopy(unrelated)
            self.assertEqual(index_projection(unrelated, INSPECT_PREFIX), {})
            self.assertEqual(unrelated, before)
            unrelated['index_snapshot'] = record['index_snapshot']
            with self.assertRaises(AssertionError):
                index_projection(unrelated, INSPECT_PREFIX)

    def test_helper_snapshots_before_real_command_and_only_exact_guarded_argv(self):
        self.init()
        before = (self.repo / '.git/index').read_bytes()
        program = self.root / 'mutating-git'
        program.write_text('#!' + sys.executable + '\nfrom pathlib import Path\n'
                           'Path(".git/index").write_bytes(b"changed after snapshot")\nprint("literal output")\n')
        program.chmod(0o700)
        self.settings['real_git'] = str(program)
        self.save_settings()
        record = self.capture()
        self.assertEqual(base64.b64decode(record['index_snapshot']), before)
        self.assertEqual((self.repo / '.git/index').read_bytes(), b'changed after snapshot')
        with self.assertRaises(AssertionError):
            index_projection(record, INSPECT_PREFIX)
        for argv in (['show'], ['ls-files', '--stage', '--debug', '-z', '--', '.']):
            subprocess.run([sys.executable, str(self.helper), *argv], cwd=self.repo,
                           env=self.env, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.assertNotIn('index_snapshot', json.loads(self.log.read_text().splitlines()[-1]))

    def test_conflict_stages_and_optional_extensions_are_preserved(self):
        self.init()
        oid = self.git('hash-object', '-w', '--stdin', input=b'conflict').strip().decode()
        self.git('update-index', '--index-info', input=''.join(f'100644 {oid} {s}\tconflict\n' for s in (1, 2, 3)).encode())
        raw = (self.repo / '.git/index').read_bytes()
        extension = b'TEST' + struct.pack('>I', 8) + b'payload!'
        body = raw[:-20] + extension
        (self.repo / '.git/index').write_bytes(body + hashlib.sha1(body).digest())
        record = self.capture()
        projection = index_projection(record, INSPECT_PREFIX)
        self.assertNotIn('stderr', projection)
        self.assertEqual([e['flags'] >> 12 & 3 for e in projection['index_snapshot']['entries']
                          if base64.b64decode(e['path']) == b'conflict'], [1, 2, 3])
        self.assertEqual(projection['index_snapshot']['extensions'][-1]['data'], base64.b64encode(b'payload!').decode())

    def test_malformed_snapshot_and_numeric_bounds_fail_closed(self):
        self.init()
        record = self.capture()
        raw = base64.b64decode(record['index_snapshot'])

        def mutate(offset, value):
            body = bytearray(raw[:-20])
            body[offset:offset + len(value)] = value
            return base64.b64encode(bytes(body) + hashlib.sha1(body).digest()).decode()

        snapshots = ['not base64!', '', base64.b64encode(raw[:-1]).decode(),
                     base64.b64encode(raw + b'\0').decode(),
                     mutate(0, b'NOPE'), mutate(4, struct.pack('>I', 4)),
                     mutate(8, struct.pack('>I', 0xffffffff)),
                     mutate(16, struct.pack('>I', 1000000000)),
                     mutate(24, struct.pack('>I', 0xffffffff)),
                     mutate(38, b'\xff\xff'), mutate(72, b'\xff\xff'),
                     mutate(len(raw)-21, b'\xff')]
        for encoded in snapshots:
            with self.subTest(encoded=encoded[:60]), self.assertRaises(AssertionError):
                index_projection(dict(record, index_snapshot=encoded), INSPECT_PREFIX)


class IndexArtifactTest(unittest.TestCase):
    def record(self):
        path, oid = b'plain', bytes.fromhex('ab' * 20)
        entry = struct.pack('>10I', 11, 12, 13, 14, 15, 16, 0o100644, 17, 18, 19)
        entry += oid + struct.pack('>H', len(path)) + path + b'\0'
        entry += b'\0' * (-len(entry) % 8)
        body = struct.pack('>4sII', b'DIRC', 2, 1) + entry
        return {'argv': INSPECT_PREFIX + DEBUG + ['.'] + EXCLUDES, 'exit_code': 0,
                'stdout': '100644 ' + oid.hex() + ' 0\tplain\0  ctime: 11:12\n  mtime: 13:14\n'
                          '  dev: 15\tino: 16\n  uid: 17\tgid: 18\n  size: 19\tflags: 0\n',
                'stderr': '', 'index_snapshot': base64.b64encode(body + hashlib.sha1(body).digest()).decode()}

    def observer(self, raw, artifact='git/git-commands.jsonl', case='T38.inspect.sha1'):
        normal = Normalizer('/tmp/parity-index-artifact', '127.0.0.1:34567')
        observations = {artifact: raw}
        return normal, Tools(normal, [], [], case_id=case, observations=observations)

    def test_artifact_certifies_both_slots_without_touching_other_bytes(self):
        record = self.record()
        other = {'argv': ['git', 'show'], 'exit_code': 0, 'stdout': record['stdout'],
                 'stderr': 'ctime: 11:12 inode: 16\n'}
        prefix = json.dumps(other, separators=(', ', ' : ')).encode() + b'\n'
        line = json.dumps(record, separators=(', ', ' : ')).encode() + b'\n'
        line = line.replace(b'ctime', b'c\\u0074ime')
        raw = prefix + line
        original = copy.deepcopy(record)
        normal, tools = self.observer(raw)
        actual = tools.artifact(raw, 'git/git-commands.jsonl')
        expected_line = line.replace(b'11:12', b'0:0').replace(b'13:14', b'0:0')
        for field, value in (('dev', 15), ('ino', 16), ('uid', 17), ('gid', 18)):
            expected_line = expected_line.replace(f'{field}: {value}'.encode(), f'{field}: 0'.encode())
        snapshot = index_projection(record, INSPECT_PREFIX)['index_snapshot']
        expected_line = expected_line.replace(go_json(record['index_snapshot']), go_json(snapshot))
        self.assertEqual(actual, prefix + expected_line)
        self.assertEqual(tools.observations['git/git-commands.jsonl'], raw)
        self.assertEqual(record, original)
        self.assertEqual([row['kind'] for row in normal.ledger], [KIND, KIND])
        self.assertEqual([row['path'] for row in normal.ledger], [[1, 'stdout'], [1, 'index_snapshot']])
        for row in normal.ledger:
            self.assertEqual(base64.b64decode(row['before']), raw[row['start']:row['end']])
        self.assertIn(b'c\\u0074ime: 0:0', actual)
        self.assertEqual(json.loads(actual.splitlines()[1])['index_snapshot'], snapshot)

    def test_arbitrary_output_and_unregistered_artifacts_remain_literal(self):
        record = self.record()
        record.pop('index_snapshot')
        for argv in (['git', 'show'], ['git', 'ls-files', '--stage', '--debug', '-z', '--', '.']):
            raw = json.dumps(dict(record, argv=argv)).encode() + b'\n'
            normal, tools = self.observer(raw)
            self.assertEqual(tools.artifact(raw, 'git/git-commands.jsonl'), raw)
            self.assertEqual(normal.ledger, [])
        for artifact, case in (('git/other.jsonl', 'T38.inspect.sha1'), ('git/git-commands.jsonl', 'T99.unknown')):
            raw = go_json(self.record()) + b'\n'
            normal, tools = self.observer(raw, artifact, case)
            self.assertEqual(tools.artifact(raw, artifact), raw)
            self.assertEqual(normal.ledger, [])

    def test_compressed_and_original_certificates_have_identical_projections(self):
        record = self.record()
        raw = decode_snapshot(record['index_snapshot'])
        compressed = {'encoding': 'zlib+base64', 'raw_size': len(raw),
                      'raw_sha256': hashlib.sha256(raw).hexdigest(),
                      'data': base64.b64encode(zlib.compress(raw)).decode()}
        expected = index_projection(record, INSPECT_PREFIX)
        self.assertEqual(decode_snapshot(compressed), raw)
        self.assertEqual(index_projection(dict(record, index_snapshot=compressed), INSPECT_PREFIX), expected)
        mutations = [
            {'encoding': 'gzip+base64'}, {'raw_size': True}, {'raw_size': -1},
            {'raw_size': MAX_INDEX_BYTES + 1}, {'raw_size': len(raw) - 1}, {'raw_size': len(raw) + 1},
            {'raw_sha256': '0' * 64}, {'raw_sha256': 'A' * 64}, {'extra': 'unaccounted'},
            {'data': 'not base64!'}, {'data': base64.b64encode(zlib.compress(raw)[:-1]).decode()},
            {'data': base64.b64encode(zlib.compress(raw) + b'extra').decode()},
            {'data': base64.b64encode(zlib.compress(raw) + zlib.compress(raw)).decode()},
            {'data': base64.b64encode(zlib.compress(b'x' * 1000000)).decode()},
        ]
        for values in mutations:
            with self.subTest(values=values), self.assertRaises(AssertionError):
                index_projection(dict(record, index_snapshot=dict(compressed, **values)), INSPECT_PREFIX)

    def test_omitted_rows_remain_observable_after_normalization(self):
        record = self.record()
        expected = index_projection(record, INSPECT_PREFIX)
        omitted = index_projection(dict(record, stdout=''), INSPECT_PREFIX)
        self.assertEqual(omitted['stdout'], '')
        self.assertEqual(omitted['index_snapshot'], expected['index_snapshot'])
        self.assertNotEqual(omitted, expected)

    def test_missing_index_certificate_is_literal_and_cannot_hide_output(self):
        record = dict(self.record(), index_snapshot={'missing': True}, stdout='')
        raw = go_json(record) + b'\n'
        normal, tools = self.observer(raw)
        self.assertEqual(tools.artifact(raw, 'git/git-commands.jsonl'), raw)
        self.assertEqual(normal.ledger, [])
        for values in ({'stdout': self.record()['stdout']}, {'index_snapshot': {'missing': 1}},
                       {'index_snapshot': {'missing': True, 'extra': True}}, {'exit_code': 1}):
            forged = go_json(dict(record, **values)) + b'\n'
            normal, tools = self.observer(forged)
            with self.subTest(values=values), self.assertRaises(AssertionError):
                tools.artifact(forged, 'git/git-commands.jsonl')
            self.assertEqual(normal.ledger, [])

    def test_forged_witness_or_schema_is_rejected_before_recording_slots(self):
        record = self.record()
        mutations = [
            lambda r: r.update(index_snapshot='forged'),
            lambda r: r.update(stdout=r['stdout'].replace('11:12', '21:12')),
            lambda r: r.update(stdout=r['stdout'].replace('ino: 16', 'ino: 26')),
            lambda r: r.update(argv=['git', 'show']),
            lambda r: r.pop('index_snapshot'),
            lambda r: r.update(extra='unaccounted'),
            lambda r: r.update(exit_code=True),
            lambda r: r.update(stderr=1),
            lambda r: r.update(argv=[1]),
        ]
        for mutate in mutations:
            forged = copy.deepcopy(record)
            mutate(forged)
            raw = go_json(forged) + b'\n'
            normal, tools = self.observer(raw)
            with self.subTest(record=forged), self.assertRaises(AssertionError):
                tools.artifact(raw, 'git/git-commands.jsonl')
            self.assertEqual(normal.ledger, [])
        raw = go_json(record) + b'\n'
        normal, tools = self.observer(raw)
        with self.assertRaisesRegex(AssertionError, 'retained independent witness'):
            tools.artifact(raw + b' ', 'git/git-commands.jsonl')
        self.assertEqual(normal.ledger, [])


if __name__ == '__main__':
    unittest.main()

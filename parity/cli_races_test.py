"""Fail-closed C37 slot tests and opt-in executable retry/interrupt probes."""

import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch

from parity.cases import cli_pty_races
from parity.cases.cli_pty_races import collect
from parity.observe import Normalizer
from parity.observe_cli_races import CASE, PREAMBLE, Races, validate_shell
from parity.run import Context


def fixture():
    command = "exec python3 /fixture/shell-helper.py"
    record = {"workspace": "/fixture/workspace", "kind": "user_shell_command", "command": command,
              "exit_code": None, "signal": "killed", "interrupted": True, "output": "shell helper ready\n",
              "output_bytes_seen": 19, "output_truncated": False, "encoding_replaced": False, "duration_ms": 102}
    witness = {"command": command, "foreground_group": True, "terminated": True, "tripwire_absent": True}
    return command, record, witness


class RaceSlotsTest(unittest.TestCase):
    def test_only_documented_signal_pairs(self):
        command, record, witness = fixture()
        for interrupted, signal in ((True, "killed"), (True, "interrupt"), (False, "interrupt")):
            with self.subTest(interrupted=interrupted, signal=signal):
                record.update(interrupted=interrupted, signal=signal)
                self.assertEqual(validate_shell(CASE, record, command, witness), record)

    def test_wrong_owner_shape_effect_and_termination_fail(self):
        command, record, witness = fixture()
        for changed in ({"signal": "terminated"}, {"exit_code": 130}, {"interrupted": False},
                        {"interrupted": 1}, {"output": "wrong\n"}, {"output_bytes_seen": 18},
                        {"output_truncated": True}, {"encoding_replaced": True}, {"extra": 1},
                        {"duration_ms": -1}, {"duration_ms": True}, {"duration_ms": 10001},
                        {"command": "different"}):
            with self.subTest(changed=changed), self.assertRaises(AssertionError):
                validate_shell(CASE, dict(record, **changed), command, witness)
        with self.assertRaises(AssertionError):
            validate_shell("C38.interrupt-running-command", record, command, witness)
        for key in ("foreground_group", "terminated", "tripwire_absent"):
            with self.subTest(witness=key), self.assertRaises(AssertionError):
                validate_shell(CASE, record, command, dict(witness, **{key: False}))

    def race(self, interrupted=True, signal='killed'):
        command, record, witness = fixture()
        record.update(interrupted=interrupted, signal=signal)
        event = {"type": "model.requested", "run_id": "run", "step": 1,
                 "data": {"history": [{"kind": "shell", "shell": record}]}}
        wire = {"model": "fixture", "input": [{"role": "user", "content": [{"type": "input_text", "text": PREAMBLE + json.dumps(record)}]}]}
        captures = [{"path": "/shell-ready", "method": "GET", "body": b""},
                    {"path": "/openai/v1/responses", "body": json.dumps(wire).encode()}]
        normal = Normalizer("/fixture", "localhost")
        races = Races(normal, [event], captures, [{"stdin": ("!" + command + "\r\x03next question\r").encode(),
                                             "started": 1, "ended": 2,
                                             "stderr": b"interrupted after 0.1s \xc2\xb7 added to the conversation\r\n"}],
                      CASE, {"cli-shell-process.json": json.dumps(witness).encode()})
        return races, normal, event, wire

    def test_ignored_sigint_fixture_rejects_otherwise_allowed_screen_races(self):
        for interrupted in (True, False):
            with self.subTest(interrupted=interrupted), self.assertRaisesRegex(AssertionError, 'requires cancellation kill'):
                self.race(interrupted=interrupted, signal='interrupt')

    def test_trace_and_wire_only_owned_fields_change(self):
        races, normal, event, wire = self.race()
        for value in (event, wire):
            raw = json.dumps(value).encode()
            result = json.loads(races.json(raw, raw, "owned"))
            if "data" in result:
                shell = result["data"]["history"][0]["shell"]
            else:
                shell = json.loads(result["input"][0]["content"][0]["text"][len(PREAMBLE):])
            self.assertEqual(shell, dict(fixture()[1], workspace='<ROOT>/workspace', signal="interrupt", duration_ms=0))
        self.assertEqual({entry["kind"] for entry in normal.ledger}, {"cli-shell-workspace", "cli-shell-termination", "cli-shell-elapsed"})

    def test_mutated_echo_fails(self):
        races, _, event, _ = self.race()
        event["data"]["history"][0]["shell"]["signal"] = "terminated"
        raw = json.dumps(event).encode()
        with self.assertRaises(AssertionError):
            races.json(raw, raw, "owned")

    def test_footer_is_exact_and_never_model_prose(self):
        races, normal, _, _ = self.race()
        footer = "interrupted after 0.1s · added to the conversation"
        self.assertEqual(races.text(footer + "\r\n", "invocation-1.stderr"), races.canonical_footer + "\r\n")
        for text in ("prefix " + footer, footer + " suffix", footer.replace("0.1", "8.5"), footer.replace("interrupted", "killed by terminated")):
            self.assertEqual(races.text(text, "invocation-1.stderr"), text)
        self.assertEqual(races.text(footer, "http-1.response.body"), footer)
        self.assertEqual(len(normal.ledger), 1)

    def test_case_counts_and_cancellation_is_runnable(self):
        cases = collect()
        self.assertEqual((len(cases), sum(not c.pending for c in cases)), (3, 3))
        self.assertEqual(cases[-1].id, 'C44.typeahead-cancellation')
        self.assertIs(cases[-1].run, cli_pty_races._cancel_typeahead)

    def test_cancellation_oracle_rejects_stale_input_and_missing_evidence(self):
        for defect in (None, 'queued-submission', 'stale-prompt', 'request-leak', 'trace-leak',
                       'cancelled-reply', 'delivered-response', 'missing-completion', 'wrong-run'):
            with self.subTest(defect=defect):
                response = SimpleNamespace(segments=[], chunks=[], completed=False)
                server = SimpleNamespace(requests=[], expect=lambda *_args: None)
                context = SimpleNamespace(server=server)
                trace = []
                session = MagicMock()
                session.__enter__.return_value = session
                session.stdout = b''
                session.raw = b''
                session.terminal.line.return_value = '❯'
                session.terminal.screen.return_value = []
                session.terminal.text.return_value = []

                def request(texts):
                    body = {'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text': text}]}
                                      for text in texts]}
                    return SimpleNamespace(response=response, json=lambda: body)

                def send(data):
                    if data == b'hold this request\r':
                        server.requests.append(request(['hold this request']))
                        trace.append({'type': 'model.requested', 'run_id': 'cancel', 'data': {'history': []}})
                    elif data == b'\x03':
                        trace.append({'type': 'run.finished', 'run_id': 'cancel', 'data': {'status': 'cancelled'}})
                        if defect == 'queued-submission':
                            server.requests.append(request(['stale cancelled input']))
                        if defect == 'stale-prompt':
                            session.terminal.line.return_value = '❯ stale cancelled input'
                        if defect == 'delivered-response':
                            response.segments.append(b'HTTP/1.1 200 OK\r\n')
                    elif data == b'fresh normal question\r':
                        texts = ['hold this request', 'fresh normal question']
                        if defect == 'request-leak':
                            texts.append('stale cancelled input')
                        server.requests.append(request(texts))
                        trace.append({'type': 'model.requested', 'run_id': 'fresh', 'data': {
                            'history': ['stale cancelled input'] if defect == 'trace-leak' else []}})
                        if defect != 'missing-completion':
                            trace.append({'type': 'run.finished', 'run_id': 'wrong' if defect == 'wrong-run' else 'fresh',
                                          'data': {'status': 'completed'}})
                        session.stdout = b'Next ordinary turn works.'
                        if defect == 'cancelled-reply':
                            session.stdout += b'Cancelled answer must not appear.'

                session.send.side_effect = send
                session.wait.side_effect = lambda predicate, _description: self.assertTrue(predicate(session.terminal))
                with patch.object(cli_pty_races, 'native', return_value=session), \
                     patch.object(cli_pty_races, 'events', return_value=trace):
                    if defect is None:
                        cli_pty_races._cancel_typeahead(context)
                        self.assertEqual(len(server.requests), 2)
                        session.snapshot.assert_any_call('typeahead-cancelled')
                        session.snapshot.assert_any_call('next-turn')
                    else:
                        with self.assertRaises(AssertionError):
                            cli_pty_races._cancel_typeahead(context)

    @unittest.skipUnless(os.environ.get("PARITY_RACES_BIN"), "set PARITY_RACES_BIN for offline executable probes")
    def test_executable_retry_and_foreground_interrupt(self):
        from parity import observe
        from parity.observe_cli_terminal import observations
        for case in collect():
            if case.pending:
                continue
            with self.subTest(case=case.id), tempfile.TemporaryDirectory(prefix="parity-races-") as tmp:
                root = Path(tmp)
                ctx = Context(os.environ["PARITY_RACES_BIN"], root / case.id, root / "artifacts", case)
                try:
                    case.run(ctx)
                    self.assertTrue(observe.finish(ctx))
                    self.assertTrue(observations(ctx))
                    if case.id == CASE:
                        events = [json.loads(line) for path in ctx.trace_paths for line in path.read_bytes().splitlines()]
                        races = Races(Normalizer(ctx.root, ctx.server.url), events, ctx.server.requests,
                                      ctx.invocations, case.id, ctx.observations)
                        self.assertIsNotNone(races.record)
                finally:
                    ctx.close()


if __name__ == "__main__":
    unittest.main()

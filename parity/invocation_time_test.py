"""Offline invocation-clock and replay compatibility boundaries."""

import unittest
from unittest.mock import patch

from parity.invocation_time import elapsed
from parity.observe import Normalizer
from parity.observe_derived import Derived
from parity.run import HarnessError, replay_case


class InvocationTimeTest(unittest.TestCase):
    def replay(self, invocations):
        raw = {}
        for index in range(1, len(invocations) + 1):
            prefix = 'invoke-%03d' % index
            raw.update({prefix + '.stdout': b'raw\xffstdout\n', prefix + '.stderr': b'raw stderr\n',
                        prefix + '.status': b'0\n'})
        metadata = dict(root='/allocated/W99', server='', traces=[], requests=0, invocations=invocations)
        original = dict(raw)

        def observe(ctx):
            self.assertEqual(ctx.observations, original)
            for expected, actual in zip(invocations, ctx.invocations):
                for key, value in expected.items():
                    self.assertEqual(actual[key], value)
            ctx.artifact_policies = {'exact': 'B'}
            (ctx.artifacts / 'substitutions.json').write_text('[]\n')
            return {'exact': b'raw\xffstdout\n'}

        with patch('parity.observe.finish', side_effect=observe):
            result = replay_case(raw, metadata)
        self.assertEqual(raw, original)
        self.assertEqual(result, ({'exact': b'raw\xffstdout\n'}, {'exact': 'B'}, []))

    def test_backwards_wall_anchors_do_not_order_or_time_monotonic_invocations(self):
        first = dict(started=1000., ended=900., monotonic_started=10., monotonic_ended=11.)
        second = dict(started=800., ended=700., monotonic_started=11., monotonic_ended=12.)
        self.assertEqual(elapsed(first), 1)
        self.replay([first, second])
        self.replay([dict(first, monotonic_ended=10.)])

    def test_malformed_monotonic_bounds_never_fall_back_to_wall(self):
        valid = dict(started=1000., ended=1001., monotonic_started=10., monotonic_ended=11.)
        invalid = [dict(valid, monotonic_started=12.)]
        for key in ('monotonic_started', 'monotonic_ended'):
            missing = dict(valid)
            del missing[key]
            invalid.append(missing)
            invalid.extend(dict(valid, **{key: value}) for value in (None, True, '10', float('nan'), float('inf'), -float('inf')))
        invalid.append(dict(valid, monotonic_started=-1e308, monotonic_ended=1e308))
        for bounds in invalid:
            with self.subTest(bounds=bounds):
                with self.assertRaises(ValueError):
                    elapsed(bounds)
                with self.assertRaisesRegex(HarnessError, 'invalid replay process bounds'):
                    self.replay([bounds])

    def test_monotonic_invocation_order_rejects_overlap_even_with_ordered_wall(self):
        first = dict(started=1000., ended=1001., monotonic_started=10., monotonic_ended=12.)
        second = dict(started=1002., ended=1003., monotonic_started=11., monotonic_ended=13.)
        with self.assertRaisesRegex(HarnessError, 'invalid replay invocation ordering'):
            self.replay([first, second])

    def test_legacy_replay_keeps_existing_wall_clock_contract(self):
        self.replay([dict(started=1000., ended=1001.)])
        self.assertEqual(elapsed(dict(started=1000., ended=1001.)), 1)
        for started, ended in ((2, 1), (None, 2), (1, float('nan')), (True, 2)):
            with self.subTest(started=started, ended=ended), self.assertRaisesRegex(HarnessError, 'invalid replay process bounds'):
                self.replay([dict(started=started, ended=ended)])

    def test_wall_anchors_remain_finite_even_with_monotonic_bounds(self):
        for key in ('started', 'ended'):
            bounds = dict(started=1000., ended=900., monotonic_started=10., monotonic_ended=11.)
            bounds[key] = float('nan')
            with self.subTest(key=key), self.assertRaisesRegex(HarnessError, 'invalid replay process bounds'):
                self.replay([bounds])

    def test_footer_elapsed_uses_monotonic_bound_not_wall_jump(self):
        normal = Normalizer('/allocated', '')
        derived = Derived(normal, [], [])
        line = '✓ complete · 1 step · 0 tool calls · model · 1.0s\n'
        bounds = dict(started=1000., ended=900., monotonic_started=10., monotonic_ended=11.)
        self.assertIn('0.0s', derived.display(line, 'stderr', bounds))
        with self.assertRaisesRegex(AssertionError, 'footer elapsed outside invocation bounds'):
            derived.display(line.replace('1.0s', '9.0s'), 'stderr', bounds)
        bounds.update(started=1., ended=1000.)
        with self.assertRaisesRegex(AssertionError, 'footer elapsed outside invocation bounds'):
            derived.display(line.replace('1.0s', '9.0s'), 'stderr', bounds)

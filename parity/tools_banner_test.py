"""Scripted chat never registers an identity from arbitrary transcript text."""

import unittest

from parity.observe import Normalizer


class ToolsBannerTests(unittest.TestCase):
    def test_scripted_banner_has_no_identity_slot(self):
        normal = Normalizer('/tmp/case', '', 0, 1)
        normal.register_display_sessions('re:agent chat · scripted\nworkspace /tmp/case/workspace · read\n'
                                         'session ID: 0123456789abcdef\n')
        self.assertEqual(normal.ids['session'], {})

    def test_scripted_banner_rejects_unallocated_workspace(self):
        normal = Normalizer('/tmp/case', '', 0, 1)
        with self.assertRaisesRegex(AssertionError, 'scripted chat workspace'):
            normal.register_display_sessions('re:agent chat · scripted\nworkspace /tmp/case-suffix/workspace\n')

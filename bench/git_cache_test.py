"""Keep offline git benchmark runs out of inherited Linux caches."""

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("git_bench", Path(__file__).parent / "git" / "run.py")
git_bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(git_bench)


class GitCacheTest(unittest.TestCase):
    def test_fixture_overrides_inherited_cache_and_preserves_fake_gh(self):
        with tempfile.TemporaryDirectory() as home:
            fixture = git_bench.Fixture.__new__(git_bench.Fixture)
            fixture.home = home
            with patch.dict(os.environ, HOME="/real-home", XDG_CACHE_HOME="/real-cache",
                            GH_TOKEN="secret", API_PROXY_URL="http://127.0.0.1:1"):
                env = fixture.env()
            self.assertEqual(env["HOME"], home)
            self.assertEqual(env["XDG_CACHE_HOME"], str(Path(home) / ".cache"))
            self.assertTrue(env["PATH"].startswith(str(Path(home) / "bin") + os.pathsep))
            self.assertNotIn("GH_TOKEN", env)
            self.assertEqual(env["API_PROXY_URL"], "http://127.0.0.1:1")


if __name__ == "__main__":
    unittest.main()

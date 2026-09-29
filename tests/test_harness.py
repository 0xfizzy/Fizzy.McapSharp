"""Infrastructure failures must remain failures, with no network required."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import test_suites as suites


class HarnessFailures(unittest.TestCase):
    def test_nonzero_child_is_not_success(self):
        with self.assertRaisesRegex(RuntimeError, 'sentinel'):
            suites.run(sys.executable, '-c', 'import sys; print("sentinel", file=sys.stderr); sys.exit(7)')

    def test_timeout_is_not_success(self):
        with self.assertRaises(subprocess.TimeoutExpired):
            suites.run(sys.executable, '-c', 'import time; time.sleep(5)', timeout=0.1)

    def test_corrupt_cached_archive_fails_before_extraction_or_network(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'tests').mkdir()
            (root / 'tests/conformance-lock.json').write_text(json.dumps({'archive_sha256': '0' * 64}))
            (root / 'artifacts/conformance').mkdir(parents=True)
            (root / 'artifacts/conformance/upstream.zip').write_bytes(b'corrupt')
            with patch.object(suites, 'ROOT', root), patch.object(suites.urllib.request, 'urlretrieve') as download:
                with self.assertRaisesRegex(RuntimeError, 'checksum mismatch'): suites.corpus()
                download.assert_not_called()

    def test_fixture_bounds_and_seed_replay(self):
        for seed in range(1, 33):
            spec = suites.generate(seed)
            self.assertEqual(spec, suites.generate(seed))
            messages = [r for r in spec['records'] if r['type'] == 'Message']
            self.assertTrue(1 <= len(messages) <= 256)
            self.assertLess(len(json.dumps(spec).encode()), 8 * 1024 * 1024)
        self.assertNotEqual(suites.generate(1), suites.generate(2))

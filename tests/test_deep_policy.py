"""Keep the accepted upstream Lz4 exception separate from binding failures."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
from test_deep import probe_mutation


class TimeoutPolicyTests(unittest.TestCase):
    def probe(self, effects, source='Lz4.mcap'):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder)
            path = directory / 'input.mcap'
            path.write_bytes(b'fixture')
            report = {'current': {'iteration': 637}, 'accepted_upstream_timeouts': []}
            with patch('test_deep.subprocess.run', side_effect=effects) as run:
                result = probe_mutation('binding', 'reference', path, source, None, report, directory)
            saved = [p.read_bytes() for p in directory.glob('upstream-lz4-timeout-*.mcap')]
            return result, report, saved, run.call_count

    @staticmethod
    def timeout():
        return subprocess.TimeoutExpired('probe', 30)

    def test_only_independently_reproduced_timeout_is_accepted_and_retained(self):
        result, report, saved, calls = self.probe([self.timeout(), self.timeout()])
        self.assertIsNone(result)
        self.assertEqual(len(report['accepted_upstream_timeouts']), 1)
        self.assertEqual(saved, [b'fixture'])
        self.assertEqual(calls, 2)

    def test_upstream_return_or_crash_does_not_waive_binding_timeout(self):
        for code in [0, 1, -11]:
            with self.subTest(code=code), self.assertRaises(subprocess.TimeoutExpired):
                self.probe([self.timeout(), subprocess.CompletedProcess('reference', code)])

    def test_other_compression_timeouts_remain_failures(self):
        for source in ['None.mcap', 'Zstd.mcap']:
            with self.subTest(source=source), self.assertRaises(subprocess.TimeoutExpired):
                self.probe([self.timeout()], source)

    def test_binding_crash_is_not_waived(self):
        result, report, saved, calls = self.probe([subprocess.CompletedProcess('binding', -11)])
        self.assertEqual(result.returncode, -11)
        self.assertFalse(report['accepted_upstream_timeouts'])
        self.assertFalse(saved)
        self.assertEqual(calls, 1)

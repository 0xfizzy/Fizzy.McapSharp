"""Attribution requires equivalent progress and independent diagnostic evidence."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
from mutation_policy import Outcome, attributed, execute, probe_mutation

CONFIG = 'MCAP_PROBE config v1 mcap=0.25.0 mode=2 input=7 crc=true end=true capacity=8388608\n'
TRACE = CONFIG + 'MCAP_PROBE next 2 4 0123456789abcdef\n'


def failure(category='timeout', code=None, stderr='', trace=TRACE):
    return Outcome(category, code, trace, stderr)


class AttributionTests(unittest.TestCase):
    def test_matching_timeouts(self):
        self.assertTrue(attributed(failure(), failure()))

    def test_matching_allocation_diagnostics(self):
        abort = failure('exit', -6, 'memory allocation of 3225936163 bytes failed')
        self.assertTrue(attributed(abort, abort))
        for code, size in [(-11, 3225936163), (-6, 1024)]:
            self.assertFalse(attributed(abort, failure('exit', code, f'memory allocation of {size} bytes failed')))

    def test_matching_caught_panic(self):
        binding = failure('exit', 86, "thread 'main' panicked at native/vendor/mcap/src/parse.rs:12:3:\ninvalid length\nMCAP_PROBE caught-panic")
        upstream = failure('exit', 101, "thread 'main' panicked at registry/mcap-0.25.0/src/parse.rs:10:3:\ninvalid length\n")
        self.assertTrue(attributed(binding, upstream))
        self.assertFalse(attributed(binding, failure('exit', 101, upstream.stderr.replace('invalid length', 'different cause'))))
        self.assertFalse(attributed(binding, failure('exit', 101, upstream.stderr.replace('parse.rs', 'other.rs'))))

    def test_signal_or_exit_code_alone_is_not_evidence(self):
        for code in [-6, -11, 1, 101]:
            with self.subTest(code=code):
                value = failure('exit', code)
                self.assertFalse(attributed(value, value))

    def test_missing_or_different_progress_is_rejected(self):
        for trace in ['', CONFIG, TRACE.replace('next 2 4', 'next 2 5'),
                      TRACE.replace('0123456789abcdef', '0123456789abcdee'),
                      TRACE.replace('crc=true', 'crc=false'), TRACE.replace('input=7', 'input=8'),
                      TRACE + 'MCAP_PROBE strict delivery\n', TRACE.replace('mode=2', 'mode=0'),
                      TRACE.replace('mode=2', 'mode=4').replace('next 2', 'next 4')]:
            with self.subTest(trace=trace):
                self.assertFalse(attributed(failure(), failure(trace=trace)))

    def test_different_outcomes_cannot_be_waived(self):
        for upstream in [Outcome('completed', 0), failure('exit', -11), Outcome('launch-error', None)]:
            self.assertFalse(attributed(failure(), upstream))

    def test_binding_assertion_is_always_strict(self):
        contract = failure(stderr='MCAP_PROBE binding-contract-failure')
        self.assertFalse(attributed(contract, failure()))

    def test_panic_followed_by_timeout_is_not_a_parser_timeout(self):
        value = failure(stderr="thread panicked at parse.rs:1:1:\nbroken")
        self.assertFalse(attributed(value, value))

    def probe(self, effects, source='Zstd.mcap', fails=False):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder)
            path = directory / 'input.mcap'
            path.write_bytes(b'fixture')
            report = {'current': {'iteration': 1}}
            limits = object()
            with patch('mutation_policy.execute', side_effect=effects) as run:
                if fails:
                    with self.assertRaises(RuntimeError):
                        probe_mutation('binding', 'reference', path, source, limits, report, directory)
                else:
                    probe_mutation('binding', 'reference', path, source, limits, report, directory)
            saved = [p.read_bytes() for p in directory.glob('*-*.mcap')]
            evidence = [json.loads(p.read_text()) for p in directory.glob('*.json')]
            for call in run.call_args_list:
                self.assertIs(call.args[1], limits)
                self.assertEqual(call.args[0][1], path)
            return report, saved, evidence, run.call_count

    def test_all_compressions_share_policy_and_retain_evidence(self):
        for source in ['None.mcap', 'Lz4.mcap', 'Zstd.mcap']:
            with self.subTest(source=source):
                report, saved, evidence, calls = self.probe([failure(), failure()], source)
                self.assertEqual(report['outcomes'], dict(completed=0, upstream=1, binding=0, unclassified=0))
                self.assertEqual(saved, [b'fixture'])
                self.assertEqual(evidence[0]['binding']['stdout'], TRACE)
                self.assertEqual(evidence[0]['upstream']['stdout'], TRACE)
                self.assertEqual(evidence[0]['limits']['address_space_bytes'], 1024**3)
                self.assertEqual(calls, 2)

    def test_completed_input_needs_no_reference(self):
        report, saved, evidence, calls = self.probe([Outcome('completed', 0)])
        self.assertEqual(report['outcomes']['completed'], 1)
        self.assertEqual((saved, evidence, calls), ([], [], 1))

    def test_unclassified_failure_keeps_both_logs(self):
        report, saved, evidence, calls = self.probe([failure(), Outcome('launch-error', None, stderr='missing reference')], fails=True)
        self.assertEqual(report['outcomes']['unclassified'], 1)
        self.assertEqual(evidence[0]['upstream']['stderr'], 'missing reference')
        self.assertEqual(saved, [b'fixture'])

    def test_successful_reference_exposes_binding_failure(self):
        report, _, _, _ = self.probe([failure(), Outcome('completed', 0)], fails=True)
        self.assertEqual(report['outcomes']['binding'], 1)

    def test_contract_failure_never_invokes_reference(self):
        report, _, _, calls = self.probe([failure(stderr='MCAP_PROBE binding-contract-failure')], fails=True)
        self.assertEqual(report['outcomes']['binding'], 1)
        self.assertEqual(calls, 1)

    def test_real_child_timeout_retains_partial_bytes(self):
        with patch('mutation_policy.TIMEOUT', 0.5):
            result = execute([sys.executable, '-c', "import time; print('progress', flush=True); time.sleep(10)"], None)
        self.assertEqual(result.category, 'timeout')
        self.assertIn('progress', result.stdout)

    def test_real_child_exit_and_missing_executable(self):
        result = execute([sys.executable, '-c', "import sys; print('diagnostic', file=sys.stderr); sys.exit(7)"], None)
        self.assertEqual((result.category, result.returncode), ('exit', 7))
        self.assertIn('diagnostic', result.stderr)
        with tempfile.TemporaryDirectory() as folder:
            self.assertEqual(execute([str(Path(folder) / 'missing-executable')], None).category, 'launch-error')


if __name__ == '__main__':
    unittest.main()

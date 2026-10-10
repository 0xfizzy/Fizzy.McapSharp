"""Run offline publication regression tests and retain a structured result."""
import sys
import unittest
from documentation import ROOT, write_json

if __name__ == '__main__':
    suite = unittest.defaultTestLoader.discover(str(ROOT / 'tests'), pattern='test_documentation_release.py')
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    write_json(ROOT / 'artifacts/reports/release-tests.json', {
        'status': 'passed' if result.wasSuccessful() else 'failed', 'tests': result.testsRun,
        'failures': [{'test': str(test), 'detail': detail} for test, detail in result.failures + result.errors],
        'scope': 'offline simulated services and isolated Git repositories; no remote publication'})
    sys.exit(0 if result.wasSuccessful() else 1)

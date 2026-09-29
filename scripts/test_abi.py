"""Test managed rejection of missing/mismatched native libraries in separate processes."""
import os
import json
import traceback
from pathlib import Path
from build import ROOT, TARGETS, host_rid
from test_suites import DLL, run


def check(dll=DLL):
    rid = host_rid()
    directory = ROOT / 'artifacts/abi-tests'
    directory.mkdir(parents=True, exist_ok=True)
    stub = directory / TARGETS[rid][1]
    local = ROOT / '.tools/cargo/bin/rustc.exe'
    rustc = str(local) if local.exists() else 'rustc'
    if local.exists():
        os.environ['CARGO_HOME'] = str(ROOT / '.tools/cargo')
        os.environ['RUSTUP_HOME'] = str(ROOT / '.tools/rustup')
    run(rustc, '--crate-type', 'cdylib', ROOT / 'tests/abi_mismatch.rs', '-o', stub, timeout=180)
    run('dotnet', dll, 'load-failure', 'missing')
    run('dotnet', dll, 'load-failure', 'abi', stub)
    run('dotnet', dll, 'exports', ROOT / 'artifacts/native' / rid / TARGETS[rid][1])


if __name__ == '__main__':
    report = {'rid': host_rid(), 'status': 'running'}
    try:
        check()
        report['status'] = 'passed'
    except BaseException:
        report['status'] = 'failed'; report['error'] = traceback.format_exc(); raise
    finally:
        directory = ROOT / 'artifacts/reports'
        directory.mkdir(parents=True, exist_ok=True)
        (directory / 'abi.json').write_text(json.dumps(report, indent=2), encoding='utf-8')

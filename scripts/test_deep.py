"""Linux x64 native FFI mutation, Valgrind and managed stress orchestration."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
import traceback
from build import ROOT, host_rid, output
from test_suites import DLL, generate, run


def probe_mutation(driver, reference, path, source, limits, report, directory):
    try:
        return subprocess.run([str(driver), str(path)], capture_output=True, text=True,
                              timeout=30, preexec_fn=limits)
    except subprocess.TimeoutExpired:
        if source != 'Lz4.mcap':
            raise
        # Only waive a timeout independently reproduced by the unmodified upstream
        # parser on the exact same bytes. A return, error, panic or crash is not a waiver.
        try:
            subprocess.run([str(reference), str(path)], capture_output=True, text=True,
                           timeout=30, preexec_fn=limits)
        except subprocess.TimeoutExpired:
            data = path.read_bytes()
            digest = hashlib.sha256(data).hexdigest()
            saved = directory / f'upstream-lz4-timeout-{digest}.mcap'
            saved.write_bytes(data)
            report['accepted_upstream_timeouts'].append(dict(
                **report['current'], sha256=digest, retained_input=str(saved),
                reason='Lz4 mutation also times out in unmodified registry mcap'))
            return None
        raise


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--seed', type=int, default=1)
    p.add_argument('--budget', type=int, default=1200)
    p.add_argument('--valgrind-budget', type=int, default=600)
    p.add_argument('--stress-budget', type=int, default=600)
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    if host_rid() != 'linux-x64': p.error('Deep native checks require Linux x64')
    if not all(1 <= n <= 1800 for n in [a.budget, a.valgrind_budget, a.stress_budget]): p.error('Budgets must be 1..1800 seconds')
    a.output = a.output.resolve(); a.output.mkdir(parents=True, exist_ok=False)
    report = dict(commit=output('git', 'rev-parse', 'HEAD'), rid=host_rid(), seed=a.seed,
                  rust=output('rustc', '--version'), dotnet=output('dotnet', '--version'),
                  config={k: str(v) for k, v in vars(a).items()}, status='running', mutations=0,
                  accepted_upstream_timeouts=[], valgrind=0)
    (a.output / 'report.json').write_text(json.dumps(report, indent=2))
    try:
        run('dotnet', 'build', ROOT / 'tests/ContractRunner', '-c', 'Release', timeout=180)
        native = ROOT / 'artifacts/native/linux-x64'
        driver = a.output / 'native-probe'
        run('rustc', '--edition=2021', '-C', 'opt-level=1', '-g', ROOT / 'tests/native_probe.rs', '-L', native,
            '-l', 'dylib=fizzy_mcap_native', '-o', driver, timeout=180)
        os.environ['LD_LIBRARY_PATH'] = str(native) + ':' + os.environ.get('LD_LIBRARY_PATH', '')
        reference_target = ROOT / 'native/target/deep-reference'
        run('cargo', 'build', '--release', '--locked', '--manifest-path',
            ROOT / 'tests/UpstreamReference/Cargo.toml', '--bin', 'timeout_probe',
            '--target-dir', reference_target, timeout=180)
        reference = reference_target / 'release/timeout_probe'
        for regression in sorted((ROOT / 'tests/corpus').glob('*.mcap')):
            run(driver, regression)
        inputs = []
        for compression in ['None', 'Lz4', 'Zstd']:
            # Even seeds enable chunks and indexes: all compression modes must actually run.
            spec = a.output / f'{compression}.json'; spec.write_text(json.dumps(generate(a.seed & ~1)))
            path = a.output / f'{compression}.mcap'; run('dotnet', DLL, 'write', spec, path, compression)
            inputs.append(path)
            run(driver, path)
        deadline = time.monotonic() + a.budget
        i = 0
        while time.monotonic() < deadline:
            source = inputs[i % 3]; data = bytearray(source.read_bytes())
            at = (a.seed * 2654435761 + i * 7919) % len(data)
            if (i // 3) % 3 == 0: data = data[:at]
            else: data[at] ^= 1 << (i % 8)
            current = a.output / 'mutation.mcap'; current.write_bytes(data)
            report['current'] = dict(iteration=i, offset=at, source=source.name, input=str(current))
            # RLIMIT_AS bounds hostile decompression/length requests without killing the runner.
            import resource
            def limits():
                resource.setrlimit(resource.RLIMIT_AS, (1024**3, 1024**3))
                resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
            def probe(path):
                return subprocess.run([str(driver), str(path)], capture_output=True, text=True, timeout=30, preexec_fn=limits)
            result = probe_mutation(driver, reference, current, source.name, limits, report, a.output)
            if result is None:
                i += 1
                continue
            if result.returncode:
                (a.output / 'native-failure.log').write_text(result.stdout + result.stderr)
                # Bounded delta reduction, preserving the original failing input and exit class.
                minimized = a.output / 'minimized.mcap'; best = bytes(data)
                step = max(1, len(best) // 2); attempts = 0
                while step and attempts < 64:
                    changed = False
                    for start in range(0, len(best), step):
                        trial = best[:start] + best[start + step:]
                        minimized.write_bytes(trial); attempts += 1
                        try: same = probe(minimized).returncode == result.returncode
                        except subprocess.TimeoutExpired: same = False
                        if same: best = trial; changed = True; break
                        if attempts >= 64: break
                    if not changed: step //= 2
                minimized.write_bytes(best)
                report['minimization'] = dict(attempts=attempts, original=len(data), reduced=len(best))
                report['reproduce'] = f'LD_LIBRARY_PATH={native} {driver} {current}'
                raise RuntimeError(result.stdout + result.stderr)
            i += 1; report['mutations'] = i
            if i % 100 == 0: (a.output / 'report.json').write_text(json.dumps(report, indent=2))
        deadline = time.monotonic() + a.valgrind_budget
        while time.monotonic() < deadline:
            path = inputs[report['valgrind'] % 3]
            run('valgrind', '--error-exitcode=99', '--leak-check=full', '--errors-for-leak-kinds=definite,indirect',
                '--show-leak-kinds=definite,indirect', f'--log-file={a.output / ("valgrind-" + str(report["valgrind"]) + ".log")}', driver, path, 10, timeout=120)
            report['valgrind'] += 1
        run('dotnet', DLL, 'lifecycle', a.output, a.stress_budget, timeout=a.stress_budget + 60)
        run('dotnet', DLL, 'large', a.output / 'large.mcap', timeout=1800)
        report['status'] = 'passed'
    except BaseException:
        report['status'] = 'failed'; report['error'] = traceback.format_exc(); raise
    finally:
        (a.output / 'report.json').write_text(json.dumps(report, indent=2))


if __name__ == '__main__': main()

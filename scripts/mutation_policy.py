"""Fail-closed attribution for bounded malformed-input subprocesses only."""
from dataclasses import asdict, dataclass
import hashlib
import json
from pathlib import Path
import re
import subprocess

TIMEOUT = 30
ADDRESS_SPACE = 1024**3


@dataclass
class Outcome:
    category: str
    returncode: int | None
    stdout: str = ''
    stderr: str = ''


def decoded(value):
    return value.decode('utf-8', errors='replace') if isinstance(value, bytes) else value or ''


def execute(command, limits):
    try:
        result = subprocess.run([str(v) for v in command], capture_output=True, text=True,
                                encoding='utf-8', errors='replace', timeout=TIMEOUT, preexec_fn=limits)
        return Outcome('completed' if result.returncode == 0 else 'exit', result.returncode,
                       result.stdout or '', result.stderr or '')
    except subprocess.TimeoutExpired as error:
        return Outcome('timeout', None, decoded(error.stdout), decoded(error.stderr))
    except OSError as error:
        return Outcome('launch-error', None, stderr=str(error))


def progress(outcome):
    lines = [line.removeprefix('MCAP_PROBE ') for line in outcome.stdout.splitlines()
             if line.startswith('MCAP_PROBE ')]
    configs = [line for line in lines if line.startswith('config ')]
    if not configs or not lines or not re.fullmatch(r'next [02] \d+ [0-9a-f]{16}', lines[-1]):
        return None
    mode = lines[-1].split()[1]
    if not re.fullmatch(rf'config v1 mcap=0\.25\.0 mode={mode} input=\d+ crc=true end=true capacity=8388608', configs[-1]):
        return None
    return configs[-1], lines[-1]


def diagnostic(outcome):
    if 'MCAP_PROBE binding-contract-failure' in outcome.stderr:
        return None
    if outcome.category == 'timeout':
        # A panic/abort followed by a stalled teardown is not a parser timeout.
        if 'panicked at' not in outcome.stderr and 'memory allocation of' not in outcome.stderr:
            return ('timeout',)
        return None
    if outcome.category != 'exit':
        return None
    allocation = re.search(r'memory allocation of (\d+) bytes failed', outcome.stderr)
    if allocation:
        return ('allocation', allocation[1], outcome.returncode)
    panic = re.search(r"panicked at ([^\r\n]+):\r?\n([^\r\n]+)", outcome.stderr)
    if panic:
        # Paths differ between registry and vendored builds; preserve source filename
        # and full panic message. No signal-only or exit-code-only waiver exists.
        location = panic[1].replace('\\', '/').rsplit('/', 1)[-1]
        filename = re.sub(r':\d+:\d+$', '', location)
        return ('panic', filename, panic[2])
    return None


def attributed(binding, upstream):
    stage = progress(binding)
    evidence = diagnostic(binding)
    if evidence and evidence[0] == 'panic':
        if binding.returncode != 86 or 'MCAP_PROBE caught-panic' not in binding.stderr or upstream.returncode != 101:
            return False
    return bool(stage and stage == progress(upstream) and evidence
                and evidence == diagnostic(upstream))


def validate_probes(driver, reference, path, limits=None):
    """Valid fixtures must succeed, and both probes must visit identical read stages."""
    binding = execute([driver, path], limits)
    if binding.category != 'completed':
        raise RuntimeError(f'Valid binding fixture failed: {path}\n{binding.stderr}')
    for mode in (0, 2):
        upstream = execute([reference, path, mode], limits)
        if upstream.category != 'completed':
            raise RuntimeError(f'Valid reference fixture failed: {path}\n{upstream.stderr}')
        prefixes = (f'MCAP_PROBE next {mode} ', f'MCAP_PROBE config v1 mcap=0.25.0 mode={mode} ')
        def stages(result):
            return [line for line in result.stdout.splitlines() if line.startswith(prefixes)]
        if not stages(upstream) or stages(binding) != stages(upstream):
            raise RuntimeError(f'Probe progress differs on valid fixture: {path}, mode {mode}')


def probe_mutation(driver, reference, path, source, limits, report, directory):
    counts = report.setdefault('outcomes', dict(completed=0, upstream=0, binding=0, unclassified=0))
    binding = execute([driver, path], limits)
    if binding.category == 'completed':
        counts['completed'] += 1
        return
    data = path.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    stage = progress(binding)
    upstream = None
    if stage and diagnostic(binding):
        upstream = execute([reference, path, stage[1].split()[1]], limits)
    if upstream and attributed(binding, upstream):
        classification = 'upstream'
    elif 'MCAP_PROBE binding-contract-failure' in binding.stderr or (upstream and upstream.category == 'completed'):
        classification = 'binding'
    else:
        classification = 'unclassified'
    counts[classification] += 1
    prefix = f'{classification}-{digest}'
    saved = directory / f'{prefix}.mcap'
    saved.write_bytes(data)
    evidence = dict(**report.get('current', {}), source_file=source, sha256=digest,
                    retained_input=str(saved), classification=classification,
                    limits=dict(timeout_seconds=TIMEOUT, address_space_bytes=ADDRESS_SPACE, core_bytes=0),
                    binding=asdict(binding), upstream=asdict(upstream) if upstream else None,
                    binding_command=[str(driver), str(path)],
                    reference_command=[str(reference), str(path), stage[1].split()[1]] if stage else None)
    # Every non-completed case retains both logs and all protocol evidence, even on
    # reference launch failures. The input is immutable until both processes exit.
    (directory / f'{prefix}.json').write_text(json.dumps(evidence, indent=2), encoding='utf-8')
    report.setdefault('failures', []).append(dict(classification=classification, sha256=digest,
                                                 evidence=f'{prefix}.json'))
    if classification != 'upstream':
        raise RuntimeError(f'{classification} mutation failure; see {prefix}.json')

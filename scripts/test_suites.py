"""Reproducible external suites. Build native Release before invoking this entry point."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import subprocess
import sys
import time
import traceback
import urllib.request
import zipfile
from concurrent.futures import ThreadPoolExecutor
from build import ROOT, host_rid, output

DLL = ROOT / 'tests/ContractRunner/bin/Release/net8.0/ContractRunner.dll'


def run(*command, timeout=30):
    result = subprocess.run(list(map(str, command)), cwd=ROOT, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=timeout)
    if result.returncode:
        raise RuntimeError(f'{list(map(str, command))}\n{result.stdout}\n{result.stderr}')
    return result.stdout


def corpus():
    lock = json.loads((ROOT / 'tests/conformance-lock.json').read_text())
    cache = ROOT / 'artifacts/conformance'
    cache.mkdir(parents=True, exist_ok=True)
    archive = cache / 'upstream.zip'
    if not archive.exists():
        urllib.request.urlretrieve(f'https://codeload.github.com/foxglove/mcap/zip/{lock["commit"]}', archive)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != lock['archive_sha256']:
        raise RuntimeError('Conformance archive checksum mismatch; remove stale cache')
    # Re-extract the verified archive so modified cached expectations cannot affect results.
    with zipfile.ZipFile(archive) as z:
        for member in z.infolist():
            if not (cache / member.filename).resolve().is_relative_to(cache.resolve()):
                raise RuntimeError('Unsafe archive member')
        z.extractall(cache)
    root = cache / ('mcap-' + lock['commit'])
    if not (root / lock['license_path']).is_file():
        raise RuntimeError('Missing upstream license')
    def materialize(path):
        pointer = path.read_text().splitlines()
        digest = pointer[1].removeprefix('oid sha256:')
        size = int(pointer[2].removeprefix('size '))
        blob = cache / ('lfs-' + digest)
        if not blob.exists():
            relative = path.relative_to(root).as_posix()
            urllib.request.urlretrieve(f'https://media.githubusercontent.com/media/foxglove/mcap/{lock["commit"]}/{relative}', blob)
        data = blob.read_bytes()
        if len(data) != size or hashlib.sha256(data).hexdigest() != digest:
            raise RuntimeError(f'LFS checksum mismatch: {path}')
        path.write_bytes(data)
    with ThreadPoolExecutor(max_workers=8) as pool:
        list(pool.map(materialize, sorted((root / 'tests/conformance/data').rglob('*.mcap'))))
    return root


def record(kind, **fields):
    def norm(v):
        if isinstance(v, int): return str(v)
        if isinstance(v, (bytes, list)): return [norm(x) for x in v]
        return v
    return dict(type=kind, fields=[[k, norm(v)] for k, v in sorted(fields.items())])


def generate(seed):
    # Explicit xorshift32 algorithm, independent of Python random library versions.
    state = seed & 0xffffffff or 0x9e3779b9
    def rand():
        nonlocal state
        state ^= (state << 13) & 0xffffffff
        state ^= state >> 17
        state ^= (state << 5) & 0xffffffff
        return state
    records = [record('Schema', id=1, name='模式', encoding='raw', data=b'\x00\xff'),
               record('Channel', id=1, schema_id=1, topic='/测试', message_encoding='raw', metadata={'key': '值'}),
               record('Channel', id=2, schema_id=0, topic='/empty-schema', message_encoding='raw', metadata={})]
    for i in range(1 + rand() % 256):
        records.append(record('Message', channel_id=1 + rand() % 2, sequence=i, log_time=rand() % 64,
                              publish_time=rand(), data=bytes(rand() % 256 for _ in range(rand() % 512))))
    records += [record('Schema', id=2, name='late', encoding='raw', data=b'late'),
                record('Channel', id=3, schema_id=2, topic='/unused-late', message_encoding='raw', metadata={})]
    records += [record('Metadata', name='meta', metadata={'seed': str(seed)}),
                record('Attachment', name='附件', media_type='application/octet-stream', log_time=0, create_time=0, data=b'\x00\xff')]
    features = ['ch', 'mx', 'chx', 'rsh', 'rch', 'st', 'ax', 'mdx', 'sum'] if seed % 2 == 0 else []
    return dict(records=records, meta=dict(variant=dict(features=features)))


def python_check(path, spec):
    from mcap.reader import make_reader
    expected = spec['records']
    with path.open('rb') as f:
        reader = make_reader(f, validate_crcs=True)
        actual = [record('Message', channel_id=m.channel_id, sequence=m.sequence, log_time=m.log_time, publish_time=m.publish_time, data=m.data)
                  for _, _, m in reader.iter_messages(log_time_order=False)]
        if actual != [r for r in expected if r['type'] == 'Message']: raise AssertionError('Python message mismatch')
    # Non-seekable readers need separate streams for each traversal.
    for method, kind in [('iter_metadata', 'Metadata'), ('iter_attachments', 'Attachment')]:
        with path.open('rb') as f:
            entries = list(getattr(make_reader(f, validate_crcs=True), method)())
            actual = [record(kind, **({'name': x.name, 'metadata': x.metadata} if kind == 'Metadata' else
                      dict(name=x.name, media_type=x.media_type, log_time=x.log_time, create_time=x.create_time, data=x.data))) for x in entries]
            if actual != [r for r in expected if r['type'] == kind]: raise AssertionError(kind + ' mismatch')


def python_write(path, spec, compression):
    from mcap.writer import Writer, CompressionType
    with path.open('wb') as f:
        w = Writer(f, compression=CompressionType[compression.upper()], chunk_size=1024,
                   use_chunking='ch' in spec['meta']['variant']['features'], enable_crcs=True, enable_data_crcs=True)
        w.start()
        for r in spec['records']:
            v = dict(r['fields']); kind = r['type']
            if kind == 'Schema': assert w.register_schema(v['name'], v['encoding'], bytes(map(int, v['data']))) == int(v['id'])
            elif kind == 'Channel': assert w.register_channel(v['topic'], v['message_encoding'], int(v['schema_id']), v['metadata']) == int(v['id'])
            elif kind == 'Message': w.add_message(int(v['channel_id']), int(v['log_time']), bytes(map(int, v['data'])), int(v['publish_time']), int(v['sequence']))
            elif kind == 'Metadata': w.add_metadata(v['name'], v['metadata'])
            elif kind == 'Attachment': w.add_attachment(int(v['create_time']), int(v['log_time']), v['name'], v['media_type'], bytes(map(int, v['data'])))
        w.finish()


def differential(args, report):
    for seed in range(args.seed, args.seed + args.samples):
        spec = generate(seed)
        case = args.output / f'seed-{seed}.json'
        case.write_text(json.dumps(spec, separators=(',', ':')), encoding='utf-8')
        report['current'] = dict(seed=seed, spec=str(case))
        for compression in ['None', 'Lz4', 'Zstd']:
            for producer in ['dotnet', 'python']:
                path = args.output / f'{seed}-{compression}-{producer}.mcap'
                if producer == 'dotnet': run('dotnet', DLL, 'write', case, path, compression)
                else: python_write(path, spec, compression)
                if path.stat().st_size > 8 * 1024 * 1024: raise AssertionError('Fixture exceeds budget')
                python_check(path, spec)
                run('dotnet', DLL, 'check', path, case)
                report['passed'] += 1


def robustness(args, report):
    for regression in sorted((ROOT / 'tests/corpus').glob('*.mcap')):
        run('dotnet', DLL, 'probe', regression)
        report['passed'] += 1
    spec = args.output / 'seed.json'; spec.write_text(json.dumps(generate(args.seed)), encoding='utf-8')
    source = args.output / 'source.mcap'
    run('dotnet', DLL, 'write', spec, source)
    data = source.read_bytes()
    deadline = time.monotonic() + args.budget
    for i in range(args.samples if not args.budget else 1000000000):
        # Replayable mutation location and byte. No arbitrary pointers enter FFI.
        mutated = bytearray(data)
        at = (args.seed * 2654435761 + i * 7919) % len(data)
        if i % 3 == 0: mutated = mutated[:at]
        else: mutated[at] ^= 1 << (i % 8)
        path = args.output / 'mutation.mcap'; path.write_bytes(mutated)
        report['current'] = dict(seed=args.seed, iteration=i, offset=at, input=str(path))
        run('dotnet', DLL, 'probe', path)
        report['passed'] += 1
        if args.budget and time.monotonic() >= deadline: break


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('suite', choices=['conformance', 'differential', 'robustness', 'stress'])
    p.add_argument('--seed', type=int, default=1)
    p.add_argument('--samples', type=int, default=32)
    p.add_argument('--budget', type=int, default=0, help='Seconds; robustness/stress only')
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--no-build', action='store_true')
    p.add_argument('--large', action='store_true')
    args = p.parse_args()
    if args.samples < 1 or args.budget < 0 or args.budget > 3600: p.error('Invalid sample count/budget')
    args.output = args.output.resolve(); args.output.mkdir(parents=True, exist_ok=True)
    if any(args.output.iterdir()): p.error('Use a fresh output directory to preserve failure evidence')
    report = dict(suite=args.suite, commit=output('git', 'rev-parse', 'HEAD'), rid=host_rid(),
                  python=platform.python_version(), dotnet=output('dotnet', '--version'),
                  rust_toolchain=(ROOT / 'rust-toolchain.toml').read_text(encoding='utf-8'),
                  config={k: str(v) for k, v in vars(args).items()}, passed=0,
                  reproduce=' '.join([sys.executable, *sys.argv]), status='running')
    started = time.monotonic()
    (args.output / 'report.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
    try:
        if not args.no_build: run('dotnet', 'build', ROOT / 'tests/ContractRunner', '-c', 'Release', timeout=180)
        if args.suite == 'conformance':
            root = corpus()
            report['upstream'] = json.loads((ROOT / 'tests/conformance-lock.json').read_text())
            report['summary'] = json.loads(run('node', ROOT / 'tests/conformance.mjs', root, DLL, args.output, timeout=1800))
            report['passed'] = sum(n for kind, n in report['summary'].items() if kind.endswith('/passed'))
        elif args.suite == 'differential': differential(args, report)
        elif args.suite == 'robustness': robustness(args, report)
        else:
            run('dotnet', DLL, 'lifecycle', args.output, max(args.budget, 1), timeout=max(args.budget, 1) + 60)
            report['passed'] += 1
            if args.large:
                run('dotnet', DLL, 'large', args.output / 'large.mcap', timeout=1800)
                report['passed'] += 1
        report['status'] = 'passed'
    except BaseException:
        report['status'] = 'failed'; report['error'] = traceback.format_exc(); raise
    finally:
        report['elapsed_seconds'] = time.monotonic() - started
        (args.output / 'report.json').write_text(json.dumps(report, indent=2), encoding='utf-8')


if __name__ == '__main__': main()

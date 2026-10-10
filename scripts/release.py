"""Release identity, immutable candidates, archive merging and bounded recovery.

Remote mutations are only invoked by explicit CLI commands or trusted workflows.
No command edits versions, creates tags, force pushes, or overwrites release assets.
"""
import argparse
import hashlib
import html
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET
import zipfile

from build import ROOT, TARGETS, output, run, source_identity, verify_assets, version
from documentation import digest, tree_hashes, write_json

REPO = '0xfizzy/Fizzy.McapSharp'
SITE = 'https://0xfizzy.github.io/Fizzy.McapSharp/'
BUNDLE = ROOT / 'artifacts/release'
SEMVER = re.compile(r'^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$')


def checked_version(value):
    if not SEMVER.fullmatch(value):
        raise ValueError('Expected canonical major.minor.patch[-prerelease], without build metadata')
    for part in (SEMVER.fullmatch(value)[4] or '').split('.'):
        if part.isdigit() and len(part) > 1 and part.startswith('0'):
            raise ValueError('Leading zero in prerelease version')
    return value


def tag_version(tag):
    if not tag.startswith('v'):
        raise ValueError('Release tag must start with v')
    return checked_version(tag[1:])


def gh(*args, binary=False):
    return subprocess.check_output(['gh', *map(str, args)], cwd=ROOT, text=not binary)


def api(route):
    return json.loads(gh('api', route))


def release_record(tag):
    # Listing authenticated releases includes drafts and distinguishes request failure from absence.
    pages = json.loads(gh('api', '--paginate', '--slurp', f'repos/{REPO}/releases?per_page=100'))
    matches = [r for page in pages for r in page if r['tag_name'] == tag]
    if len(matches) > 1:
        raise ValueError('Ambiguous release tag')
    return matches[0] if matches else None


def remote_commit(ref):
    return api(f'repos/{REPO}/commits/{ref}')['sha']


def local_check(tag, remote=True, require_ci=True):
    expected = tag_version(tag)
    if output('git', 'status', '--porcelain'):
        raise ValueError('Release requires a clean checkout, including untracked files')
    if version() != expected:
        raise ValueError('Tag and managed version differ')
    cargo = tomllib.loads((ROOT / 'native/Cargo.toml').read_text(encoding='utf-8'))
    lock = tomllib.loads((ROOT / 'native/Cargo.lock').read_text(encoding='utf-8'))
    native = [p['version'] for p in lock['package'] if p['name'] == cargo['package']['name']]
    if cargo['package']['version'] != expected or native != [expected]:
        raise ValueError('Managed, Cargo.toml and Cargo.lock versions differ')
    commit = output('git', 'rev-parse', 'HEAD')
    if output('git', 'rev-parse', f'refs/tags/{tag}^{{commit}}') != commit:
        raise ValueError('Checkout must match the release tag')
    if remote:
        if remote_commit(tag) != commit:
            raise ValueError('Remote tag differs from checkout')
        comparison = api(f'repos/{REPO}/compare/{commit}...main')
        if comparison['status'] not in ['ahead', 'identical']:
            raise ValueError('Release commit is not on main history')
        for workflow in (['build.yml', 'docs.yml'] if require_ci else []):
            runs = api(f'repos/{REPO}/actions/workflows/{workflow}/runs?head_sha={commit}&status=success&per_page=100')['workflow_runs']
            if not any(r['head_sha'] == commit and r['event'] in ['push', 'workflow_dispatch'] and r['head_branch'] == 'main' for r in runs):
                raise ValueError(f'No successful normal main {workflow} run for release commit')
    return {'version': expected, 'tag': tag, 'commit': commit}


def package_content(path):
    with zipfile.ZipFile(path) as archive:
        names = [n for n in archive.namelist() if not n.endswith('/')]
        if len(names) != len(set(names)):
            raise ValueError('Duplicate package entries')
        nuspec = ET.fromstring(archive.read('Fizzy.McapSharp.nuspec'))
        identity = (nuspec.findtext('{*}metadata/{*}id'), nuspec.findtext('{*}metadata/{*}version'))
        content = {n: digest(archive.read(n)) for n in names if n != '.signature.p7s'}
        return identity, content


def nuget_bytes(value):
    checked_version(value)
    url = f'https://api.nuget.org/v3-flatcontainer/fizzy.mcapsharp/{value.lower()}/fizzy.mcapsharp.{value.lower()}.nupkg'
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            return response.read()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise


def compare_remote(package, data):
    BUNDLE.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='nuget-', dir=BUNDLE) as folder:
        remote = Path(folder) / package.name
        remote.write_bytes(data)
        if package_content(package) != package_content(remote):
            raise ValueError('Published package content conflicts with immutable candidate')


def wait_nuget(package, attempts=20, delay=15, fetch=nuget_bytes, sleep=time.sleep):
    value = package_content(package)[0][1]
    last = 'not available'
    for attempt in range(attempts):
        try:
            data = fetch(value)
            if data is not None:
                compare_remote(package, data)
                return {'nuget': 'verified', 'version': value}
        except (urllib.error.URLError, TimeoutError) as error:
            last = str(error)
        if attempt + 1 < attempts:
            sleep(delay)
    raise RuntimeError(f'NuGet availability deadline exceeded: {last}; resume the same tag')


def unpack_site(archive_path, destination):
    with zipfile.ZipFile(archive_path) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)):
            raise ValueError('Duplicate archive entries')
        for info in archive.infolist():
            name = PurePosixPath(info.filename)
            if name.is_absolute() or '..' in name.parts or '\\' in info.filename or ':' in info.filename or (info.external_attr >> 16) & 0o170000 == 0o120000:
                raise ValueError('Unsafe documentation archive path')
        archive.extractall(destination)


def verify_bundle(folder, tag=None):
    manifest = json.loads((folder / 'provenance.json').read_text(encoding='utf-8'))
    if manifest.get('schema') != 1:
        raise ValueError('Unsupported provenance schema')
    value = checked_version(manifest['version'])
    required = {f'Fizzy.McapSharp.{value}.nupkg', 'docs.zip', 'validation.zip', 'validation-jobs.json'}
    if not required.issubset(manifest['assets']):
        raise ValueError('Candidate manifest omits required assets')
    if manifest['tag'] != 'v' + value or (tag and manifest['tag'] != tag):
        raise ValueError('Bundle tag mismatch')
    for name, expected in manifest['assets'].items():
        if Path(name).name != name or digest((folder / name).read_bytes()) != expected:
            raise ValueError(f'Candidate asset mismatch: {name}')
    package = folder / f'Fizzy.McapSharp.{value}.nupkg'
    if package_content(package)[0] != ('Fizzy.McapSharp', value):
        raise ValueError('Candidate package identity mismatch')
    if set(manifest['native']) != set(TARGETS):
        raise ValueError('Missing native provenance')
    evidence = json.loads((folder / 'validation-jobs.json').read_text(encoding='utf-8'))
    if evidence['commit'] != manifest['commit'] or str(evidence['run_id']) != str(manifest['run_id']) or not any(
            j['name'].endswith('verified') and j['conclusion'] == 'success' for j in evidence['jobs']):
        raise ValueError('Persisted validation evidence does not prove the original gate')
    with zipfile.ZipFile(package) as archive:
        archive.read('lib/net8.0/Fizzy.McapSharp.xml')
        for rid, (_, filename) in TARGETS.items():
            native = manifest['native'][rid]
            if native['commit'] != manifest['commit'] or native['source_sha256'] != manifest['source_sha256'] or digest(archive.read(f'runtimes/{rid}/native/{filename}')) != native['sha256']:
                raise ValueError(f'Native provenance mismatch: {rid}')
    with tempfile.TemporaryDirectory(prefix='verify-', dir=folder) as temp:
        site = Path(temp)
        unpack_site(folder / 'docs.zip', site)
        if tree_hashes(site) != manifest['files']:
            raise ValueError('Site file manifest mismatch')
        info = json.loads((site / 'doc-info.json').read_text(encoding='utf-8'))
        if (info['version'], info['release_commit'], info['docs_commit'], info['revision']) != (value, manifest['commit'], manifest['commit'], 0):
            raise ValueError('Original documentation provenance mismatch')
    return manifest


def prepare(tag):
    identity = local_check(tag)
    verify_assets()
    packages = list((ROOT / 'artifacts/packages').glob('*.nupkg'))
    if len(packages) != 1:
        raise ValueError('Expected exactly one verified package')
    if BUNDLE.exists() and any(BUNDLE.iterdir()):
        raise ValueError('Candidate output must be empty; restore an existing release instead')
    BUNDLE.mkdir(parents=True, exist_ok=True)
    shutil.copy2(packages[0], BUNDLE / packages[0].name)
    shutil.copy2(ROOT / 'artifacts/docs/docs.zip', BUNDLE / 'docs.zip')
    reports = ROOT / 'artifacts/validation-reports'
    if not reports.exists() or not list(reports.rglob('*.json')):
        raise ValueError('Missing validation reports from verified workflow')
    # Include the documentation/sample gates alongside the reusable native/package reports.
    for name in ['documentation-build.json', 'documentation-samples.json', 'release-tests.json']:
        report = ROOT / 'artifacts/reports' / name
        if not report.exists() or json.loads(report.read_text(encoding='utf-8'))['status'] != 'passed':
            raise ValueError(f'Missing successful gate report: {name}')
        shutil.copy2(report, reports / name)
    shutil.make_archive(str(BUNDLE / 'validation'), 'zip', reports)
    run_id = os.environ['GITHUB_RUN_ID']
    jobs = json.loads(gh('api', '--paginate', '--slurp', f'repos/{REPO}/actions/runs/{run_id}/jobs?filter=latest&per_page=100'))
    write_json(BUNDLE / 'validation-jobs.json', {'commit': identity['commit'], 'run_id': run_id,
        'jobs': [{'id': j['id'], 'name': j['name'], 'conclusion': j['conclusion']} for page in jobs for j in page['jobs'] if j['conclusion'] == 'success']})
    manifest = dict(schema=1, **identity, source_sha256=source_identity()['source_sha256'], run_id=os.environ['GITHUB_RUN_ID'],
                    native={rid: json.loads((ROOT / 'artifacts/native' / rid / 'manifest.json').read_text(encoding='utf-8')) for rid in TARGETS},
                    files=json.loads((ROOT / 'artifacts/docs/files.json').read_text(encoding='utf-8')),
                    tools=json.loads((ROOT / 'artifacts/docs/site/doc-info.json').read_text(encoding='utf-8'))['tools'],
                    gates={'native': 'passed', 'package_matrix': 'passed', 'deep': 'passed', 'documentation': 'passed', 'samples': 'passed'})
    manifest['assets'] = {p.name: digest(p.read_bytes()) for p in BUNDLE.iterdir() if p.is_file()}
    write_json(BUNDLE / 'provenance.json', manifest)
    verify_bundle(BUNDLE, tag)
    create_candidate_bundle(BUNDLE)
    return manifest


def create_candidate_bundle(folder):
    """One self-contained recovery asset; legacy loose assets remain readable."""
    manifest = verify_bundle(folder)
    names = sorted([*manifest['assets'], 'provenance.json'])
    target = folder / 'release-bundle.zip'
    with zipfile.ZipFile(target, 'w', zipfile.ZIP_DEFLATED) as archive:
        for name in names:
            entry = zipfile.ZipInfo(name, (2000, 1, 1, 0, 0, 0))
            entry.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(entry, (folder / name).read_bytes())
    return target


def persist(tag, folder):
    record = release_record(tag)
    if record is None:
        gh('release', 'create', tag, '--repo', REPO, '--verify-tag', '--draft', '--title', tag, '--notes', 'Release verification is in progress. See the attached provenance and validation reports.')
        record = release_record(tag)
    assets = {a['name']: a for a in record['assets']}
    if (folder / 'provenance.json').exists():
        # Journal immutable identity before uploads, so a partially uploaded candidate can resume.
        marker = folder.parent / 'candidate.json'
        marker.write_bytes((folder / 'provenance.json').read_bytes())
        if 'candidate.json' in assets:
            data = gh('api', '-H', 'Accept: application/octet-stream', f'repos/{REPO}/releases/assets/{assets["candidate.json"]["id"]}', binary=True)
            if data != marker.read_bytes():
                raise ValueError('Candidate identity is already bound to different bytes')
        else:
            gh('release', 'upload', tag, marker, '--repo', REPO)
    # Provenance is uploaded last: its presence is the durable candidate completion marker.
    candidate_names = None
    if (folder / 'provenance.json').exists():
        candidate_names = {*verify_bundle(folder, tag)['assets'], 'provenance.json'}
        if (folder / 'release-bundle.zip').exists():
            candidate_names.add('release-bundle.zip')
    files = sorted((p for p in folder.glob('*') if candidate_names is None or p.name in candidate_names), key=lambda p: (0 if p.name == 'release-bundle.zip' or re.fullmatch(r'revision-r\d+\.zip', p.name) else 2 if p.name == 'provenance.json' else 1, p.name))
    for path in files:
        if not path.is_file():
            continue
        if path.name in assets:
            data = gh('api', '-H', 'Accept: application/octet-stream', f'repos/{REPO}/releases/assets/{assets[path.name]["id"]}', binary=True)
            if digest(data) != digest(path.read_bytes()):
                raise ValueError(f'Existing immutable release asset conflicts: {path.name}')
        else:
            gh('release', 'upload', tag, path, '--repo', REPO)


def restore(tag, complete_upload=False):
    record = release_record(tag)
    if not record or not any(a['name'] in ['release-bundle.zip', 'provenance.json', 'candidate.json'] for a in record['assets']):
        raise ValueError('No completed durable candidate; rerun the original candidate job to finish its uploads')
    BUNDLE.mkdir(parents=True, exist_ok=True)
    asset_map = {a['name']: a for a in record['assets']}
    def download(name):
        item = asset_map[name]
        return gh('api', '-H', 'Accept: application/octet-stream', f'repos/{REPO}/releases/assets/{item["id"]}', binary=True)
    if 'release-bundle.zip' in asset_map:
        with tempfile.TemporaryDirectory(prefix='bundle-', dir=BUNDLE.parent) as temporary:
            directory = Path(temporary)
            archive = directory / 'release-bundle.zip'
            archive.write_bytes(download('release-bundle.zip'))
            unpacked = directory / 'verified'
            unpack_site(archive, unpacked)
            manifest = verify_bundle(unpacked, tag)
            expected = {*manifest['assets'], 'provenance.json'}
            if set(tree_hashes(unpacked)) != expected:
                raise ValueError('Self-contained bundle contains unexpected files')
            if remote_commit(tag) != manifest['commit']:
                raise ValueError('Release tag moved since candidate creation')
            for name in expected:
                shutil.copy2(unpacked / name, BUNDLE / name)
            shutil.copy2(archive, BUNDLE / archive.name)
        if complete_upload:
            persist(tag, BUNDLE)
        return manifest
    raw = download('provenance.json' if 'provenance.json' in asset_map else 'candidate.json')
    manifest = json.loads(raw)
    missing = set(manifest['assets']) - set(asset_map)
    transient = None
    if missing:
        transient = Path(tempfile.mkdtemp(prefix='resume-input-', dir=BUNDLE.parent))
        gh('run', 'download', str(manifest['run_id']), '--repo', REPO, '--name', 'release-bundle', '--dir', transient)
    for name in manifest['assets']:
        if Path(name).name != name:
            raise ValueError('Invalid asset filename')
        (BUNDLE / name).write_bytes((transient / name).read_bytes() if name in missing else download(name))
    (BUNDLE / 'provenance.json').write_bytes(raw)
    manifest = verify_bundle(BUNDLE, tag)
    if remote_commit(tag) != manifest['commit']:
        raise ValueError('Release tag moved since candidate creation')
    # verify_bundle checks durable gate evidence. Expired Actions history must not prevent
    # restoring a completely archived release years later.
    if complete_upload:
        persist(tag, BUNDLE)
    return manifest


def redirect(path, relative):
    path.parent.mkdir(parents=True, exist_ok=True)
    target = html.escape(relative, quote=True)
    path.write_text(f'<!doctype html><meta charset="utf-8"><meta http-equiv="refresh" content="0;url={target}"><a href="{target}">Documentation</a>\n', encoding='utf-8')


def merge_site(root, site, value, revision=None, activate=False):
    """Merge into the latest tree; never alter any existing immutable revision."""
    checked_version(value)
    root.mkdir(parents=True, exist_ok=True)
    info = json.loads((site / 'doc-info.json').read_text(encoding='utf-8'))
    if info['version'] != value or info['revision'] != revision:
        raise ValueError('Site identity does not match destination')
    before = {p.relative_to(root).as_posix(): tree_hashes(p) for p in root.glob('v*/r*') if p.is_dir()}
    if revision is None:
        target = root / 'dev'
        if target.exists():
            shutil.rmtree(target)
        shutil.copytree(site, target)
    else:
        if revision < 0:
            raise ValueError('Negative revision')
        target = root / f'v{value}/r{revision}'
        if target.exists():
            if tree_hashes(target) != tree_hashes(site):
                raise ValueError('Immutable revision already exists with different content')
        else:
            if revision and not (root / f'v{value}/r{revision - 1}').is_dir():
                raise ValueError('Revision sequence has a gap')
            shutil.copytree(site, target)
        current_file = root / f'v{value}/current.json'
        current = json.loads(current_file.read_text(encoding='utf-8'))['revision'] if current_file.exists() else -1
        # Replaying old releases must not roll a revision pointer backwards.
        if activate and revision >= current:
            activate_site(root, value, revision)
    refresh_index(root)
    for name, expected in before.items():
        if tree_hashes(root / name) != expected:
            raise ValueError(f'Historical archive changed: {name}')


def activate_site(root, value, revision, rollback=False, expected_commit=None):
    """Activate only after the caller has verified the immutable URL in the same deploy lock."""
    target = root / f'v{checked_version(value)}/r{revision}'
    info = json.loads((target / 'doc-info.json').read_text(encoding='utf-8'))
    if info['version'] != value or info['revision'] != revision or (expected_commit and info['docs_commit'] != expected_commit):
        raise ValueError('Activation identity differs from verified immutable site')
    current_file = root / f'v{value}/current.json'
    current = json.loads(current_file.read_text(encoding='utf-8'))['revision'] if current_file.exists() else -1
    marker = root / f'v{value}/completed/r{revision}.json'
    already_completed = marker.exists()
    if rollback and not already_completed:
        raise ValueError('Rollback requires a previously completed revision')
    write_json(marker, {'revision': revision, 'docs_commit': info['docs_commit']})
    # Replaying a previously completed revision must not undo an explicit rollback.
    if rollback or (revision >= current and (not already_completed or current == revision or current < 0)):
        write_json(current_file, {'revision': revision, 'completed': True})
        redirect(root / f'v{value}/index.html', f'r{revision}/index.html')
    refresh_index(root)


def assert_current_main(site):
    info = json.loads((site / 'doc-info.json').read_text(encoding='utf-8'))
    if info['revision'] is not None or info['docs_commit'] != remote_commit('main'):
        raise ValueError('Stale main documentation build refused; run docs for current main')


def refresh_index(root):
    entries = []
    for current in root.glob('v*/current.json'):
        value = checked_version(current.parent.name[1:])
        pointer = json.loads(current.read_text(encoding='utf-8'))
        if pointer.get('completed') is not True or not (current.parent / f'completed/r{pointer["revision"]}.json').is_file():
            continue
        entries.append({'version': value, 'current': pointer['revision'], 'completed': True})
    entries.sort(key=lambda x: tuple(int(i) for i in SEMVER.fullmatch(x['version']).groups()[:3]), reverse=True)
    stable = [e for e in entries if '-' not in e['version']]
    latest = stable[0]['version'] if stable else None
    write_json(root / 'versions.json', {'schema': 1, 'latest': latest, 'dev': (root / 'dev/index.html').exists(), 'entries': entries})
    if latest:
        redirect(root / 'latest/index.html', f'../v{latest}/index.html')
        redirect(root / 'index.html', 'latest/index.html')
    elif (root / 'dev/index.html').exists():
        redirect(root / 'index.html', 'dev/index.html')
    else:
        (root / 'index.html').write_text('<!doctype html><p>No stable release is available.</p>', encoding='utf-8')
    (root / '.nojekyll').touch()


def deploy_tree(site, value, revision, rollback=None, remote_url=None, credential_header=None, recovery=False, activate=False, expected_commit=None, promote=False):
    """Called only inside the shared Pages concurrency job; use optimistic Git pushes."""
    if revision is None and remote_url is None:
        assert_current_main(site)
    parent = ROOT / 'artifacts/pages'
    parent.mkdir(parents=True, exist_ok=True)
    for attempt in range(3):
        checkout = Path(tempfile.mkdtemp(prefix='tree-', dir=parent))
        def git(*args):
            return subprocess.check_output(['git', '-C', str(checkout), *map(str, args)], text=True).strip()
        git('init')
        git('config', 'core.autocrlf', 'false')
        git('remote', 'add', 'origin', remote_url or f'https://github.com/{REPO}.git')
        # Actions checkout installs a scoped HTTP credential header. Reuse without logging it.
        if remote_url is None:
            header = credential_header or output('git', 'config', '--get', 'http.https://github.com/.extraheader')
            git('config', 'http.https://github.com/.extraheader', header)
        refs = git('ls-remote', '--heads', 'origin', 'gh-pages')
        if refs:
            git('fetch', '--depth=1', 'origin', 'gh-pages')
            git('checkout', '-b', 'gh-pages', 'FETCH_HEAD')
        else:
            git('checkout', '--orphan', 'gh-pages')
        if rollback is not None:
            target = checkout / f'v{value}/r{rollback}'
            if not target.is_dir():
                raise ValueError('Requested archived revision does not exist')
            if activate:
                activate_site(checkout, value, rollback, rollback=True, expected_commit=expected_commit)
        elif recovery:
            # Recovery adds every immutable revision, but preserves existing active pointers.
            existing = checkout / f'v{value}/current.json'
            active = json.loads(existing.read_text(encoding='utf-8'))['revision'] if existing.exists() else revision
            for restored in sorted(site.glob(f'v{value}/r*'), key=lambda p: int(p.name[1:])):
                merge_site(checkout, restored, value, int(restored.name[1:]), activate=False)
            if not (checkout / f'v{value}/r{active}').is_dir():
                raise ValueError('Recovery target revision is missing')
            if activate and (promote or not existing.exists()):
                activate_site(checkout, value, revision if promote else active, expected_commit=expected_commit)
        else:
            merge_site(checkout, site, value, revision, activate=False)
            if activate and revision is not None:
                activate_site(checkout, value, revision, expected_commit=expected_commit)
        git('config', 'user.name', 'github-actions[bot]')
        git('config', 'user.email', '41898282+github-actions[bot]@users.noreply.github.com')
        git('add', '--all')
        if git('status', '--porcelain'):
            git('commit', '-m', f'docs: archive {value} revision {revision}')
            result = subprocess.run(['git', '-C', str(checkout), 'push', 'origin', 'HEAD:refs/heads/gh-pages'], capture_output=True, text=True)
            if result.returncode:
                if attempt < 2:
                    continue  # reread latest remote tree and reapply; never force
                raise RuntimeError('Archive push failed after three fresh merges; inspect branch permissions or concurrency')
        # Never upload repository metadata or credentials in a Pages artifact.
        export = Path(tempfile.mkdtemp(prefix='export-', dir=parent))
        for item in checkout.iterdir():
            if item.name == '.git':
                continue
            if item.is_dir():
                shutil.copytree(item, export / item.name)
            else:
                shutil.copy2(item, export / item.name)
        page = export / ('dev' if revision is None else f'v{value}/r{revision}') / 'doc-info.json'
        result = {'export': str(export), 'docs_commit': json.loads(page.read_text(encoding='utf-8'))['docs_commit']}
        if activate and revision is not None:
            result['active_revision'] = json.loads((export / f'v{value}/current.json').read_text(encoding='utf-8'))['revision']
            result['latest'] = json.loads((export / 'versions.json').read_text(encoding='utf-8'))['latest'] or ''
        return result
    raise RuntimeError('Archive merge failed')


def recover_site(tag, revision):
    manifest = restore(tag)
    value = manifest['version']
    record = release_record(tag)
    assets = {a['name']: a for a in record['assets']}
    recovered = ROOT / 'artifacts/recovered-archive'
    recovered.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='recovery-', dir=BUNDLE) as temporary:
        original = Path(temporary) / 'r0'
        unpack_site(BUNDLE / 'docs.zip', original)
        merge_site(recovered, original, value, 0)
        revisions = sorted({int(m[1] or m[2]) for name in assets if (m := re.fullmatch(r'(?:provenance-r([1-9]\d*)\.json|revision-r([1-9]\d*)\.zip)', name))})
        for number in revisions:
            if f'revision-r{number}.zip' in assets:
                item = assets[f'revision-r{number}.zip']
                bundle_data = gh('api', '-H', 'Accept: application/octet-stream', f'repos/{REPO}/releases/assets/{item["id"]}', binary=True)
                import io
                with zipfile.ZipFile(io.BytesIO(bundle_data)) as bundle:
                    expected = {f'provenance-r{number}.json', f'docs-r{number}.zip'}
                    if len(bundle.namelist()) != 2 or set(bundle.namelist()) != expected:
                        raise ValueError('Revision bundle contains unexpected files')
                    metadata = json.loads(bundle.read(f'provenance-r{number}.json'))
                    data = bundle.read(f'docs-r{number}.zip')
            else:
                item = assets[f'provenance-r{number}.json']
                metadata = json.loads(gh('api', '-H', 'Accept: application/octet-stream', f'repos/{REPO}/releases/assets/{item["id"]}', binary=True))
                item = assets[f'docs-r{number}.zip']
                data = gh('api', '-H', 'Accept: application/octet-stream', f'repos/{REPO}/releases/assets/{item["id"]}', binary=True)
            if metadata['version'] != value or metadata['revision'] != number or metadata['release_commit'] != manifest['commit'] or metadata['archive_sha256'] != digest(data) or metadata['package_sha256'] != digest((BUNDLE / f'Fizzy.McapSharp.{value}.nupkg').read_bytes()):
                raise ValueError('Revision recovery provenance mismatch')
            archive = Path(temporary) / f'r{number}.zip'
            archive.write_bytes(data)
            site = Path(temporary) / f'r{number}'
            unpack_site(archive, site)
            if tree_hashes(site) != metadata['files']:
                raise ValueError('Revision file hash mismatch')
            info = json.loads((site / 'doc-info.json').read_text(encoding='utf-8'))
            if (info['version'], info['revision'], info['release_commit'], info['docs_commit']) != (value, number, manifest['commit'], metadata['docs_commit']):
                raise ValueError('Revision site identity mismatch')
            merge_site(recovered, site, value, number)
    if revision is None or not (recovered / f'v{value}/r{revision}').exists():
        raise ValueError('Select an existing revision for recovery')
    return {'version': value, 'revision': revision, 'recovered': str(recovered)}


def verify_online(value, revision, attempts=12, delay=10, expected_commit=None):
    prefix = f'v{value}/r{revision}/' if revision is not None else 'dev/'
    last = ''
    for attempt in range(attempts):
        try:
            with urllib.request.urlopen(SITE + prefix + 'doc-info.json', timeout=30) as response:
                info = json.load(response)
            if info['version'] != value or info['revision'] != revision:
                raise ValueError('Online documentation identity mismatch')
            if expected_commit and info['docs_commit'] != expected_commit:
                raise ValueError('Online documentation source commit mismatch')
            for path in ['index.html', 'docs/index.html', 'docs/zh-CN/index.html', 'api/Fizzy.McapSharp.McapWriter.html', 'index.json', 'version.js']:
                with urllib.request.urlopen(SITE + prefix + path, timeout=30) as response:
                    if not response.read():
                        raise ValueError(f'Empty online resource: {path}')
            return {'pages': 'verified', 'url': SITE + prefix}
        except (urllib.error.URLError, TimeoutError, ValueError) as error:
            last = str(error)
        if attempt + 1 < attempts:
            time.sleep(delay)
    raise RuntimeError(f'Online verification failed: {last}; resume deployment')


def verify_entrypoints(value, attempts=12, delay=10, expected_revision=None, expected_latest=None):
    last = ''
    for attempt in range(attempts):
        try:
            with urllib.request.urlopen(SITE + 'versions.json', timeout=30) as response:
                index = json.load(response)
            entry = next(e for e in index['entries'] if e['version'] == value and e.get('completed'))
            if expected_revision is not None and entry['current'] != expected_revision:
                raise ValueError('Online active revision is stale')
            if expected_latest is not None and (index['latest'] or '') != expected_latest:
                raise ValueError('Online latest version is stale')
            with urllib.request.urlopen(SITE + f'v{value}/index.html', timeout=30) as response:
                if f'r{entry["current"]}/index.html' not in response.read().decode():
                    raise ValueError('Version entrypoint is not active')
            stable = [e['version'] for e in index['entries'] if '-' not in e['version'] and e.get('completed')]
            latest = max(stable, key=lambda v: tuple(map(int, v.split('.')))) if stable else None
            if index['latest'] != latest:
                raise ValueError('Latest does not select highest completed stable version')
            if latest:
                with urllib.request.urlopen(SITE + 'latest/index.html', timeout=30) as response:
                    if f'../v{latest}/index.html' not in response.read().decode():
                        raise ValueError('Latest entrypoint differs from completed index')
            return {'entrypoints': 'verified', 'active_revision': entry['current']}
        except (urllib.error.URLError, TimeoutError, ValueError, StopIteration) as error:
            last = str(error)
        if attempt + 1 < attempts:
            time.sleep(delay)
    raise RuntimeError(f'Entrypoint verification failed: {last}; resume deployment')


def without_xml_comments(text):
    # Match string/comment tokens before documentation lines, so a /// in a raw or
    # verbatim multiline literal cannot disguise an executable payload change.
    tokens = re.compile(r'(?P<raw>"{3,}).*?(?P=raw)|@"(?:[^"]|"")*"|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'|/\*.*?\*/|//[^\n]*', re.S)
    result, cursor = [], 0
    for match in tokens.finditer(text):
        line_start = text.rfind('\n', 0, match.start()) + 1
        if match.group().startswith('///') and not text[line_start:match.start()].strip():
            result.append(text[cursor:line_start])
            cursor = match.end() + (1 if text[match.end():match.end()+1] == '\n' else 0)
    result.append(text[cursor:])
    return ''.join(result)



def check_revision_changes(base):
    changed = output('git', 'diff', '--name-only', base, 'HEAD').splitlines()
    allowed = ('docs/', 'samples/Documentation/', 'scripts/docsite/')
    for name in changed:
        if name.startswith('src/') and name.endswith('.cs'):
            before = subprocess.check_output(['git', 'show', f'{base}:{name}'], cwd=ROOT, text=True)
            after = (ROOT / name).read_text(encoding='utf-8')
            strip = without_xml_comments
            if strip(before) != strip(after):
                raise ValueError(f'Only XML-comment changes are allowed in revision source: {name}')
        elif not name.startswith(allowed) and name not in ['README.md', 'README.zh-CN.md', 'index.md', 'toc.yml', 'docfx.json']:
            raise ValueError(f'Revision changes non-documentation file: {name}')
    return changed


def revision_build(tag, revision, persist_assets=True):
    if revision is None or revision < 1:
        raise ValueError('Document revisions start at 1')
    if output('git', 'status', '--porcelain'):
        raise ValueError('Revision checkout must be clean')
    manifest = restore(tag)
    existing = release_record(tag)
    if any(a['name'] in [f'provenance-r{revision}.json', f'revision-r{revision}.zip'] for a in existing['assets']):
        recover_site(tag, revision)
        recovered = ROOT / f'artifacts/recovered-archive/v{manifest["version"]}/r{revision}'
        info = json.loads((recovered / 'doc-info.json').read_text(encoding='utf-8'))
        if info['docs_commit'] != output('git', 'rev-parse', 'HEAD'):
            raise ValueError('Revision is already bound to another documentation commit')
        destination = ROOT / 'artifacts/docs/site'
        if destination.exists():
            raise ValueError('Revision restore requires an empty site output')
        shutil.copytree(recovered, destination)
        return {'version': manifest['version'], 'revision': revision, 'restored': True}
    base = manifest['commit']
    check_revision_changes(base)
    from documentation import build_docs, samples
    from build import build
    build()
    run('dotnet', 'build', ROOT / 'Fizzy.McapSharp.csproj', '-c', 'Release', '--no-incremental', '-p:DocumentationStrict=true')
    package = BUNDLE / f'Fizzy.McapSharp.{manifest["version"]}.nupkg'
    build_docs(package, revision, base, ROOT / 'bin/Release/net8.0/Fizzy.McapSharp.xml')
    samples(BUNDLE)
    revised = ROOT / f'artifacts/revision-r{revision}'
    revised.mkdir(parents=True, exist_ok=True)
    shutil.copy2(ROOT / 'artifacts/docs/docs.zip', revised / f'docs-r{revision}.zip')
    write_json(revised / f'provenance-r{revision}.json', {
        'schema': 1, 'version': manifest['version'], 'revision': revision, 'release_commit': base,
        'docs_commit': output('git', 'rev-parse', 'HEAD'), 'run_id': os.environ.get('GITHUB_RUN_ID'),
        'package_sha256': digest(package.read_bytes()), 'archive_sha256': digest((revised / f'docs-r{revision}.zip').read_bytes()),
        'files': json.loads((ROOT / 'artifacts/docs/files.json').read_text(encoding='utf-8'))})
    with zipfile.ZipFile(revised / f'revision-r{revision}.zip', 'w', zipfile.ZIP_DEFLATED) as bundle:
        for name in [f'docs-r{revision}.zip', f'provenance-r{revision}.json']:
            entry = zipfile.ZipInfo(name, (2000, 1, 1, 0, 0, 0))
            entry.compress_type = zipfile.ZIP_DEFLATED
            bundle.writestr(entry, (revised / name).read_bytes())
    if persist_assets:
        persist(tag, revised)
    return {'version': manifest['version'], 'revision': revision}


def set_outputs(values):
    if os.environ.get('GITHUB_OUTPUT'):
        with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as stream:
            for key, value in values.items():
                stream.write(f'{key}={str(value).lower() if isinstance(value, bool) else value}\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['check', 'start', 'resume', 'status', 'prepare', 'persist', 'restore', 'publish-state', 'wait-nuget', 'deploy-tree', 'online', 'finalize', 'revision', 'recover-site', 'assert-main', 'verify-entrypoints'])
    parser.add_argument('--tag')
    parser.add_argument('--site', type=Path, default=ROOT / 'artifacts/docs/site')
    parser.add_argument('--version')
    parser.add_argument('--revision', type=int)
    parser.add_argument('--rollback', type=int)
    parser.add_argument('--recovery', action='store_true')
    parser.add_argument('--activate', action='store_true')
    parser.add_argument('--promote', action='store_true')
    parser.add_argument('--docs-commit')
    parser.add_argument('--expected-revision', type=int)
    parser.add_argument('--expected-latest')
    args = parser.parse_args()
    if args.command == 'assert-main':
        assert_current_main(args.site)
        return {'main': 'current'}
    if args.command == 'verify-entrypoints':
        return verify_entrypoints(checked_version(args.version), expected_revision=args.expected_revision, expected_latest=args.expected_latest)
    if args.command in ['deploy-tree', 'online']:
        value = checked_version(args.version)
        if args.command == 'deploy-tree' and args.activate:
            if args.revision is None or not args.docs_commit:
                raise ValueError('Activation requires a fixed revision and the verified documentation commit')
            verify_online(value, args.revision, expected_commit=args.docs_commit)
        result = deploy_tree(args.site, value, args.revision, args.rollback, recovery=args.recovery, activate=args.activate, expected_commit=args.docs_commit, promote=args.promote) if args.command == 'deploy-tree' else verify_online(value, args.revision, expected_commit=args.docs_commit)
        set_outputs(result)
        return result
    if not args.tag:
        raise ValueError('--tag is required')
    value = tag_version(args.tag)
    if args.command == 'recover-site':
        result = recover_site(args.tag, args.revision)
        set_outputs(result)
        return result
    if args.command == 'revision':
        result = revision_build(args.tag, args.revision)
        set_outputs(result)
        return result
    if args.command in ['check', 'start']:
        record = release_record(args.tag)
        ready = bool(record and any(a['name'] in ['release-bundle.zip', 'provenance.json', 'candidate.json'] for a in record['assets']))
        result = local_check(args.tag, require_ci=not ready)
        if record and record['assets'] and not ready:
            raise ValueError('Partial candidate upload: rerun the original candidate job; never regenerate conflicting assets')
        if ready:
            restore(args.tag)
        result['reuse'] = ready
        set_outputs(result)
        if args.command == 'start':
            gh('workflow', 'run', 'publish.yml', '--repo', REPO, '--ref', args.tag)
        return result
    if args.command == 'resume':
        restore(args.tag)
        gh('workflow', 'run', 'publish.yml', '--repo', REPO, '--ref', args.tag)
        return {'dispatched': args.tag, 'mode': 'restore durable candidate'}
    if args.command == 'prepare':
        return prepare(args.tag)
    if args.command == 'persist':
        manifest = verify_bundle(BUNDLE, args.tag)
        persist(args.tag, BUNDLE)
        return {'candidate': 'durable', 'commit': manifest['commit']}
    if args.command == 'restore':
        return restore(args.tag, complete_upload=True)
    if args.command == 'status':
        record = release_record(args.tag)
        result = {'tag': args.tag, 'release_complete': False, 'release': 'public' if record and not record['draft'] else 'draft' if record else 'absent',
                  'resume': f'./scripts/Release.ps1 Resume -Tag {args.tag}'}
        if record and any(a['name'] in ['provenance.json', 'release-bundle.zip'] for a in record['assets']):
            manifest = restore(args.tag)
            package = BUNDLE / f'Fizzy.McapSharp.{value}.nupkg'
            data = nuget_bytes(value)
            if data is not None:
                compare_remote(package, data)
            result.update(commit=manifest['commit'], nuget='verified' if data else 'absent')
            result['published_validation_reports'] = [a['name'] for a in record['assets'] if a['name'].startswith('published-validation-')]
            runs = api(f'repos/{REPO}/actions/workflows/publish.yml/runs?head_sha={manifest["commit"]}&per_page=20')['workflow_runs']
            result['workflow_runs'] = [{'id': r['id'], 'status': r['status'], 'conclusion': r['conclusion'], 'url': r['html_url']} for r in runs if r['head_sha'] == manifest['commit']]
            try:
                result.update(verify_online(value, 0, attempts=1, expected_commit=manifest['commit']))
                with urllib.request.urlopen(SITE + 'versions.json', timeout=30) as response:
                    entries = json.load(response)['entries']
                result['active_revision'] = next(e['current'] for e in entries if e['version'] == value)
            except Exception as error:
                result['pages'] = str(error)
            result['release_complete'] = result.get('nuget') == 'verified' and result.get('pages') == 'verified' and result['release'] == 'public' and bool(result['published_validation_reports'])
        return result
    manifest = verify_bundle(BUNDLE, args.tag)
    if remote_commit(args.tag) != manifest['commit']:
        raise ValueError('Remote tag moved')
    package = BUNDLE / f'Fizzy.McapSharp.{value}.nupkg'
    if args.command == 'publish-state':
        data = nuget_bytes(value)
        if data is not None:
            compare_remote(package, data)
        result = {'exists': data is not None, 'package': str(package)}
        set_outputs(result)
        return result
    if args.command == 'wait-nuget':
        return wait_nuget(package)
    if args.command == 'finalize':
        wait_nuget(package)
        result = verify_online(value, 0, expected_commit=manifest['commit'])
        notes = ROOT / 'artifacts/release-notes.md'
        notes.write_text(f'{args.tag}\n\nSource: `{manifest["commit"]}`\n\n[Documentation]({SITE}v{value}/)\n\nPackage, public-source smoke tests and documentation deployment verified. Immutable provenance and validation reports are attached.\n', encoding='utf-8')
        gh('release', 'edit', args.tag, '--repo', REPO, '--draft=false', '--prerelease=' + str('-' in value).lower(), '--notes-file', notes)
        return result


if __name__ == '__main__':
    report = {'status': 'failed', 'command': sys.argv[1:2]}
    try:
        report.update(main() or {})
        report['status'] = 'passed'
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        report['error'] = str(error)
        raise
    finally:
        write_json(ROOT / 'artifacts/reports/release.json', report)

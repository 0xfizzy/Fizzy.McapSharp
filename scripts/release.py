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
        if (info['version'], info['release_commit'], info['docs_commit'], info.get('channel', 'release')) != (value, manifest['commit'], manifest['commit'], 'release'):
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
    files = sorted((p for p in folder.glob('*') if candidate_names is None or p.name in candidate_names), key=lambda p: (0 if p.name == 'release-bundle.zip' else 2 if p.name == 'provenance.json' else 1, p.name))
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


# Documentation publication lives separately from immutable package publication.
from docs_archive import merge_site, deploy_tree, complete_deployment, assert_current_main, verify_online


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



def check_update_changes(base):
    changed = output('git', 'diff', '--name-only', base, 'HEAD').splitlines()
    allowed = ('docs/',)
    for name in changed:
        if name.startswith('src/') and name.endswith('.cs'):
            before = subprocess.check_output(['git', 'show', f'{base}:{name}'], cwd=ROOT, text=True)
            after = (ROOT / name).read_text(encoding='utf-8')
            strip = without_xml_comments
            if strip(before) != strip(after):
                raise ValueError(f'Only XML-comment changes are allowed in documentation source: {name}')
        elif name.startswith('samples/Documentation/') and name.endswith('.cs'):
            continue
        elif not name.startswith(allowed) and name not in ['README.md', 'README.zh-CN.md', 'index.md', 'toc.yml']:
            raise ValueError(f'Documentation update changes non-documentation file: {name}')
    return changed


def update_build(tag):
    manifest = restore(tag)
    check_update_changes(manifest['commit'])
    from documentation import build_docs, samples
    from build import build
    build()
    run('dotnet', 'build', ROOT / 'Fizzy.McapSharp.csproj', '-c', 'Release', '--no-incremental', '-p:DocumentationStrict=true')
    package = BUNDLE / f'Fizzy.McapSharp.{manifest["version"]}.nupkg'
    build_docs(package, manifest['commit'], ROOT / 'bin/Release/net8.0/Fizzy.McapSharp.xml')
    samples(BUNDLE)
    return {'version': manifest['version']}


def set_outputs(values):
    if os.environ.get('GITHUB_OUTPUT'):
        with open(os.environ['GITHUB_OUTPUT'], 'a', encoding='utf-8') as stream:
            for key, value in values.items():
                stream.write(f'{key}={str(value).lower() if isinstance(value, bool) else value}\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['check', 'start', 'resume', 'status', 'prepare', 'persist', 'restore', 'publish-state', 'wait-nuget', 'finalize', 'update'])
    parser.add_argument('--tag', required=True)
    args = parser.parse_args()
    value = tag_version(args.tag)
    if args.command == 'update':
        result = update_build(args.tag)
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
                result.update(verify_online(value, True, attempts=1, expected_commit=manifest['commit']))
                with urllib.request.urlopen(SITE + 'versions.json', timeout=30) as response:
                    entries = json.load(response)['entries']
                result['listed'] = any(e['version'] == value for e in entries)
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
        result = verify_online(value, True, expected_commit=manifest['commit'])
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

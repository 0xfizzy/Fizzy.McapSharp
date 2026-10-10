"""One current document tree per package, with durable deployment payloads in Git."""
import argparse
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import urllib.request
from pathlib import Path
from build import ROOT, output
from documentation import tree_hashes, write_json, check_site

REPO = '0xfizzy/Fizzy.McapSharp'
SITE = 'https://0xfizzy.github.io/Fizzy.McapSharp/'

def git_at(folder, *args):
    return subprocess.check_output(['git', '-C', str(folder), *map(str, args)], text=True).strip()

def archive_checkout(remote_url=None):
    parent = ROOT / 'artifacts/pages'
    parent.mkdir(parents=True, exist_ok=True)
    folder = Path(tempfile.mkdtemp(prefix='archive-', dir=parent))
    git_at(folder, 'init')
    git_at(folder, 'config', 'core.autocrlf', 'false')
    git_at(folder, 'config', 'user.name', 'github-actions[bot]')
    git_at(folder, 'config', 'user.email', '41898282+github-actions[bot]@users.noreply.github.com')
    git_at(folder, 'remote', 'add', 'origin', remote_url or f'https://github.com/{REPO}.git')
    if remote_url is None:
        header = output('git', 'config', '--get', 'http.https://github.com/.extraheader')
        git_at(folder, 'config', 'http.https://github.com/.extraheader', header)
    refs = git_at(folder, 'ls-remote', '--heads', 'origin', 'gh-pages')
    if refs:
        git_at(folder, 'fetch', 'origin', 'gh-pages')
        git_at(folder, 'checkout', '-b', 'gh-pages', 'FETCH_HEAD')
    else:
        git_at(folder, 'checkout', '--orphan', 'gh-pages')
    (folder / '.gitattributes').write_text('* -text\n', encoding='utf-8')
    return folder

def archive_head():
    refs = output('git', 'ls-remote', '--heads', 'origin', 'gh-pages')
    return refs.split()[0] if refs else 'empty'

def save(folder, message):
    git_at(folder, 'add', '--all')
    if git_at(folder, 'status', '--porcelain'):
        git_at(folder, 'commit', '-m', message)
        git_at(folder, 'push', 'origin', 'HEAD:refs/heads/gh-pages')
    return git_at(folder, 'rev-parse', 'HEAD')

def identity(site):
    return json.loads((site / 'doc-info.json').read_text(encoding='utf-8'))

def assert_current_main(site):
    from release import remote_commit
    info = identity(site)
    if info.get('channel') != 'dev' or info['docs_commit'] != remote_commit('main'):
        raise ValueError('Stale main documentation build refused')

def refresh_index(root):
    from release import checked_version, redirect
    entries = []
    for path in root.glob('v*/doc-info.json'):
        value = checked_version(path.parent.name[1:])
        entries.append({'version': value, 'completed': True})
    entries.sort(key=lambda e: tuple(map(int, e['version'].split('-')[0].split('.'))), reverse=True)
    stable = [e['version'] for e in entries if '-' not in e['version']]
    latest = stable[0] if stable else None
    write_json(root / 'versions.json', {'schema': 2, 'latest': latest, 'dev': (root / 'dev/index.html').exists(), 'entries': entries})
    if latest:
        redirect(root / 'latest/index.html', f'../v{latest}/index.html')
        redirect(root / 'index.html', 'latest/index.html')
    elif (root / 'dev/index.html').exists():
        redirect(root / 'index.html', 'dev/index.html')
    (root / '.nojekyll').touch()

def migrate(root):
    # No redirects: remove old numbered public URLs and their control files.
    for directory in root.glob('v*'):
        pointer = directory / 'current.json'
        if pointer.exists():
            number = json.loads(pointer.read_text(encoding='utf-8'))['revision']
            old = directory / f'r{number}'
            if not old.is_dir():
                raise ValueError('Missing selected legacy documentation')
            temp = directory.parent / '_migration'
            shutil.copytree(old, temp)
            shutil.rmtree(directory)
            temp.rename(directory)
        for child in list(directory.glob('r[0-9]*')) + [directory / 'completed', directory / 'current.json']:
            if child.is_dir(): shutil.rmtree(child)
            elif child.exists(): child.unlink()
    for page in list(root.glob('v*/doc-info.json')) + list(root.glob('dev/doc-info.json')):
        info = json.loads(page.read_text(encoding='utf-8'))
        legacy = 'revision' in info
        info.pop('revision', None)
        info['channel'] = 'dev' if page.parent.name == 'dev' else 'release'
        write_json(page, info)
        if legacy: shutil.copy2(ROOT / 'scripts/docsite/version.js', page.parent / 'version.js')

def merge_site(root, site, value, fixed=False):
    from release import checked_version
    checked_version(value)
    tree_hashes(site)  # Refuse linked input before copytree can dereference it.
    info = identity(site)
    if info['version'] != value or info['channel'] != ('release' if fixed else 'dev'):
        raise ValueError('Site identity does not match destination')
    root.mkdir(parents=True, exist_ok=True)
    migrate(root)
    target = root / (f'v{value}' if fixed else 'dev')
    if target.exists(): shutil.rmtree(target)
    shutil.copytree(site, target)
    refresh_index(root)

def run_key(run_id):
    if not re.fullmatch(r'[0-9]+', str(run_id)): raise ValueError('Numeric run ID required')
    return str(run_id)

def deploy_tree(site, value, fixed=False, expected_archive=None, run_id=None, remote_url=None, initial=False):
    if not fixed and remote_url is None: assert_current_main(site)
    folder = archive_checkout(remote_url)
    try: head = git_at(folder, 'rev-parse', 'HEAD')
    except subprocess.CalledProcessError: head = 'empty'
    if (fixed or expected_archive is not None) and expected_archive != head:
        raise ValueError('Archive changed since preparation; review and prepare again')
    existing = folder / f'v{value}'
    if initial and existing.exists() and tree_hashes(existing) != tree_hashes(site):
        raise ValueError('Initial package publication cannot overwrite corrected documentation; use Update-Docs')
    key = run_key(run_id or os.environ['GITHUB_RUN_ID'])
    payload = folder / '.deployments' / key
    if payload.exists():
        raise ValueError('Run already archived; use Resume without rebuilding')
    shutil.copytree(site, payload / 'site')
    write_json(payload / 'manifest.json', {'version': value, 'fixed': fixed, 'base': head, 'run_id': key, 'files': tree_hashes(site)})
    candidate = save(folder, f'docs: retain candidate {key}')
    export = Path(tempfile.mkdtemp(prefix='export-', dir=ROOT / 'artifacts/pages'))
    for child in folder.iterdir():
        if child.name.startswith('.'): continue
        if child.is_dir(): shutil.copytree(child, export / child.name)
        else: shutil.copy2(child, export / child.name)
    merge_site(export, site, value, fixed)
    check_site(export / (f'v{value}' if fixed else 'dev'))
    return {'export': str(export), 'archive': str(folder), 'candidate': candidate, 'run_id': key, 'docs_commit': identity(site)['docs_commit'], 'latest': json.loads((export / 'versions.json').read_text())['latest'] or '', 'content_sha256': identity(site).get('content_sha256', '')}

def complete_deployment(folder, run_id, candidate):
    folder = Path(folder)
    remote = git_at(folder, 'ls-remote', '--heads', 'origin', 'gh-pages').split()[0]
    if remote != candidate: raise ValueError('Archive changed during deployment')
    payload = folder / '.deployments' / run_key(run_id)
    manifest = json.loads((payload / 'manifest.json').read_text(encoding='utf-8'))
    if tree_hashes(payload / 'site') != manifest['files']: raise ValueError('Candidate content changed')
    merge_site(folder, payload / 'site', manifest['version'], manifest['fixed'])
    write_json(payload / 'success.json', {'candidate': candidate, 'docs_commit': identity(payload / 'site')['docs_commit']})
    return {'archive_commit': save(folder, f'docs: completed deployment {run_id}')}

def recover_payload(run_id=None, archive_commit=None, value=None, remote_url=None, expected_archive=None):
    if value is not None:
        from release import checked_version
        checked_version(value)
    folder = archive_checkout(remote_url)
    baseline = git_at(folder, 'rev-parse', 'HEAD')
    if expected_archive is not None and expected_archive != baseline:
        raise ValueError('Archive changed since recovery preparation')
    if archive_commit:
        if not re.fullmatch(r'[a-f0-9]{40}', archive_commit): raise ValueError('Full archive commit SHA required')
        git_at(folder, 'merge-base', '--is-ancestor', archive_commit, 'HEAD')
        git_at(folder, 'checkout', '--detach', archive_commit)
        completed = list(folder.glob('.deployments/*/success.json'))
        matches = [p for p in completed if json.loads((p.parent / 'manifest.json').read_text())['version'] == value]
        if not matches or not (folder / f'v{value}/doc-info.json').exists(): raise ValueError('No successful deployment for selected version')
        hashes = tree_hashes(folder / f'v{value}')
        if not any(json.loads((p.parent / 'manifest.json').read_text())['files'] == hashes for p in matches):
            raise ValueError('Selected archive tree does not match a successful deployment')
        return folder / f'v{value}', baseline
    payload = folder / '.deployments' / run_key(run_id)
    manifest = json.loads((payload / 'manifest.json').read_text(encoding='utf-8'))
    if (value is not None and manifest['version'] != value) or tree_hashes(payload / 'site') != manifest['files']:
        raise ValueError('Candidate identity or hashes differ')
    if (payload / 'success.json').exists(): raise ValueError('Run already completed; use explicit Rollback')
    # Resume only an unchanged failed candidate; later completed updates must win.
    candidate = git_at(folder, 'log', '-1', '--format=%H', '--', f'.deployments/{run_id}/manifest.json')
    if git_at(folder, 'rev-parse', 'HEAD') != candidate: raise ValueError('Candidate superseded; prepare a reviewed update')
    return payload / 'site', candidate

def verify_online(value, fixed=False, attempts=12, delay=10, expected_commit=None, expected_latest=None, expected_content=None, expected_site=None):
    prefix = f'v{value}/' if fixed else 'dev/'
    for attempt in range(attempts):
        try:
            with urllib.request.urlopen(SITE + prefix + 'doc-info.json', timeout=30) as response: info = json.load(response)
            if info['version'] != value or info['channel'] != ('release' if fixed else 'dev') or (expected_commit and info['docs_commit'] != expected_commit): raise ValueError('Online identity is stale')
            if expected_content and info.get('content_sha256') != expected_content: raise ValueError('Online content fingerprint is stale')
            for name in ['index.html', 'docs/zh-CN/index.html', 'api/Fizzy.McapSharp.html', 'version.js']:
                with urllib.request.urlopen(SITE + prefix + name, timeout=30) as response:
                    if response.status != 200: raise ValueError('Missing online resource')
            if expected_latest is not None:
                with urllib.request.urlopen(SITE + 'versions.json', timeout=30) as response: index = json.load(response)
                if (index['latest'] or '') != expected_latest: raise ValueError('Online latest is stale')
                if fixed and not any(e['version'] == value for e in index['entries']): raise ValueError('Version missing from index')
            if expected_site is not None:
                verify_online_files(SITE + prefix, Path(expected_site))
            return {'pages': 'verified'}
        except Exception:
            if attempt + 1 == attempts: raise
            time.sleep(delay)

def verify_online_files(base, site):
    from concurrent.futures import ThreadPoolExecutor
    from urllib.parse import quote
    from documentation import digest
    manifest = tree_hashes(site)
    def verify(item):
        name, expected = item
        with urllib.request.urlopen(base + quote(name), timeout=60) as response:
            if digest(response.read()) != expected:
                raise ValueError(f'Online file differs from candidate: {name}')
    with ThreadPoolExecutor(max_workers=8) as executor:
        list(executor.map(verify, manifest.items()))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('action', choices=['stage', 'complete', 'online', 'assert-main', 'recover'])
    parser.add_argument('--site', type=Path, default=ROOT / 'artifacts/docs/site')
    parser.add_argument('--version')
    parser.add_argument('--fixed', action='store_true')
    parser.add_argument('--initial', action='store_true')
    parser.add_argument('--expected-latest')
    parser.add_argument('--expected-content')
    parser.add_argument('--expected-archive')
    parser.add_argument('--run-id')
    parser.add_argument('--archive-commit')
    parser.add_argument('--archive')
    parser.add_argument('--candidate')
    parser.add_argument('--docs-commit')
    args = parser.parse_args()
    if args.action == 'stage': result = deploy_tree(args.site, args.version, args.fixed, args.expected_archive, initial=args.initial)
    elif args.action == 'complete': result = complete_deployment(args.archive, args.run_id, args.candidate)
    elif args.action == 'online': result = verify_online(args.version, args.fixed, expected_commit=args.docs_commit, expected_latest=args.expected_latest, expected_content=args.expected_content, expected_site=args.site)
    elif args.action == 'assert-main': assert_current_main(args.site); result = {}
    else:
        site, candidate = recover_payload(args.run_id, args.archive_commit, args.version, expected_archive=args.expected_archive)
        shutil.copytree(site, args.site)
        result = {'expected_archive': candidate or archive_head()}
    from release import set_outputs
    set_outputs(result)
    print(json.dumps(result))

if __name__ == '__main__': main()

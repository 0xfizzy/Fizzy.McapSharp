"""Scripted documentation revision preparation, validation, dispatch and recovery."""
import argparse
import json
import re
import sys
import subprocess
from pathlib import Path

from build import ROOT, output, run
from documentation import check_sources, write_json
from release import (REPO, SITE, BUNDLE, tag_version, release_record, restore, revision_build,
                     check_revision_changes, remote_commit, gh, verify_online, api, recover_site)


def inventory(tag):
    record = release_record(tag)
    if not record or record['draft']:
        raise ValueError('Documentation revisions require an existing completed public release')
    names = [a['name'] for a in record['assets']]
    numbers = {int(re.search(r'-r(\d+)', name)[1]) for name in names if re.fullmatch(r'(?:docs-r\d+\.zip|provenance-r\d+\.json|revision-r\d+\.zip)', name)}
    return record, numbers


def clean():
    if output('git', 'status', '--porcelain'):
        raise ValueError('Commit reviewed changes first; revision operations require a clean checkout')


def check(tag, revision, validate=False, remote=False):
    clean()
    tag_version(tag)
    manifest = restore(tag)
    if revision is None or revision < 1:
        raise ValueError('A new revision must be at least 1')
    changed = check_revision_changes(manifest['commit'])
    check_sources()
    commit = output('git', 'rev-parse', 'HEAD')
    branch = output('git', 'symbolic-ref', '--short', 'HEAD')
    if remote and remote_commit(branch) != commit:
        raise ValueError('Push the reviewed revision branch before Start')
    if validate:
        revision_build(tag, revision, persist_assets=False)
    return {'tag': tag, 'revision': revision, 'branch': branch, 'docs_commit': commit, 'changed': changed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['prepare', 'check', 'start', 'status', 'resume', 'rollback'])
    parser.add_argument('--tag', required=True)
    parser.add_argument('--revision', type=int)
    parser.add_argument('--branch')
    args = parser.parse_args()
    value = tag_version(args.tag)
    record, numbers = inventory(args.tag)
    revision = args.revision
    if args.action == 'prepare':
        clean()
        manifest = restore(args.tag)
        revision = revision if revision is not None else max(numbers, default=0) + 1
        if revision != max(numbers, default=0) + 1:
            raise ValueError('Prepare must reserve the next revision after all durable revision assets')
        branch = args.branch or f'docs/{args.tag}-r{revision}'
        run('git', 'check-ref-format', '--branch', branch)
        if output('git', 'rev-parse', f'{args.tag}^{{commit}}') != manifest['commit']:
            raise ValueError('Local release tag differs from original provenance')
        import urllib.request
        with urllib.request.urlopen(SITE + f'v{value}/current.json', timeout=30) as response:
            active = json.load(response)['revision']
        restored = recover_site(args.tag, active)
        info = json.loads((Path(restored['recovered']) / f'v{value}/r{active}/doc-info.json').read_text(encoding='utf-8'))
        verify_online(value, active, expected_commit=info['docs_commit'])
        # Keep the previous correction set; executable-change checks still use the original package commit.
        exists = subprocess.run(['git', 'cat-file', '-e', info['docs_commit'] + '^{commit}'], cwd=ROOT, capture_output=True)
        if exists.returncode:
            run('git', 'fetch', 'origin', info['docs_commit'])
        run('git', 'cat-file', '-e', info['docs_commit'] + '^{commit}')
        run('git', 'switch', '-c', branch, info['docs_commit'])
        return {'branch': branch, 'revision': revision, 'base_commit': info['docs_commit'], 'base_revision': active,
                'next': f'Edit translations/XML/guides, commit, run Revision-Docs.ps1 Check -Tag {args.tag} -Revision {revision}, then push and Start.'}
    if revision is None or revision < 0:
        raise ValueError('--revision is required and must be nonnegative')
    if args.action == 'check':
        return check(args.tag, revision, validate=True)
    if args.action == 'start':
        identity = check(args.tag, revision, remote=True)
        if revision not in numbers and revision != max(numbers, default=0) + 1:
            raise ValueError('Revision sequence has a gap')
        gh('workflow', 'run', 'docs-revision.yml', '--repo', REPO, '--ref', identity['branch'],
           '-f', f'tag={args.tag}', '-f', f'revision={revision}', '-f', 'rollback=false')
        return dict(identity, dispatched=True)
    if args.action == 'status':
        names = {a['name'] for a in record['assets']}
        durable = revision == 0 or f'revision-r{revision}.zip' in names or {f'docs-r{revision}.zip', f'provenance-r{revision}.json'}.issubset(names)
        result = {'tag': args.tag, 'revision': revision, 'durable': durable, 'upload_state': 'complete' if durable else 'partial' if revision in numbers else 'absent'}
        try:
            result.update(verify_online(value, revision, attempts=1))
            import urllib.request
            with urllib.request.urlopen(SITE + f'v{value}/current.json', timeout=30) as response:
                result['active'] = json.load(response)['revision'] == revision
        except Exception as error:
            result['online_error'] = str(error)
        result['runs'] = [{'id': r['id'], 'status': r['status'], 'conclusion': r['conclusion'], 'url': r['html_url']}
                          for r in api(f'repos/{REPO}/actions/workflows/docs-revision.yml/runs?per_page=20')['workflow_runs']]
        return result
    # Resume/rollback use trusted main restoration without rebuilding immutable content.
    restore(args.tag)
    names = {a['name'] for a in record['assets']}
    if revision and not (f'revision-r{revision}.zip' in names or {f'docs-r{revision}.zip', f'provenance-r{revision}.json'}.issubset(names)):
        raise ValueError('No complete durable revision exists; rerun the original failed revision build, never reconstruct it')
    if args.action == 'resume':
        gh('workflow', 'run', 'docs-restore.yml', '--repo', REPO, '--ref', 'main',
           '-f', f'tag={args.tag}', '-f', f'revision={revision}', '-f', 'activate_revision=true')
    else:
        gh('workflow', 'run', 'docs-revision.yml', '--repo', REPO, '--ref', 'main',
           '-f', f'tag={args.tag}', '-f', f'revision={revision}', '-f', 'rollback=true')
    return {'tag': args.tag, 'revision': revision, 'dispatched': args.action}


if __name__ == '__main__':
    report = {'status': 'failed'}
    try:
        report.update(main())
        report['status'] = 'passed'
        print(json.dumps(report, ensure_ascii=False))
    except Exception as error:
        report['error'] = str(error)
        raise
    finally:
        write_json(ROOT / 'artifacts/reports/revision.json', report)

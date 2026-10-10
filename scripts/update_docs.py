"""Prepare, validate, dispatch and recover same-version documentation updates."""
import argparse
import json
import re
from pathlib import Path
from build import ROOT, output, run
from documentation import write_json
from release import REPO, gh, release_record, restore, tag_version, check_update_changes, update_build
from docs_archive import archive_head

def clean():
    if output('git', 'status', '--porcelain'): raise ValueError('Commit reviewed changes first; clean checkout required')

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['prepare', 'check', 'start', 'status', 'resume', 'rollback'])
    parser.add_argument('--tag', required=True)
    parser.add_argument('--source-ref')
    parser.add_argument('--run-id')
    parser.add_argument('--archive-commit')
    parser.add_argument('--branch')
    args = parser.parse_args()
    if args.tag != 'dev':
        tag_version(args.tag)
        record = release_record(args.tag)
        if not record or record['draft']: raise ValueError('Documentation updates require a completed public release')
    elif args.action not in ['status', 'resume']:
        raise ValueError('Use docs workflow on main to build Development; Tag dev supports Status and Resume')
    state = ROOT / 'artifacts/docs-update.json'
    if args.action == 'status':
        if args.run_id: return json.loads(gh('run', 'view', args.run_id, '--repo', REPO, '--json', 'status,conclusion,url,headSha'))
        return json.loads(gh('run', 'list', '--repo', REPO, '--workflow', 'docs-update.yml', '--json', 'databaseId,status,conclusion,url'))
    if args.action == 'prepare':
        clean()
        manifest = restore(args.tag)
        run('git', 'fetch', 'origin', 'gh-pages')
        baseline = output('git', 'rev-parse', 'FETCH_HEAD')
        if args.source_ref:
            source = output('git', 'rev-parse', args.source_ref + '^{commit}')
        else:
            info = json.loads(output('git', 'show', f"{baseline}:{args.tag}/doc-info.json"))
            if info['version'] != tag_version(args.tag) or info['release_commit'] != manifest['commit'] or info['channel'] != 'release':
                raise ValueError('Archived documentation differs from original package provenance')
            source = info['docs_commit']
        if not re.fullmatch('[a-f0-9]{40}', source): raise ValueError('Full documentation source SHA required')
        run('git', 'fetch', 'origin', source)
        branch = args.branch or f'docs/{args.tag}-update'
        run('git', 'switch', '-c', branch, source)
        write_json(state, {'tag': args.tag, 'expected_archive': baseline, 'package_commit': manifest['commit']})
        return {'branch': branch, 'source_ref': source, 'expected_archive': baseline}
    if args.action in ['check', 'start']:
        clean()
        commit = output('git', 'rev-parse', (args.source_ref or 'HEAD') + '^{commit}')
        if commit != output('git', 'rev-parse', 'HEAD'): raise ValueError('Check and Start require SourceRef checked out')
        manifest = restore(args.tag)
        check_update_changes(manifest['commit'])
        if args.action == 'check': return update_build(args.tag)
        baseline = json.loads(state.read_text())['expected_archive'] if state.exists() and json.loads(state.read_text())['tag'] == args.tag else archive_head()
        if archive_head() != baseline: raise ValueError('Archive changed since Prepare; review and prepare again')
        # GitHub checkout validates that the immutable source SHA is available remotely.
        fields = {'tag': args.tag, 'source_ref': commit, 'expected_archive': baseline, 'mode': 'build'}
    elif args.action == 'resume':
        if not args.run_id or not args.run_id.isdigit(): raise ValueError('Resume requires RunId')
        fields = {'tag': args.tag, 'run_id': args.run_id, 'mode': 'resume', 'expected_archive': archive_head()}
    else:
        if not args.archive_commit or not re.fullmatch('[a-f0-9]{40}', args.archive_commit): raise ValueError('Rollback requires full ArchiveCommit SHA')
        fields = {'tag': args.tag, 'archive_commit': args.archive_commit, 'mode': 'rollback', 'expected_archive': archive_head()}
    arguments = ['workflow', 'run', 'docs-update.yml', '--repo', REPO, '--ref', 'main']
    for name, value in fields.items(): arguments += ['-f', f'{name}={value}']
    gh(*arguments)
    return dict(fields, dispatched=True)

if __name__ == '__main__':
    result = {'status': 'failed'}
    try:
        result.update(main()); result['status'] = 'passed'; print(json.dumps(result))
    except Exception as error:
        result['error'] = str(error); raise
    finally: write_json(ROOT / 'artifacts/reports/docs-update.json', result)

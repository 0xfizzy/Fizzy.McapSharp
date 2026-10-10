"""Offline release drills: real archive bytes, simulated NuGet responses, no publication."""
import io
import json
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import release
import documentation
import revision
from documentation import tree_hashes, write_json, Page


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        (release.ROOT / 'artifacts').mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=release.ROOT / 'artifacts', prefix='release-test-')
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / 'archive'

    def site(self, value, revision, text='original'):
        site = self.base / f'site-{value}-{revision}-{text}'
        site.mkdir()
        write_json(site / 'doc-info.json', dict(version=value, revision=revision, docs_commit='a' * 40, release_commit='a' * 40))
        (site / 'index.html').write_text(text)
        return site

    def merge(self, root, site, value, revision=None):
        release.merge_site(root, site, value, revision, activate=True)

    def test_stage_is_invisible_until_verified_activation(self):
        site = self.site('1.0.0', 0)
        release.merge_site(self.root, site, '1.0.0', 0)
        self.assertTrue((self.root / 'v1.0.0/r0/index.html').exists())
        self.assertFalse((self.root / 'v1.0.0/current.json').exists())
        self.assertIsNone(json.loads((self.root / 'versions.json').read_text())['latest'])
        with self.assertRaisesRegex(ValueError, 'identity'):
            release.activate_site(self.root, '1.0.0', 0, expected_commit='b' * 40)
        release.activate_site(self.root, '1.0.0', 0, expected_commit='a' * 40)
        self.assertEqual('1.0.0', json.loads((self.root / 'versions.json').read_text())['latest'])
        original = tree_hashes(self.root / 'v1.0.0/r0')
        release.merge_site(self.root, self.site('1.0.0', 1, 'new'), '1.0.0', 1)
        self.assertEqual(0, json.loads((self.root / 'v1.0.0/current.json').read_text())['revision'])
        self.assertEqual(original, tree_hashes(self.root / 'v1.0.0/r0'))

    def test_completed_revision_retry_does_not_undo_explicit_rollback(self):
        original = self.site('1.0.0', 0)
        revised = self.site('1.0.0', 1)
        self.merge(self.root, original, '1.0.0', 0)
        self.merge(self.root, revised, '1.0.0', 1)
        release.activate_site(self.root, '1.0.0', 0, rollback=True)
        self.merge(self.root, revised, '1.0.0', 1)
        self.assertEqual(0, json.loads((self.root / 'v1.0.0/current.json').read_text())['revision'])

    def test_rollback_rejects_staged_but_uncompleted_revision(self):
        release.merge_site(self.root, self.site('1.0.0', 0), '1.0.0', 0)
        with self.assertRaisesRegex(ValueError, 'previously completed'):
            release.activate_site(self.root, '1.0.0', 0, rollback=True)
        self.assertFalse((self.root / 'v1.0.0/current.json').exists())

    def test_entrypoint_verification_rejects_stale_cdn_index(self):
        stale = {'entries': [{'version': '1.0.0', 'current': 0, 'completed': True}], 'latest': '1.0.0'}
        with patch('urllib.request.urlopen', return_value=io.BytesIO(json.dumps(stale).encode())):
            with self.assertRaisesRegex(RuntimeError, 'active revision is stale'):
                release.verify_entrypoints('1.0.0', attempts=1, expected_revision=1)
        with patch('urllib.request.urlopen', return_value=io.BytesIO(json.dumps(stale).encode())):
            with self.assertRaisesRegex(RuntimeError, 'latest version is stale'):
                release.verify_entrypoints('1.0.0', attempts=1, expected_revision=0, expected_latest='2.0.0')

    def test_stale_main_is_rejected(self):
        site = self.site('1.0.0', None)
        with patch.object(release, 'remote_commit', return_value='b' * 40):
            with self.assertRaisesRegex(ValueError, 'Stale main'):
                release.assert_current_main(site)
        with patch.object(release, 'remote_commit', return_value='a' * 40):
            release.assert_current_main(site)

    def test_xml_only_guard_preserves_multiline_string_payloads(self):
        before = '/// Old summary\npublic class C {}\n'
        after = '/// New summary\n/// Another line\npublic class C {}\n'
        self.assertEqual(release.without_xml_comments(before), release.without_xml_comments(after))
        for literal in ['@"\n/// payload\n"', '"""\n/// payload\n"""']:
            self.assertNotEqual(release.without_xml_comments(literal), release.without_xml_comments(literal.replace('payload', 'changed')))

    def package(self, payload=b'assembly', signed=False):
        data = io.BytesIO()
        with zipfile.ZipFile(data, 'w') as z:
            z.writestr('Fizzy.McapSharp.nuspec', '<package><metadata><id>Fizzy.McapSharp</id><version>1.0.0</version></metadata></package>')
            z.writestr('lib/net8.0/Fizzy.McapSharp.dll', payload)
            if signed:
                z.writestr('.signature.p7s', b'repository-signature')
        return data.getvalue()

    def test_two_versions_revision_and_dev_preserve_original(self):
        self.merge(self.root, self.site('1.0.0', 0), '1.0.0', 0)
        original = tree_hashes(self.root / 'v1.0.0/r0')
        self.merge(self.root, self.site('2.0.0', 0), '2.0.0', 0)
        self.merge(self.root, self.site('1.0.0', 1, 'corrected'), '1.0.0', 1)
        self.merge(self.root, self.site('3.0.0', None), '3.0.0')
        self.assertEqual(original, tree_hashes(self.root / 'v1.0.0/r0'))
        self.assertEqual(1, json.loads((self.root / 'v1.0.0/current.json').read_text())['revision'])
        self.assertEqual('2.0.0', json.loads((self.root / 'versions.json').read_text())['latest'])

    def test_duplicate_and_conflict(self):
        site = self.site('1.0.0', 0)
        self.merge(self.root, site, '1.0.0', 0)
        before = tree_hashes(self.root)
        self.merge(self.root, site, '1.0.0', 0)
        self.assertEqual(before, tree_hashes(self.root))
        with self.assertRaisesRegex(ValueError, 'Immutable'):
            self.merge(self.root, self.site('1.0.0', 0, 'changed'), '1.0.0', 0)

    def test_semantic_latest_and_old_retry(self):
        sites = {}
        for value in ['1.9.0', '1.10.0', '2.0.0-beta.1']:
            sites[value] = self.site(value, 0)
            self.merge(self.root, sites[value], value, 0)
        self.merge(self.root, self.site('1.9.0', 1), '1.9.0', 1)
        self.merge(self.root, sites['1.9.0'], '1.9.0', 0)
        self.assertEqual('1.10.0', json.loads((self.root / 'versions.json').read_text())['latest'])
        self.assertEqual(1, json.loads((self.root / 'v1.9.0/current.json').read_text())['revision'])

    def test_identity_gap_and_unsafe_versions(self):
        with self.assertRaises(ValueError):
            self.merge(self.root, self.site('1.0.0', 0), '2.0.0', 0)
        with self.assertRaises(ValueError):
            self.merge(self.root, self.site('1.0.0', 2), '1.0.0', 2)
        for value in ['../escape', '01.0.0', '1.0', '1.0.0-alpha.01']:
            with self.assertRaises(ValueError):
                release.checked_version(value)

    def test_zip_traversal_rejected(self):
        archive = self.base / 'bad.zip'
        with zipfile.ZipFile(archive, 'w') as z:
            z.writestr('../outside', 'bad')
        with self.assertRaisesRegex(ValueError, 'Unsafe'):
            release.unpack_site(archive, self.base / 'unpack')
        self.assertFalse((self.base / 'outside').exists())

    def test_signed_package_and_conflicting_content(self):
        package = self.base / 'candidate.nupkg'
        package.write_bytes(self.package())
        with patch.object(release, 'BUNDLE', self.base):
            release.compare_remote(package, self.package(signed=True))
            with self.assertRaisesRegex(ValueError, 'conflicts'):
                release.compare_remote(package, self.package(b'wrong'))

    def test_response_lost_then_visible_without_republish(self):
        package = self.base / 'candidate.nupkg'
        package.write_bytes(self.package())
        replies = iter([None, None, self.package(signed=True)])
        sleeps = []
        with patch.object(release, 'BUNDLE', self.base):
            result = release.wait_nuget(package, attempts=3, fetch=lambda _: next(replies), sleep=sleeps.append)
        self.assertEqual('verified', result['nuget'])
        self.assertEqual(2, len(sleeps))

    def test_polling_deadline(self):
        package = self.base / 'candidate.nupkg'
        package.write_bytes(self.package())
        with self.assertRaisesRegex(RuntimeError, 'deadline'):
            release.wait_nuget(package, attempts=2, fetch=lambda _: None, sleep=lambda _: None)

    def test_restore_site_without_transient_artifacts(self):
        site = self.site('1.0.0', 0)
        original = tree_hashes(site)
        archive = self.base / 'durable.zip'
        with zipfile.ZipFile(archive, 'w') as z:
            for name in original:
                z.write(site / name, name)
        restored = self.base / 'restored'
        release.unpack_site(archive, restored)
        self.merge(self.root, restored, '1.0.0', 0)
        self.assertEqual(original, tree_hashes(self.root / 'v1.0.0/r0'))

    def test_html_parser_collects_anchors(self):
        page = Page('<h2 id="example">Example</h2><a href="#example">link</a>')
        self.assertIn('example', page.ids)
        self.assertIn(('#example', 'a'), page.links)

    def git(self, *args):
        return subprocess.check_output(['git', *map(str, args)], stderr=subprocess.DEVNULL, text=True).strip()

    def test_real_git_publish_retry_preserves_newer_tree_and_rollback(self):
        remote = self.base / 'remote.git'
        self.git('init', '--bare', remote)
        fake_root = self.base / 'workspace'
        fake_root.mkdir()
        first = self.site('1.0.0', 0)
        newer = self.site('2.0.0', 0)
        with patch.object(release, 'ROOT', fake_root):
            release.deploy_tree(first, '1.0.0', 0, remote_url=str(remote), activate=True)
            original_run = subprocess.run
            injected = []
            def competing_push(args, **kwargs):
                if isinstance(args, list) and 'push' in args and not injected:
                    injected.append(True)
                    release.deploy_tree(newer, '2.0.0', 0, remote_url=str(remote), activate=True)
                return original_run(args, **kwargs)
            with patch.object(subprocess, 'run', side_effect=competing_push):
                exported = release.deploy_tree(self.site('1.0.0', 1), '1.0.0', 1, remote_url=str(remote), activate=True)
            tree = Path(exported['export'])
            self.assertTrue((tree / 'v2.0.0/r0/index.html').exists())
            self.assertTrue((tree / 'v1.0.0/r1/index.html').exists())
            self.assertFalse((tree / '.git').exists())
            result = release.deploy_tree(first, '1.0.0', 0, rollback=0, remote_url=str(remote), activate=True)
            tree = Path(result['export'])
            self.assertEqual(0, json.loads((tree / 'v1.0.0/current.json').read_text())['revision'])
            self.assertTrue((tree / 'v1.0.0/r1/index.html').exists())
            # A failed Pages deployment can replay the same archive without dropping later versions.
            result = release.deploy_tree(first, '1.0.0', 0, remote_url=str(remote), activate=True)
            self.assertTrue((Path(result['export']) / 'v2.0.0/r0/index.html').exists())

    def test_remote_failure_does_not_initialize_archive(self):
        fake_root = self.base / 'workspace'
        fake_root.mkdir()
        with patch.object(release, 'ROOT', fake_root):
            with self.assertRaises(subprocess.CalledProcessError):
                release.deploy_tree(self.site('1.0.0', 0), '1.0.0', 0, remote_url=str(self.base / 'does-not-exist.git'))
        self.assertFalse(list((fake_root / 'artifacts/pages').glob('export-*')))

    def test_preflight_blocks_tag_native_and_commit_mismatch(self):
        source = self.base / 'source'
        (source / 'native').mkdir(parents=True)
        (source / 'native/Cargo.toml').write_text('[package]\nname="native"\nversion="1.0.0"\n')
        (source / 'native/Cargo.lock').write_text('[[package]]\nname="native"\nversion="1.0.0"\n')
        def answer(*args):
            if args[1] == 'status': return ''
            return 'a' * 40
        with patch.object(release, 'ROOT', source), patch.object(release, 'version', return_value='1.0.0'), patch.object(release, 'output', side_effect=answer):
            self.assertEqual('1.0.0', release.local_check('v1.0.0', remote=False)['version'])
            with self.assertRaisesRegex(ValueError, 'managed version'):
                release.local_check('v2.0.0', remote=False)
            (source / 'native/Cargo.lock').write_text('[[package]]\nname="native"\nversion="0.9.0"\n')
            with self.assertRaisesRegex(ValueError, 'Cargo.lock'):
                release.local_check('v1.0.0', remote=False)

    def test_translation_missing_and_drift_are_errors(self):
        source = self.base / 'source'
        (source / 'docs').mkdir(parents=True)
        (source / 'README.md').write_text('[Chinese](README.zh-CN.md)\n', encoding='utf-8')
        (source / 'README.zh-CN.md').write_text('[English](README.md)\n', encoding='utf-8')
        (source / 'index.md').write_text('Home\n', encoding='utf-8')
        with patch.object(documentation, 'ROOT', source):
            with self.assertRaisesRegex(ValueError, 'needs review'):
                documentation.check_sources()
            documentation.check_sources(['README.md'])
            (source / 'README.md').write_text('[Chinese](README.zh-CN.md)\nChanged\n', encoding='utf-8')
            with self.assertRaisesRegex(ValueError, 'needs review'):
                documentation.check_sources()
            (source / 'README.zh-CN.md').unlink()
            with self.assertRaisesRegex(ValueError, 'Missing translation'):
                documentation.check_sources()

    def test_final_site_missing_anchors_and_absolute_paths(self):
        site = self.base / 'site'
        for name in ['index.html', 'docs/index.html', 'docs/zh-CN/index.html', 'api/Fizzy.McapSharp.html', 'index.json']:
            path = site / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('<h1 id="ok">Title</h1>' if name.endswith('.html') else '{}')
        documentation.check_site(site)
        (site / 'index.html').write_text('<a href="docs/index.html#missing">Broken</a><a href="/api/missing.html">Root</a>')
        with self.assertRaisesRegex(ValueError, 'missing anchor'):
            documentation.check_site(site)

    def test_candidate_assets_are_never_overwritten(self):
        folder = self.base / 'bundle'
        folder.mkdir()
        (folder / 'docs.zip').write_bytes(b'original')
        record = {'assets': [{'name': 'docs.zip', 'id': 10}]}
        with patch.object(release, 'release_record', return_value=record), patch.object(release, 'gh', return_value=b'original') as calls:
            release.persist('v1.0.0', folder)
            self.assertFalse(any('upload' in call.args for call in calls.call_args_list))
        with patch.object(release, 'release_record', return_value=record), patch.object(release, 'gh', return_value=b'conflict'):
            with self.assertRaisesRegex(ValueError, 'conflicts'):
                release.persist('v1.0.0', folder)

    def bundle(self):
        """Synthetic byte fixtures test provenance, never native loading or real packaging."""
        folder = self.base / 'candidate'
        folder.mkdir()
        commit = 'a' * 40
        site = self.site('1.0.0', 0)
        package = folder / 'Fizzy.McapSharp.1.0.0.nupkg'
        native = {}
        with zipfile.ZipFile(package, 'w') as z:
            z.writestr('Fizzy.McapSharp.nuspec', '<package><metadata><id>Fizzy.McapSharp</id><version>1.0.0</version></metadata></package>')
            z.writestr('lib/net8.0/Fizzy.McapSharp.xml', '<doc/>')
            for rid, (target, filename) in release.TARGETS.items():
                payload = ('test fixture ' + rid).encode()
                z.writestr(f'runtimes/{rid}/native/{filename}', payload)
                native[rid] = dict(commit=commit, source_sha256='source', rid=rid, target=target, sha256=release.digest(payload))
        with zipfile.ZipFile(folder / 'docs.zip', 'w') as z:
            for name in tree_hashes(site):
                z.write(site / name, name)
        write_json(folder / 'validation-jobs.json', {'commit': commit, 'run_id': '123', 'jobs': [{'id': 1, 'name': 'validate / verified', 'conclusion': 'success'}]})
        with zipfile.ZipFile(folder / 'validation.zip', 'w') as z:
            z.writestr('fixture.json', '{"status":"passed","scope":"synthetic test"}')
        manifest = dict(schema=1, version='1.0.0', tag='v1.0.0', commit=commit, source_sha256='source', run_id='123',
                        native=native, files=tree_hashes(site), assets=tree_hashes(folder))
        write_json(folder / 'provenance.json', manifest)
        return folder, manifest

    def test_bundle_native_commit_and_asset_tampering_blocked(self):
        folder, manifest = self.bundle()
        release.verify_bundle(folder, 'v1.0.0')
        manifest['native']['linux-arm64']['commit'] = 'b' * 40
        write_json(folder / 'provenance.json', manifest)
        with self.assertRaisesRegex(ValueError, 'Native provenance'):
            release.verify_bundle(folder, 'v1.0.0')
        with self.assertRaisesRegex(ValueError, 'tag mismatch'):
            release.verify_bundle(folder, 'v2.0.0')

    def test_durable_restore_needs_no_actions_artifact(self):
        folder, manifest = self.bundle()
        saved = {p.name: p.read_bytes() for p in folder.iterdir()}
        assets = [{'name': name, 'id': i} for i, name in enumerate(saved)]
        by_id = {str(a['id']): saved[a['name']] for a in assets}
        destination = self.base / 'restored-candidate'
        def fake_gh(*args, binary=False):
            route = str(args[-1])
            if '/releases/assets/' in route:
                return by_id[route.rsplit('/', 1)[1]]
            if '/jobs?' in route:
                return json.dumps([{'jobs': [{'name': 'validate / verified', 'conclusion': 'success'}]}])
            raise AssertionError(f'Unexpected network call: {args}')
        with patch.object(release, 'BUNDLE', destination), patch.object(release, 'release_record', return_value={'assets': assets}), patch.object(release, 'remote_commit', return_value=manifest['commit']), patch.object(release, 'gh', side_effect=fake_gh):
            restored = release.restore('v1.0.0')
            self.assertEqual(manifest, restored)
            self.assertEqual(saved, {p.name: p.read_bytes() for p in destination.iterdir()})

    def test_single_asset_candidate_restores_without_loose_assets(self):
        folder, manifest = self.bundle()
        archive = release.create_candidate_bundle(folder)
        saved = archive.read_bytes()
        release.create_candidate_bundle(folder)
        self.assertEqual(saved, archive.read_bytes())
        destination = self.base / 'restored-single'
        with patch.object(release, 'BUNDLE', destination), patch.object(release, 'release_record', return_value={'assets': [{'name': 'release-bundle.zip', 'id': 8}]}), patch.object(release, 'remote_commit', return_value=manifest['commit']), patch.object(release, 'gh', return_value=saved) as calls:
            self.assertEqual(manifest, release.restore('v1.0.0'))
            self.assertEqual(1, calls.call_count)
            self.assertEqual(manifest, release.verify_bundle(destination))

    def test_revision_single_bundle_restores_original_package_identity(self):
        folder, manifest = self.bundle()
        revised = self.site('1.0.0', 1, 'revision')
        docs = io.BytesIO()
        with zipfile.ZipFile(docs, 'w') as archive:
            for name in tree_hashes(revised):
                archive.writestr(name, (revised / name).read_bytes())
        metadata = {'version': '1.0.0', 'revision': 1, 'release_commit': manifest['commit'], 'docs_commit': manifest['commit'],
                    'archive_sha256': release.digest(docs.getvalue()), 'package_sha256': release.digest((folder / 'Fizzy.McapSharp.1.0.0.nupkg').read_bytes()), 'files': tree_hashes(revised)}
        bundle = io.BytesIO()
        with zipfile.ZipFile(bundle, 'w') as archive:
            archive.writestr('docs-r1.zip', docs.getvalue())
            archive.writestr('provenance-r1.json', json.dumps(metadata))
        workspace = self.base / 'workspace'
        workspace.mkdir()
        record = {'assets': [{'name': 'revision-r1.zip', 'id': 9}]}
        with patch.object(release, 'ROOT', workspace), patch.object(release, 'BUNDLE', folder), patch.object(release, 'restore', return_value=manifest), patch.object(release, 'release_record', return_value=record), patch.object(release, 'gh', return_value=bundle.getvalue()):
            result = release.recover_site('v1.0.0', 1)
            self.assertEqual(tree_hashes(revised), tree_hashes(Path(result['recovered']) / 'v1.0.0/r1'))
            self.assertFalse((Path(result['recovered']) / 'v1.0.0/current.json').exists())

    def test_revision_inventory_reserves_partial_and_complete_identities(self):
        record = {'draft': False, 'assets': [{'name': 'docs-r1.zip'}, {'name': 'revision-r2.zip'}, {'name': 'provenance-r3.json'}]}
        with patch.object(revision, 'release_record', return_value=record):
            self.assertEqual({1, 2, 3}, revision.inventory('v1.0.0')[1])
        record['draft'] = True
        with patch.object(revision, 'release_record', return_value=record):
            with self.assertRaisesRegex(ValueError, 'public release'):
                revision.inventory('v1.0.0')

    def test_prepare_starts_from_verified_active_revision_commit(self):
        recovered = self.base / 'recovered'
        info = {'version': '1.0.0', 'revision': 1, 'docs_commit': 'b' * 40, 'release_commit': 'a' * 40}
        write_json(recovered / 'v1.0.0/r1/doc-info.json', info)
        def local_output(*args):
            return '' if args[1] == 'status' else 'a' * 40
        with patch.object(sys, 'argv', ['revision.py', 'prepare', '--tag', 'v1.0.0']), patch.object(revision, 'inventory', return_value=({'assets': []}, {1})), patch.object(revision, 'restore', return_value={'commit': 'a' * 40}), patch.object(revision, 'output', side_effect=local_output), patch.object(revision, 'run') as git, patch.object(subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)), patch.object(revision, 'recover_site', return_value={'recovered': str(recovered)}), patch.object(revision, 'verify_online') as online, patch('urllib.request.urlopen', return_value=io.BytesIO(b'{"revision":1}')):
            result = revision.main()
            self.assertEqual(2, result['revision'])
            self.assertEqual('b' * 40, result['base_commit'])
            self.assertEqual(('git', 'switch', '-c', 'docs/v1.0.0-r2', 'b' * 40), git.call_args.args)
            online.assert_called_once_with('1.0.0', 1, expected_commit='b' * 40)

    def test_revision_resume_dispatches_restore_without_rebuild(self):
        with patch.object(sys, 'argv', ['revision.py', 'resume', '--tag', 'v1.0.0', '--revision', '1']), patch.object(revision, 'inventory', return_value=({'assets': [{'name': 'revision-r1.zip'}]}, {1})), patch.object(revision, 'restore'), patch.object(revision, 'gh') as dispatch, patch.object(revision, 'revision_build') as build:
            self.assertEqual('resume', revision.main()['dispatched'])
            self.assertIn('docs-restore.yml', dispatch.call_args.args)
            self.assertIn('activate_revision=true', dispatch.call_args.args)
            build.assert_not_called()

    def test_real_git_stage_failure_keeps_latest_and_replay_preserves_newer_revision(self):
        remote = self.base / 'stage.git'
        self.git('init', '--bare', remote)
        fake_root = self.base / 'workspace'
        fake_root.mkdir()
        original = self.site('1.0.0', 0)
        updated = self.site('1.0.0', 1)
        with patch.object(release, 'ROOT', fake_root):
            release.deploy_tree(original, '1.0.0', 0, remote_url=str(remote), activate=True)
            result = release.deploy_tree(updated, '1.0.0', 1, remote_url=str(remote))
            tree = Path(result['export'])
            self.assertEqual(0, json.loads((tree / 'v1.0.0/current.json').read_text())['revision'])
            self.assertFalse((tree / 'v1.0.0/completed/r1.json').exists())
            release.deploy_tree(updated, '1.0.0', 1, remote_url=str(remote), activate=True, expected_commit='a' * 40)
            result = release.deploy_tree(original, '1.0.0', 0, remote_url=str(remote), activate=True)
            self.assertEqual(1, json.loads((Path(result['export']) / 'v1.0.0/current.json').read_text())['revision'])

    def test_partial_durable_upload_uses_original_bundle_only(self):
        folder, manifest = self.bundle()
        saved = {p.name: p.read_bytes() for p in folder.iterdir()}
        destination = self.base / 'partial-candidate'
        def fake_gh(*args, binary=False):
            if args[0:2] == ('run', 'download'):
                output = Path(args[args.index('--dir') + 1])
                for name, data in saved.items(): (output / name).write_bytes(data)
                return ''
            if '/releases/assets/' in str(args[-1]): return saved['provenance.json']
            if '/jobs?' in str(args[-1]): return json.dumps([{'jobs': [{'name': 'verified', 'conclusion': 'success'}]}])
            raise AssertionError(args)
        with patch.object(release, 'BUNDLE', destination), patch.object(release, 'release_record', return_value={'assets': [{'name': 'candidate.json', 'id': 1}]}), patch.object(release, 'remote_commit', return_value=manifest['commit']), patch.object(release, 'gh', side_effect=fake_gh):
            self.assertEqual(manifest, release.restore('v1.0.0'))


if __name__ == '__main__':
    unittest.main()

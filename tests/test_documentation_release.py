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
import docs_archive
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
        write_json(site / 'doc-info.json', dict(version=value, channel="dev" if revision is None else "release", docs_commit='a' * 40, release_commit='a' * 40))
        (site / 'index.html').write_text(text)
        return site

    def merge(self, root, site, value, revision=None):
        release.merge_site(root, site, value, revision is not None)





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
        self.assertEqual(original, tree_hashes(self.root / 'v1.0.0'))

    def test_html_parser_collects_anchors(self):
        page = Page('<h2 id="example">Example</h2><a href="#example">link</a>')
        self.assertIn('example', page.ids)
        self.assertIn(('#example', 'a'), page.links)

    def git(self, *args):
        return subprocess.check_output(['git', *map(str, args)], stderr=subprocess.DEVNULL, text=True).strip()



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

class DirectArchiveTests(unittest.TestCase):
    setUp = ReleaseTests.setUp
    site = ReleaseTests.site
    git = ReleaseTests.git

    def test_overwrite_deletes_pages_preserves_other_versions_and_latest(self):
        original = self.site('1.0.0', 0)
        (original / 'removed.html').write_text('removed')
        newer = self.site('2.0.0', 0)
        docs_archive.merge_site(self.root, original, '1.0.0', True)
        docs_archive.merge_site(self.root, newer, '2.0.0', True)
        before = tree_hashes(self.root / 'v2.0.0')
        docs_archive.merge_site(self.root, self.site('1.0.0', 1, 'corrected'), '1.0.0', True)
        self.assertFalse((self.root / 'v1.0.0/removed.html').exists())
        self.assertEqual(before, tree_hashes(self.root / 'v2.0.0'))
        self.assertEqual('corrected', (self.root / 'v1.0.0/index.html').read_text())
        self.assertEqual('2.0.0', json.loads((self.root / 'versions.json').read_text())['latest'])
        docs_archive.merge_site(self.root, self.site('3.0.0-beta.1', 0), '3.0.0-beta.1', True)
        self.assertEqual('2.0.0', json.loads((self.root / 'versions.json').read_text())['latest'])

    def test_legacy_paths_removed_without_redirect(self):
        import shutil
        old = self.site('1.0.0', 0)
        write_json(old / 'doc-info.json', dict(version='1.0.0', revision=0, docs_commit='a'*40, release_commit='a'*40))
        shutil.copytree(old, self.root / 'v1.0.0/r0')
        write_json(self.root / 'v1.0.0/current.json', {'revision': 0})
        docs_archive.migrate(self.root)
        self.assertFalse((self.root / 'v1.0.0/r0').exists())
        self.assertFalse((self.root / 'v1.0.0/current.json').exists())
        self.assertEqual('original', (self.root / 'v1.0.0/index.html').read_text())
        self.assertNotIn('revision', json.loads((self.root / 'v1.0.0/doc-info.json').read_text()))

    def test_real_git_candidate_success_staleness_resume_and_rollback(self):
        remote = self.base / 'direct.git'
        self.git('init', '--bare', remote)
        original = self.site('1.0.0', 0)
        with patch.object(docs_archive, 'check_site'):
            first = docs_archive.deploy_tree(original, '1.0.0', True, 'empty', '100', str(remote))
            self.assertFalse((Path(first['archive']) / 'v1.0.0').exists())
            recovered, candidate = docs_archive.recover_payload('100', value='1.0.0', remote_url=str(remote))
            self.assertEqual(tree_hashes(original), tree_hashes(recovered))
            self.assertEqual(first['candidate'], candidate)
            with self.assertRaisesRegex(ValueError, 'successful'):
                docs_archive.recover_payload(archive_commit=candidate, value='1.0.0', remote_url=str(remote))
            completed = docs_archive.complete_deployment(first['archive'], '100', candidate)['archive_commit']
            with self.assertRaisesRegex(ValueError, 'already completed'):
                docs_archive.recover_payload('100', value='1.0.0', remote_url=str(remote))
            with self.assertRaisesRegex(ValueError, 'changed since preparation'):
                docs_archive.deploy_tree(original, '1.0.0', True, candidate, '101', str(remote))
            changed = self.site('1.0.0', 1, 'corrected')
            second = docs_archive.deploy_tree(changed, '1.0.0', True, completed, '102', str(remote))
            self.assertEqual('original', (Path(second['archive']) / 'v1.0.0/index.html').read_text())
            # A failed candidate has no success marker and leaves durable current docs unchanged.
            self.assertFalse((Path(second['archive']) / '.deployments/102/success.json').exists())
            restored, _ = docs_archive.recover_payload(archive_commit=completed, value='1.0.0', remote_url=str(remote))
            self.assertEqual(tree_hashes(original), tree_hashes(restored))
            current = docs_archive.complete_deployment(second['archive'], '102', second['candidate'])['archive_commit']
            self.assertEqual('corrected', (Path(second['archive']) / 'v1.0.0/index.html').read_text())
            rollback = docs_archive.deploy_tree(restored, '1.0.0', True, current, '103', str(remote))
            rolled_back = docs_archive.complete_deployment(rollback['archive'], '103', rollback['candidate'])['archive_commit']
            self.assertNotEqual(completed, rolled_back)
            self.assertEqual('original', (Path(rollback['archive']) / 'v1.0.0/index.html').read_text())
            self.git('-C', rollback['archive'], 'merge-base', '--is-ancestor', current, rolled_back)

    def test_corrupt_payload_refused(self):
        remote = self.base / 'corrupt.git'
        self.git('init', '--bare', remote)
        with patch.object(docs_archive, 'check_site'):
            result = docs_archive.deploy_tree(self.site('1.0.0', 0), '1.0.0', True, 'empty', '200', str(remote))
        (Path(result['archive']) / '.deployments/200/site/index.html').write_text('tampered')
        with self.assertRaisesRegex(ValueError, 'content changed'):
            docs_archive.complete_deployment(result['archive'], '200', result['candidate'])

    def test_pending_candidate_cannot_resume_after_newer_success(self):
        remote = self.base / 'stale.git'
        self.git('init', '--bare', remote)
        with patch.object(docs_archive, 'check_site'):
            pending = docs_archive.deploy_tree(self.site('1.0.0', 0), '1.0.0', True, 'empty', '300', str(remote))
            newer = docs_archive.deploy_tree(self.site('1.0.0', 1, 'new'), '1.0.0', True, pending['candidate'], '301', str(remote))
            completed = docs_archive.complete_deployment(newer['archive'], '301', newer['candidate'])['archive_commit']
            with self.assertRaisesRegex(ValueError, 'superseded'):
                docs_archive.recover_payload('300', value='1.0.0', remote_url=str(remote))
            with self.assertRaisesRegex(ValueError, 'changed during deployment'):
                docs_archive.complete_deployment(pending['archive'], '300', pending['candidate'])
            with self.assertRaisesRegex(ValueError, 'cannot overwrite corrected'):
                docs_archive.deploy_tree(Path(pending['archive']) / '.deployments/300/site', '1.0.0', True, completed, '302', str(remote), initial=True)

    def test_pending_dev_payload_recovery_needs_no_release(self):
        remote = self.base / 'dev.git'
        self.git('init', '--bare', remote)
        with patch.object(docs_archive, 'check_site'):
            site = self.site('0.1.0', None)
            pending = docs_archive.deploy_tree(site, '0.1.0', False, run_id='400', remote_url=str(remote))
            recovered, _ = docs_archive.recover_payload('400', remote_url=str(remote))
            self.assertEqual(tree_hashes(site), tree_hashes(recovered))
            self.assertEqual('dev', docs_archive.identity(recovered)['channel'])
            self.assertFalse((Path(pending['archive']) / 'dev').exists())

    def test_online_fingerprint_rejects_same_source_stale_content(self):
        info = {'version': '1.0.0', 'channel': 'release', 'docs_commit': 'a'*40, 'content_sha256': 'old'}
        with patch('urllib.request.urlopen', return_value=io.BytesIO(json.dumps(info).encode())):
            with self.assertRaisesRegex(ValueError, 'fingerprint is stale'):
                docs_archive.verify_online('1.0.0', True, attempts=1, expected_commit='a'*40, expected_content='new')

    def test_update_rejects_sample_project_but_accepts_example_source(self):
        with patch.object(release, 'output', return_value='samples/Documentation/Documentation.csproj'):
            with self.assertRaisesRegex(ValueError, 'non-documentation'):
                release.check_update_changes('base')
        with patch.object(release, 'output', return_value='samples/Documentation/Program.cs'):
            self.assertEqual(['samples/Documentation/Program.cs'], release.check_update_changes('base'))

    def test_dev_resume_baseline_is_rechecked_at_deployment(self):
        remote = self.base / 'dev-race.git'
        self.git('init', '--bare', remote)
        with patch.object(docs_archive, 'check_site'):
            first = docs_archive.deploy_tree(self.site('0.1.0', None), '0.1.0', False, run_id='500', remote_url=str(remote))
            recovered, baseline = docs_archive.recover_payload('500', remote_url=str(remote))
            newer = docs_archive.deploy_tree(self.site('0.1.0', None, 'newer'), '0.1.0', False, run_id='501', remote_url=str(remote))
            docs_archive.complete_deployment(newer['archive'], '501', newer['candidate'])
            with self.assertRaisesRegex(ValueError, 'changed since preparation'):
                docs_archive.deploy_tree(recovered, '0.1.0', False, expected_archive=baseline, run_id='502', remote_url=str(remote))

    def test_online_fresh_identity_does_not_hide_stale_html(self):
        site = self.site('1.0.0', 0)
        def response(url, **kwargs):
            if url.endswith('doc-info.json'): return io.BytesIO((site / 'doc-info.json').read_bytes())
            return io.BytesIO(b'stale html')
        with patch('urllib.request.urlopen', side_effect=response):
            with self.assertRaisesRegex(ValueError, 'Online file differs'):
                docs_archive.verify_online_files('https://example.test/v1.0.0/', site)

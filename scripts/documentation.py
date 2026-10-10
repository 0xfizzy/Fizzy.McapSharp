"""Reproducible documentation build, translation review and isolated examples."""
import argparse
import hashlib
from html.parser import HTMLParser
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import sys
import tempfile
import urllib.parse
import webbrowser
import xml.etree.ElementTree as ET
import zipfile

from build import ROOT, build, output, run, version

DOCS = ROOT / 'artifacts/docs'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


def tree_hashes(directory):
    for path in directory.rglob('*'):
        if path.is_symlink() or path.is_junction():
            raise ValueError(f'Archive trees cannot contain links: {path}')
    return {p.relative_to(directory).as_posix(): digest(p.read_bytes())
            for p in sorted(directory.rglob('*')) if p.is_file()}


def translation_pairs():
    for path in [ROOT / 'README.md', *sorted((ROOT / 'docs').glob('*.md'))]:
        if '.zh-CN.' not in path.name:
            yield path, path.with_name('README.zh-CN.md') if path.name == 'README.md' else path.parent / 'zh-CN' / path.name


def translation_hash(path):
    return digest(path.read_text(encoding='utf-8-sig').replace('\r\n', '\n').encode())


def check_sources(accept=None):
    manifest = ROOT / 'docs/translations.json'
    hashes = json.loads(manifest.read_text(encoding='utf-8')) if manifest.exists() else {}
    pairs = list(translation_pairs())
    if accept:
        selected = {Path(x).as_posix() for x in accept}
        known = {p.relative_to(ROOT).as_posix() for p, _ in pairs}
        if selected - known:
            raise ValueError(f'Unknown English translation sources: {selected - known}')
        for source, translated in pairs:
            key = source.relative_to(ROOT).as_posix()
            if key in selected:
                if not translated.exists():
                    raise ValueError(f'Missing translation: {translated}')
                hashes[key] = translation_hash(source)
        write_json(manifest, hashes)
    errors = []
    for translated in (ROOT / 'docs/zh-CN').glob('*.md'):
        if not (ROOT / 'docs' / translated.name).exists():
            errors.append(f'Orphan translation: {translated}')
    for directory in [ROOT / 'docs', ROOT / 'docs/zh-CN']:
        guide_pages = {p.name for p in directory.glob('*.md')}
        if not guide_pages:
            continue
        toc = directory / 'toc.yml'
        linked = set(re.findall(r'^\s*href:\s*(\S+)', toc.read_text(encoding='utf-8'), re.M)) if toc.exists() else set()
        for missing in sorted(guide_pages - linked):
            errors.append(f'Guide missing from navigation: {directory.name}/{missing}')
    for source, translated in pairs:
        key = source.relative_to(ROOT).as_posix()
        if not translated.exists():
            errors.append(f'Missing translation: {translated}')
            continue
        if hashes.get(key) != translation_hash(source):
            errors.append(f'Translation needs review: {key}')
        for a, b in [(source, translated), (translated, source)]:
            if os.path.relpath(b, a.parent).replace('\\', '/') not in a.read_text(encoding='utf-8'):
                errors.append(f'Missing language switch: {a}')
    for path in [ROOT / 'README.md', ROOT / 'README.zh-CN.md', ROOT / 'index.md', *sorted((ROOT / 'docs').rglob('*.md'))]:
        if not path.exists():
            errors.append(f'Missing source page: {path}')
            continue
        body = re.sub(r'```.*?```', '', path.read_text(encoding='utf-8'), flags=re.S)
        for link in re.findall(r'\]\(([^\s)]+)(?:\s+[^)]*)?\)', body):
            parsed = urllib.parse.urlsplit(link)
            if parsed.scheme or parsed.netloc or not parsed.path:
                continue
            target = (path.parent / urllib.parse.unquote(parsed.path)).resolve()
            if target.is_relative_to(ROOT / 'api'):
                continue  # checked after DocFX metadata generation
            if not target.exists():
                errors.append(f'{path.relative_to(ROOT)}: missing {link}')
    if errors:
        raise ValueError('\n'.join(errors))
    return {'translation_pairs': len(pairs), 'source_links': 'passed'}


class Page(HTMLParser):
    def __init__(self, text):
        super().__init__(convert_charrefs=True)
        self.ids, self.links = set(), []
        self.feed(text)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if 'id' in attrs:
            self.ids.add(attrs['id'])
        for key in ('href', 'src'):
            if key in attrs:
                self.links.append((attrs[key], tag))


def check_site(site):
    pages = {p.resolve(): Page(p.read_text(encoding='utf-8')) for p in site.rglob('*.html')}
    errors = []
    for path, page in pages.items():
        for link, tag in page.links:
            parsed = urllib.parse.urlsplit(link)
            if parsed.scheme or parsed.netloc or link.startswith('/'):
                if link.startswith('/') and not link.startswith('//'):
                    errors.append(f'Root-relative URL breaks project hosting: {path.name}: {link}')
                continue
            target = (path.parent / urllib.parse.unquote(parsed.path)).resolve() if parsed.path else path
            if target.is_dir():
                target /= 'index.html'
            if not target.is_relative_to(site.resolve()) or not target.exists():
                errors.append(f'{path.relative_to(site)}: missing/escaping {link}')
            elif parsed.fragment and target in pages and urllib.parse.unquote(parsed.fragment) not in pages[target].ids:
                errors.append(f'{path.relative_to(site)}: missing anchor {link}')
    for required in ['index.html', 'docs/index.html', 'docs/zh-CN/index.html', 'api/Fizzy.McapSharp.html', 'index.json']:
        if not (site / required).is_file():
            errors.append(f'Missing required site output: {required}')
    if errors:
        raise ValueError('\n'.join(errors[:80]))
    return {'pages': len(pages), 'site_links': 'passed'}


def namespace_pages():
    directory = DOCS / 'api'
    namespaces = set()
    for file in directory.glob('*.yml'):
        for namespace in re.findall(r'^  namespace: ([\w.]+)', file.read_text(encoding='utf-8'), re.M):
            while namespace:
                namespaces.add(namespace)
                namespace = namespace.rpartition('.')[0]
    for name in sorted(namespaces):
        file = directory / (name + '.yml')
        if file.exists():
            continue
        children = sorted(n for n in namespaces if n.rpartition('.')[0] == name)
        file.write_text('### YamlMime:ManagedReference\nitems:\n- uid: ' + name + '\n  id: ' + name +
                        '\n  name: ' + name + '\n  fullName: ' + name + '\n  type: Namespace\n  children:\n' +
                        ''.join('  - ' + c + '\n' for c in children) + 'references:\n' +
                        ''.join(f'- uid: {c}\n  name: {c}\n  fullName: {c}\n  href: {c}.html\n' for c in children), encoding='utf-8')


def extract_package(package, destination):
    with zipfile.ZipFile(package) as archive:
        for name in ['Fizzy.McapSharp.dll', 'Fizzy.McapSharp.xml']:
            (destination / name).write_bytes(archive.read('lib/net8.0/' + name))
        nuspec = ET.fromstring(archive.read('Fizzy.McapSharp.nuspec'))
        if nuspec.findtext('{*}metadata/{*}id') != 'Fizzy.McapSharp' or nuspec.findtext('{*}metadata/{*}version') != version():
            raise ValueError('Documentation package identity differs from source metadata')


def build_docs(package=None, release_commit=None, xml_override=None):
    check_sources()
    DOCS.mkdir(parents=True, exist_ok=True)
    for name in ['api', 'site', 'input']:
        directory = DOCS / name
        if not directory.resolve().is_relative_to(ROOT.resolve() / 'artifacts'):
            raise ValueError(f'Generated output escapes repository artifacts: {directory}')
        if directory.is_symlink() or directory.is_junction():
            raise ValueError(f'Refusing linked output: {directory}')
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir()
    if package:
        extract_package(package, DOCS / 'input')
        if xml_override:
            original = ET.parse(DOCS / 'input/Fizzy.McapSharp.xml')
            updated = ET.parse(xml_override)
            ids = lambda tree: sorted(x.attrib['name'] for x in tree.findall('./members/member'))
            if ids(original) != ids(updated):
                raise ValueError('Updated XML member inventory differs from original package')
            shutil.copy2(xml_override, DOCS / 'input/Fizzy.McapSharp.xml')
    else:
        build()
        run('dotnet', 'build', ROOT / 'Fizzy.McapSharp.csproj', '-c', 'Release', '--no-incremental', '-p:DocumentationStrict=true')
        for name in ['Fizzy.McapSharp.dll', 'Fizzy.McapSharp.xml']:
            shutil.copy2(ROOT / 'bin/Release/net8.0' / name, DOCS / 'input' / name)
    xml = ET.parse(DOCS / 'input/Fizzy.McapSharp.xml')
    members = []
    for member in xml.findall('./members/member'):
        if len(member.findall('summary')) > 1 or (member.find('summary') is None and member.find('inheritdoc') is None):
            raise ValueError(f'Missing or duplicate XML summary: {member.attrib["name"]}')
        members.append(member.attrib['name'])
    write_json(DOCS / 'api-contracts.json', members)
    run('dotnet', 'tool', 'restore')
    write_json(DOCS / 'metadata.json', {'metadata': [{'src': [{'src': str(DOCS / 'input'), 'files': ['*.dll']}], 'dest': str(DOCS / 'api'), 'disableGitFeatures': True}]})
    run('dotnet', 'docfx', 'metadata', DOCS / 'metadata.json', '--warningsAsErrors')
    namespace_pages()
    run('dotnet', 'docfx', 'build', ROOT / 'docfx.json', '--warningsAsErrors')
    info = {'version': version(), 'channel': 'release' if package else 'dev', 'release_commit': release_commit or output('git', 'rev-parse', 'HEAD'),
            'docs_commit': output('git', 'rev-parse', 'HEAD'), 'tools': {'dotnet': output('dotnet', '--version'), 'docfx': '2.78.3'},
            'run_id': os.environ.get('GITHUB_RUN_ID', 'local')}
    site = DOCS / 'site'
    write_json(site / 'doc-info.json', info)
    shutil.copy2(ROOT / 'scripts/docsite/version.js', site / 'version.js')
    for page in site.rglob('*.html'):
        root = os.path.relpath(site, page.parent).replace('\\', '/') + '/'
        body = page.read_text(encoding='utf-8')
        page.write_text(body.replace('</body>', f'<script src="{root}version.js" data-doc-root="{root}"></script></body>'), encoding='utf-8')
    content = tree_hashes(site)
    content.pop('doc-info.json', None)
    info['content_sha256'] = digest(json.dumps(content, sort_keys=True).encode())
    write_json(site / 'doc-info.json', info)
    report = check_site(site)
    npm = 'npm.cmd' if os.name == 'nt' else 'npm'
    run(npm, 'ci', '--no-audit', '--no-fund')
    executable = output('node', '-e', "process.stdout.write(require('@playwright/test').chromium.executablePath())")
    # Reuse the exact pinned browser when present; do not contend for another installer's lock.
    if not Path(executable).is_file():
        run('node', ROOT / 'node_modules/@playwright/test/cli.js', 'install', 'chromium')
    run('node', ROOT / 'scripts/Test-Browser.mjs')
    report['browser'] = 'passed'
    write_json(DOCS / 'files.json', tree_hashes(site))
    # Stable entry timestamps keep retries with identical site bytes reproducible.
    with zipfile.ZipFile(DOCS / 'docs.zip', 'w', zipfile.ZIP_DEFLATED) as archive:
        for name in tree_hashes(site):
            entry = zipfile.ZipInfo(name, (2000, 1, 1, 0, 0, 0))
            entry.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(entry, (site / name).read_bytes())
    return dict(report, **info, archive_sha256=digest((DOCS / 'docs.zip').read_bytes()))


def samples(package_directory=None, published=False):
    folder = Path(tempfile.mkdtemp(prefix='samples-', dir=ROOT / 'artifacts'))
    for source in (ROOT / 'samples/Documentation').glob('*.cs'):
        shutil.copy2(source, folder / source.name)
    shutil.copy2(ROOT / 'samples/Documentation/Documentation.csproj', folder / 'Documentation.csproj')
    project = folder / 'Documentation.csproj'
    if package_directory and published:
        raise ValueError('Choose either a local package directory or the published source')
    if package_directory or published:
        source = 'https://api.nuget.org/v3/index.json' if published else str(package_directory.resolve())
        config = ET.Element('configuration')
        sources = ET.SubElement(config, 'packageSources')
        ET.SubElement(sources, 'clear')
        ET.SubElement(sources, 'add', key='only', value=source)
        ET.ElementTree(config).write(folder / 'NuGet.Config', encoding='utf-8')
        reference = f'<PackageReference Include="Fizzy.McapSharp" Version="{version()}" />'
    else:
        build()
        reference = f'<ProjectReference Include="{ROOT / "Fizzy.McapSharp.csproj"}" />'
    template = project.read_text(encoding='utf-8')
    if template.count('<!-- REFERENCE -->') != 1 or template.count('<!-- END REFERENCE -->') != 1:
        raise ValueError('Trusted sample project must contain exactly one reference placeholder')
    project.write_text(re.sub(r'<!-- REFERENCE -->.*?<!-- END REFERENCE -->', lambda _: reference, template, flags=re.S), encoding='utf-8')
    restore = ['dotnet', 'restore', project, '--packages', folder / 'packages']
    if package_directory or published:
        restore += ['--configfile', folder / 'NuGet.Config']
    run(*restore)
    run('dotnet', 'run', '--project', project, '-c', 'Release', '--no-restore')
    return {'samples': 'passed', 'source': 'published-package' if published else 'local-package' if package_directory else 'current-source'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['check', 'build', 'serve', 'samples', 'site'])
    parser.add_argument('--accept-translation', action='append')
    parser.add_argument('--package', type=Path)
    parser.add_argument('--package-directory', type=Path)
    parser.add_argument('--published', action='store_true')
    parser.add_argument('--release-commit')
    parser.add_argument('--xml-override', type=Path)
    parser.add_argument('--port', type=int, default=8087)
    parser.add_argument('--site-directory', type=Path, default=DOCS / 'site')
    args = parser.parse_args()
    if args.command == 'check':
        return check_sources(args.accept_translation)
    if args.command == 'build':
        return build_docs(args.package, args.release_commit, args.xml_override)
    if args.command == 'site':
        return check_site(DOCS / 'site')
    if args.command == 'samples':
        return samples(args.package_directory, args.published)
    site = args.site_directory.resolve()
    if not (site / 'index.html').exists():
        raise ValueError('Build the site first: ./scripts/Build-Docs.ps1')
    # Mount the existing single-version output at its deployed project/version prefix.
    prefix = '/Fizzy.McapSharp/dev/'
    class PreviewHandler(http.server.SimpleHTTPRequestHandler):
        def translate_path(self, request):
            path = urllib.parse.urlsplit(request).path
            if not path.startswith(prefix):
                return str(site / '__not_found__')
            relative = urllib.parse.unquote(path[len(prefix):])
            target = (site / relative).resolve()
            return str(target if target.is_relative_to(site) else site / '__not_found__')
    server = http.server.ThreadingHTTPServer(('127.0.0.1', args.port), PreviewHandler)
    print(f'Preview: http://127.0.0.1:{args.port}{prefix} (Ctrl+C to stop)', flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return {'preview': 'stopped'}


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
        write_json(ROOT / 'artifacts/reports/documentation.json', report)
        if sys.argv[1:2] and sys.argv[1] in ['build', 'check', 'samples', 'site', 'serve']:
            write_json(ROOT / f'artifacts/reports/documentation-{sys.argv[1]}.json', report)

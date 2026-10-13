"""Restore and run the complete package without source references or a shared cache."""
import argparse
import hashlib
from pathlib import Path
import uuid
import zipfile
import shutil
import json
import traceback
import xml.etree.ElementTree as ET
from build import ROOT, TARGETS, FRAMEWORKS, API_FRAMEWORK, target_path, msbuild_property, check_binary, host_rid, run, version


def validate_managed_assets(archive, frameworks=FRAMEWORKS):
    expected_managed = {f"lib/{tfm}/Fizzy.McapSharp.{ext}" for tfm in frameworks for ext in ("dll", "xml")}
    actual_managed = {name for name in archive.namelist() if name.startswith("lib/")}
    if actual_managed != expected_managed:
        raise RuntimeError(f"Unexpected managed assets: {actual_managed ^ expected_managed}")
    documents = []
    for tfm in frameworks:
        if not archive.read(f"lib/{tfm}/Fizzy.McapSharp.dll").startswith(b"MZ"):
            raise RuntimeError(f"Invalid {tfm} assembly")
        xml = ET.fromstring(archive.read(f"lib/{tfm}/Fizzy.McapSharp.xml"))
        if xml.findtext("assembly/name") != "Fizzy.McapSharp" or not xml.findall("members/member"):
            raise RuntimeError(f"Invalid {tfm} XML documentation")
        documents.append(ET.tostring(xml))
    if len(set(documents)) != 1:
        raise RuntimeError("Target XML contracts differ; shared API reference requires identical contracts")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-directory", type=Path, default=ROOT / "artifacts/packages")
    parser.add_argument("--fixtures", type=Path, help="All three platform fixture directories")
    parser.add_argument("--published", action="store_true", help="Restore only from public NuGet with an isolated cache")
    parser.add_argument("--saved-bundle", action="store_true", help="Verify original release provenance and retain its original target set")
    args = parser.parse_args()
    rid = host_rid()
    package = args.package_directory.resolve() / f"Fizzy.McapSharp.{version()}.nupkg"
    package_frameworks = FRAMEWORKS
    if args.saved_bundle:
        from release import verify_bundle
        verify_bundle(args.package_directory.resolve())
        with zipfile.ZipFile(package) as archive:
            if not any(name.startswith('lib/net10.0/') for name in archive.namelist()):
                package_frameworks = (API_FRAMEWORK,)
    smoke = ROOT / "artifacts" / ("smoke-" + uuid.uuid4().hex)
    smoke.mkdir(parents=True)
    # Isolate package-only projects from the checkout's source build settings.
    (smoke / "Directory.Build.props").write_text("""<Project>
  <PropertyGroup>
    <Configuration Condition="'$(Configuration)' == ''">Debug</Configuration>
    <BaseIntermediateOutputPath>$(MSBuildThisFileDirectory)build/obj/$(MSBuildProjectName)/</BaseIntermediateOutputPath>
    <OutputPath>$(MSBuildThisFileDirectory)build/$(Configuration)/tests/$(MSBuildProjectName)/</OutputPath>
    <DefaultItemExcludes>$(DefaultItemExcludes);$(MSBuildThisFileDirectory)build/**</DefaultItemExcludes>
  </PropertyGroup>
</Project>""", encoding="utf-8")
    with zipfile.ZipFile(package) as archive:
        metadata = ET.fromstring(archive.read('Fizzy.McapSharp.nuspec'))
        icon_name = metadata.findtext('{*}metadata/{*}icon')
        if icon_name != 'icon.png':
            raise RuntimeError('Missing package icon metadata')
        icon = archive.read(icon_name)
        if not icon.startswith(b'\x89PNG\r\n\x1a\n') or len(icon) > 1024 * 1024:
            raise RuntimeError('Invalid NuGet PNG icon')
        origin = json.loads((ROOT / 'assets/icon-source.json').read_text(encoding='utf-8'))
        if hashlib.sha256(icon).hexdigest() != origin['sha256']:
            raise RuntimeError('Package icon differs from the official MCAP asset')
        archive.read('MCAP-LICENSE.txt')
        expected_assets = {f"runtimes/{asset_rid}/native/{filename}" for asset_rid, (_, filename) in TARGETS.items()}
        actual_assets = {name for name in archive.namelist() if name.startswith("runtimes/")}
        if actual_assets != expected_assets:
            raise RuntimeError(f"Unexpected or missing runtime assets: {actual_assets ^ expected_assets}")
        validate_managed_assets(archive, package_frameworks)
        for asset_rid, (_, filename) in TARGETS.items():
            data = archive.read(f"runtimes/{asset_rid}/native/{filename}")
            check = smoke / (asset_rid + "-" + filename)
            check.write_bytes(data)
            check_binary(check, asset_rid)
        expected = archive.read(f"runtimes/{rid}/native/{TARGETS[rid][1]}")
    print("Package SHA256:", hashlib.sha256(package.read_bytes()).hexdigest())
    (smoke / "NuGet.Config").write_text('<configuration><packageSources><clear/><add key="local" value="' +
        ('https://api.nuget.org/v3/index.json' if args.published else str(args.package_directory.resolve())).replace("&", "&amp;").replace('"', "&quot;") + '"/></packageSources></configuration>', encoding="utf-8")
    project = smoke / "Smoke.csproj"
    project.write_text(f'''<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType>
<TargetFramework>net8.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings><NuGetAudit>false</NuGetAudit><UseAppHost>false</UseAppHost><EnableDefaultCompileItems>false</EnableDefaultCompileItems>
<SelfContained>false</SelfContained><EnableRuntimePackDownload>false</EnableRuntimePackDownload>
</PropertyGroup><ItemGroup><Compile Include="*.cs" /><PackageReference Include="Fizzy.McapSharp" Version="{version()}" /></ItemGroup></Project>''')
    (smoke / "Program.cs").write_text('''using Fizzy.McapSharp;
var framework = System.Runtime.InteropServices.RuntimeInformation.FrameworkDescription;
var assembly = typeof(McapWriter).Assembly;
var target = ((System.Runtime.Versioning.TargetFrameworkAttribute)Attribute.GetCustomAttribute(assembly, typeof(System.Runtime.Versioning.TargetFrameworkAttribute))!).FrameworkName;
if (target != "EXPECTED_ASSEMBLY_FRAMEWORK") throw new Exception("Assembly target mismatch: " + target);
Console.WriteLine($"{framework}: {target}");
foreach (var compression in Enum.GetValues<McapCompression>())
{
    var path = Path.Combine(Path.GetTempPath(), "机器人 sample-" + Guid.NewGuid() + ".mcap");
    try {
        using (var writer = new McapWriter(path, new() { Compression = compression })) {
            var channel = writer.RegisterChannel("smoke", "raw");
            writer.WriteToKnownChannel(new McapMessageHeader(channel, 1, 1, 1), [42]); writer.Complete();
        }
        var reader = new McapReaderFactory(path); reader.Validate();
        if (reader.ReadMessages().Single().Data[0] != 42) throw new Exception("Payload mismatch");
        using var session=reader.OpenMessages();var buffer=new byte[1];
        if(session.ReadNext(buffer,out var header,out var length)!=McapReadStatus.Success || length!=1 || buffer[0]!=42 || header.Sequence!=1)throw new Exception("Buffered ABI mismatch");
        using var stream=new MemoryStream();using(var writer=new McapWriter(stream,new(){Compression=compression},leaveOpen:true)){var channel=writer.RegisterChannel("stream","raw");writer.WriteToKnownChannel(new McapMessageHeader(channel,0,1,1),[7]);writer.Complete();}
        stream.Position=0;using var streamed=McapReaderFactory.OpenMessages(stream,leaveOpen:true);
        if(streamed.ReadNext(buffer,out _,out _)!=McapReadStatus.Success||buffer[0]!=7)throw new Exception("Stream ABI mismatch");
        using var leased=reader.OpenMessages();using var batch=leased.ReadBatchLease()!;
        using var forwarded=new MemoryStream();
        var original=batch.GetHeader(0);var replacement=new McapMessageHeader(65000,9,10,11);
        using(var writer=new McapWriter(forwarded,new(){Compression=compression},true)) {
            writer.RegisterChannel(original.ChannelId,"original","raw");writer.RegisterChannel(65000,"remapped","raw");
            if(writer.WriteBatch(batch)!=1||writer.WriteBatch(batch,new[]{replacement})!=1)throw new Exception("Lease batch count mismatch");
            writer.Complete();
        }
        using var copied=new McapMessageReader(forwarded.ToArray());using var roundtrip=copied.ReadBatchLease()!;
        if(roundtrip.Count!=2||roundtrip.GetHeader(0)!=original||roundtrip.GetHeader(1)!=replacement ||
            !roundtrip.GetPayload(0).SequenceEqual(batch.GetPayload(0))||!roundtrip.GetPayload(1).SequenceEqual(batch.GetPayload(0)))
            throw new Exception("Lease forwarding ABI mismatch");
    } finally { File.Delete(path); }
}
Console.WriteLine("Isolated native load and all compression roundtrips passed.");
''', encoding="utf-8")
    config = smoke / "NuGet.Config"
    template = project.read_text()
    for framework in FRAMEWORKS:
        selected_framework = framework if framework in package_frameworks else API_FRAMEWORK
        expected_target = '.NETCoreApp,Version=v' + selected_framework.removeprefix('net')
        program = smoke / 'Program.cs'
        if framework == FRAMEWORKS[0]:
            program_template = program.read_text(encoding='utf-8')
        program.write_text(program_template.replace('EXPECTED_ASSEMBLY_FRAMEWORK', expected_target), encoding='utf-8')
        project.write_text(template.replace('net8.0', framework))
        run("dotnet", "restore", project, "--configfile", config, "--packages", smoke / "packages")
        if args.published:
            from release import package_content
            restored = smoke / 'packages/fizzy.mcapsharp' / version().lower() / f'fizzy.mcapsharp.{version().lower()}.nupkg'
            if package_content(package) != package_content(restored):
                raise RuntimeError('Public package differs from candidate')
        assets_path = Path(msbuild_property(project, 'ProjectAssetsFile', framework))
        assets = json.loads(assets_path.read_text(encoding='utf-8'))
        selected = assets['targets'][framework][f'Fizzy.McapSharp/{version()}']
        for kind in ('compile', 'runtime'):
            if set(selected[kind]) != {f'lib/{selected_framework}/Fizzy.McapSharp.dll'}:
                raise RuntimeError(f'Wrong NuGet selection for {framework}: {selected[kind]}')
        run("dotnet", "run", "--project", project, "-c", "Release", "--no-restore")
        with zipfile.ZipFile(package) as archive:
            binary = target_path(project, framework).parent / 'Fizzy.McapSharp.dll'
            if binary.read_bytes() != archive.read(f'lib/{selected_framework}/Fizzy.McapSharp.dll'):
                raise RuntimeError(f'Consumer assembly differs from {framework} package asset')
        run("dotnet", "restore", project, "-r", rid, "--configfile", config, "--packages", smoke / "packages")
        published = smoke / "published" / framework
        run("dotnet", "publish", project, "-c", "Release", "-r", rid, "--self-contained", "false",
            "--no-restore", "-p:UseAppHost=false", "-o", published)
        if (published / TARGETS[rid][1]).read_bytes() != expected:
            raise RuntimeError("Published native asset does not match selected RID")
        run("dotnet", published / "Smoke.dll")

        # Compile exactly the same public-API contracts against the candidate package only.
        contract = smoke / "contracts" / framework
        contract.mkdir(parents=True)
        for source in (ROOT / 'tests/ContractRunner').glob('*.cs'):
            shutil.copy2(source, contract / source.name)
        contract_project = contract / 'ContractRunner.csproj'
        contract_project.write_text(project.read_text().replace('</PropertyGroup>', '<Nullable>enable</Nullable></PropertyGroup>'))
        run('dotnet', 'restore', contract_project, '--configfile', config, '--packages', smoke / 'packages')
        run('dotnet', 'build', contract_project, '-c', 'Release', '--no-restore')
        contract_dll = target_path(contract_project, framework)
        from test_suites import generate
        spec = contract / 'fixture.json'
        spec.write_text(json.dumps(generate(2), separators=(',', ':')), encoding='utf-8')
        for compression in ['None', 'Lz4', 'Zstd']:
            fixture = contract / (compression + '.mcap')
            run('dotnet', contract_dll, 'write', spec, fixture, compression)
            run('dotnet', contract_dll, 'check', fixture, spec)
        if args.fixtures:
            run('dotnet', contract_dll, 'exchange', args.fixtures.resolve())
    return dict(rid=rid, source='published-package' if args.published else 'local-package', package=str(package), sha256=hashlib.sha256(package.read_bytes()).hexdigest(),
                frameworks=list(FRAMEWORKS), package_frameworks=list(package_frameworks), fixture_directory=str(args.fixtures) if args.fixtures else None)


if __name__ == "__main__":
    report = {'status': 'running'}
    try:
        report.update(main())
        report['status'] = 'passed'
    except BaseException:
        report['status'] = 'failed'
        report['error'] = traceback.format_exc()
        raise
    finally:
        (ROOT / 'artifacts').mkdir(exist_ok=True)
        (ROOT / 'artifacts/package-report.json').write_text(json.dumps(report, indent=2))

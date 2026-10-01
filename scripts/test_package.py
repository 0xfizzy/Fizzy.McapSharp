"""Restore and run the complete package without source references or a shared cache."""
import argparse
import hashlib
from pathlib import Path
import uuid
import zipfile
import shutil
import json
import traceback
from build import ROOT, TARGETS, check_binary, host_rid, run, version


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package-directory", type=Path, default=ROOT / "artifacts/packages")
    parser.add_argument("--fixtures", type=Path, help="All three platform fixture directories")
    args = parser.parse_args()
    rid = host_rid()
    package = args.package_directory.resolve() / f"Fizzy.McapSharp.{version()}.nupkg"
    smoke = ROOT / "artifacts" / ("smoke-" + uuid.uuid4().hex)
    smoke.mkdir(parents=True)
    with zipfile.ZipFile(package) as archive:
        expected_assets = {f"runtimes/{asset_rid}/native/{filename}" for asset_rid, (_, filename) in TARGETS.items()}
        actual_assets = {name for name in archive.namelist() if name.startswith("runtimes/")}
        if actual_assets != expected_assets:
            raise RuntimeError(f"Unexpected or missing runtime assets: {actual_assets ^ expected_assets}")
        if "lib/net8.0/Fizzy.McapSharp.dll" not in archive.namelist():
            raise RuntimeError("Missing net8.0 managed assembly")
        for asset_rid, (_, filename) in TARGETS.items():
            data = archive.read(f"runtimes/{asset_rid}/native/{filename}")
            check = smoke / (asset_rid + "-" + filename)
            check.write_bytes(data)
            check_binary(check, asset_rid)
        expected = archive.read(f"runtimes/{rid}/native/{TARGETS[rid][1]}")
    print("Package SHA256:", hashlib.sha256(package.read_bytes()).hexdigest())
    (smoke / "NuGet.Config").write_text('<configuration><packageSources><clear/><add key="local" value="' +
        str(args.package_directory.resolve()).replace("&", "&amp;").replace('"', "&quot;") + '"/></packageSources></configuration>', encoding="utf-8")
    project = smoke / "Smoke.csproj"
    project.write_text(f'''<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType>
<TargetFramework>net8.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings><NuGetAudit>false</NuGetAudit><UseAppHost>false</UseAppHost>
</PropertyGroup><ItemGroup><PackageReference Include="Fizzy.McapSharp" Version="{version()}" /></ItemGroup></Project>''')
    (smoke / "Program.cs").write_text('''using Fizzy.McapSharp;
foreach (var compression in Enum.GetValues<McapCompression>())
{
    var path = Path.Combine(Path.GetTempPath(), "机器人 sample-" + Guid.NewGuid() + ".mcap");
    try {
        using (var writer = new McapWriter(path, new() { Compression = compression })) {
            var channel = writer.RegisterChannel("smoke", "raw");
            writer.WriteMessage(new McapMessageHeader(channel, 1, 1, 1), [42]); writer.Complete();
        }
        var reader = new McapReader(path); reader.Validate();
        if (reader.ReadMessages().Single().Data[0] != 42) throw new Exception("Payload mismatch");
        using var session=reader.OpenMessages();var buffer=new byte[1];
        if(session.ReadNext(buffer,out var header,out var length)!=McapReadStatus.Message || length!=1 || buffer[0]!=42 || header.Sequence!=1)throw new Exception("Buffered ABI mismatch");
        using var stream=new MemoryStream();using(var writer=new McapWriter(stream,new(){Compression=compression},leaveOpen:true)){var channel=writer.RegisterChannel("stream","raw");writer.WriteMessage(new McapMessageHeader(channel,0,1,1),[7]);writer.Complete();}
        stream.Position=0;using var streamed=McapReader.OpenMessages(stream,leaveOpen:true);
        if(streamed.ReadNext(buffer,out _,out _)!=McapReadStatus.Message||buffer[0]!=7)throw new Exception("Stream ABI mismatch");
        using var leased=reader.OpenMessages();using var batch=leased.ReadBatchLease()!;
        using var forwarded=new MemoryStream();
        var original=batch.GetHeader(0);var replacement=new McapMessageHeader(65000,9,10,11);
        using(var writer=new McapWriter(forwarded,new(){Compression=compression},true)) {
            writer.RegisterChannel(original.ChannelId,"original","raw");writer.RegisterChannel(65000,"remapped","raw");
            if(writer.WriteBatch(batch)!=1||writer.WriteBatch(batch,new[]{replacement})!=1)throw new Exception("Lease batch count mismatch");
            writer.Complete();
        }
        using var copied=new McapBufferReader(forwarded.ToArray());using var roundtrip=copied.ReadBatchLease()!;
        if(roundtrip.Count!=2||roundtrip.GetHeader(0)!=original||roundtrip.GetHeader(1)!=replacement ||
            !roundtrip.GetPayload(0).SequenceEqual(batch.GetPayload(0))||!roundtrip.GetPayload(1).SequenceEqual(batch.GetPayload(0)))
            throw new Exception("Lease forwarding ABI mismatch");
    } finally { File.Delete(path); }
}
Console.WriteLine("Isolated native load and all compression roundtrips passed.");
''', encoding="utf-8")
    config = smoke / "NuGet.Config"
    run("dotnet", "restore", project, "--configfile", config, "--packages", smoke / "packages")
    run("dotnet", "run", "--project", project, "-c", "Release", "--no-restore")
    run("dotnet", "restore", project, "-r", rid, "--configfile", config, "--packages", smoke / "packages")
    published = smoke / "published"
    run("dotnet", "publish", project, "-c", "Release", "-r", rid, "--self-contained", "false",
        "--no-restore", "-p:UseAppHost=false", "-o", published)
    if (published / TARGETS[rid][1]).read_bytes() != expected:
        raise RuntimeError("Published native asset does not match selected RID")
    run("dotnet", published / "Smoke.dll")

    # Compile exactly the same public-API contracts against the candidate package only.
    contract = smoke / "contracts"
    contract.mkdir()
    for source in (ROOT / 'tests/ContractRunner').glob('*.cs'):
        shutil.copy2(source, contract / source.name)
    contract_project = contract / 'ContractRunner.csproj'
    contract_project.write_text(project.read_text().replace('</PropertyGroup>', '<Nullable>enable</Nullable></PropertyGroup>'))
    run('dotnet', 'restore', contract_project, '--configfile', config, '--packages', smoke / 'packages')
    run('dotnet', 'build', contract_project, '-c', 'Release', '--no-restore')
    contract_dll = contract / 'bin/Release/net8.0/ContractRunner.dll'
    from test_suites import generate
    spec = contract / 'fixture.json'
    spec.write_text(json.dumps(generate(2), separators=(',', ':')), encoding='utf-8')
    for compression in ['None', 'Lz4', 'Zstd']:
        fixture = contract / (compression + '.mcap')
        run('dotnet', contract_dll, 'write', spec, fixture, compression)
        run('dotnet', contract_dll, 'check', fixture, spec)
    if args.fixtures:
        run('dotnet', contract_dll, 'exchange', args.fixtures.resolve())
    return dict(rid=rid, package=str(package), sha256=hashlib.sha256(package.read_bytes()).hexdigest(),
                fixture_directory=str(args.fixtures) if args.fixtures else None)


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

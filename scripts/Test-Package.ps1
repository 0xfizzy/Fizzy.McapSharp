param([string]$PackageDirectory = (Join-Path $PSScriptRoot '../artifacts/packages'))
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$smoke = Join-Path $repo ('artifacts/smoke-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $smoke | Out-Null
@'
<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net8.0</TargetFramework><ImplicitUsings>enable</ImplicitUsings></PropertyGroup><ItemGroup><PackageReference Include="Fizzy.McapSharp" Version="0.1.0" /></ItemGroup></Project>
'@ | Set-Content (Join-Path $smoke 'Smoke.csproj')
@'
using Fizzy.McapSharp;
var path=Path.Combine(Path.GetTempPath(),Guid.NewGuid()+".mcap");
try {
    using(var writer=new McapWriter(path)){var channel=writer.RegisterChannel("smoke","raw");writer.WriteMessage(channel,1,1,1,[42]);writer.Complete();}
    var reader=new McapReader(path);reader.Validate();
    if(reader.ReadMessages().Single().Data[0]!=42)throw new Exception("Payload mismatch");
    Console.WriteLine("Isolated NuGet restore, native load and roundtrip passed.");
} finally { File.Delete(path); }
'@ | Set-Content (Join-Path $smoke 'Program.cs')
dotnet restore (Join-Path $smoke 'Smoke.csproj') --source ([IO.Path]::GetFullPath($PackageDirectory)) --packages (Join-Path $smoke 'packages')
if ($LASTEXITCODE) { throw 'Isolated restore failed' }
dotnet run --project (Join-Path $smoke 'Smoke.csproj') -c Release --no-restore
if ($LASTEXITCODE) { throw 'Package smoke test failed' }

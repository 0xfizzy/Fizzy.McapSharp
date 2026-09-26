param([switch]$Pack, [switch]$Test)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
if (Test-Path "$repo/.tools/cargo/bin/cargo.exe") {
    $env:CARGO_HOME = "$repo/.tools/cargo"
    $env:RUSTUP_HOME = "$repo/.tools/rustup"
    $cargo = "$repo/.tools/cargo/bin/cargo.exe"
} else { $cargo = 'cargo' }
& $cargo build --release --locked --manifest-path "$repo/native/Cargo.toml"
if ($LASTEXITCODE) { throw 'Native build failed' }
dotnet build "$repo/src/Fizzy.McapSharp/Fizzy.McapSharp.csproj" -c Release
if ($LASTEXITCODE) { throw 'Managed build failed' }
if ($Test) {
    dotnet test "$repo/tests/Fizzy.McapSharp.Tests/Fizzy.McapSharp.Tests.csproj" -c Release
    if ($LASTEXITCODE) { throw 'Tests failed' }
}
if ($Pack) {
    dotnet pack "$repo/src/Fizzy.McapSharp/Fizzy.McapSharp.csproj" -c Release -o "$repo/artifacts/packages"
    if ($LASTEXITCODE) { throw 'Pack failed' }
}

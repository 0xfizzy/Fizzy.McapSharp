# Building, testing, and consumer integration

English | [简体中文](development.zh-CN.md)

## Environment and layout

Run the PowerShell commands below from the repository root. Requirements: Windows x64, .NET 8 SDK, Rust 1.98.1, and Visual C++ MSVC build tools with the Windows SDK. See [rust-toolchain.toml](../rust-toolchain.toml) for the toolchain; `native/Cargo.lock` pins the full dependency graph.

| Path | Responsibility |
| --- | --- |
| `Fizzy.McapSharp.csproj` | Library project and package metadata; compiles only `src/` |
| `src/` | Public .NET API, P/Invoke, and SafeHandle |
| `native/src/lib.rs` | Rust MCAP wrapper, private C ABI, file mapping, and validation |
| `tests/Fizzy.McapSharp.Tests/` | Managed functionality and resource lifetime tests |
| `tests/Interop/`, `tests/interop.py` | Bidirectional interoperability with official Python MCAP |
| `scripts/` | Build, packaging, and isolated package tests |
| `.github/workflows/` | CI build and manual publishing |

`Build.ps1` prefers `.tools/cargo/bin/cargo.exe` and sets the corresponding CARGO_HOME/RUSTUP_HOME when present; otherwise it uses Cargo from PATH. It does not install tools. `.tools/` is ignored; do not commit local tool installations.

## Build and test

```powershell
./scripts/Build.ps1 -Test -Pack
./scripts/Test-Package.ps1
```

`Build.ps1` always runs `cargo build --release --locked` before building the managed Release project. `-Test` runs xUnit; `-Pack` writes NuGet packages to `artifacts/packages/`. Without switches, it only builds. The managed project copies `native/target/release/fizzy_mcap_native.dll`. The script does not pass an explicit `--target`; run in a Windows x64 MSVC environment and do not mix outputs from different targets.

`Test-Package.ps1` creates an independent application and package cache under `artifacts/smoke-<GUID>/`, restores the package version configured in the script from the specified directory, and checks native loading and a message round trip. Override the package location with `-PackageDirectory`. This tests a local package, not the package published on NuGet.org.

Existing xUnit coverage includes all three compression modes, time and topic filtering, metadata, attachments, unindexed reading, incomplete files, CRC corruption, recovery results, terminal writer failure, overwrite prevention, enumerator disposal, and declarations without messages. Tests use temporary files and require no devices.

### Python interoperability

Run in an environment with Python available; CI uses Python 3.12:

```powershell
python -m pip install mcap==1.3.1 lz4==4.4.5 zstandard==0.25.0
$interopDirectory = Join-Path 'artifacts' ('interop-' + [Guid]::NewGuid().ToString('N'))
dotnet run --project tests/Interop -c Release -- write $interopDirectory
python tests/interop.py $interopDirectory
dotnet run --project tests/Interop -c Release -- read $interopDirectory
```

Keep this order: .NET writes files, Python validates them and generates files, then .NET validates the Python output. Checks cover None, Lz4, Zstd, queries, metadata, and attachments. Use a fresh directory each time because the writer refuses to overwrite existing files. `Build.ps1 -Test` does not run interoperability checks.

## Package contents

The package ID is `Fizzy.McapSharp`, targeting net8.0. It includes the managed assembly, README, third-party notices, and `runtimes/win-x64/native/fizzy_mcap_native.dll`. The project's `RequireNativeForPackage` target prevents packing when the DLL is missing. It checks existence only, so build the native DLL from the current source before packing.

Packages are written to `artifacts/packages/`; local scripts do not publish. The project declares the package version, but does not prove availability on NuGet.org. Maintainer publishing rules are in [AGENTS.md](../AGENTS.md#publishing).

## Consumer integration

Use `PackageReference` by default. Consumer MSBuild conditions select source references for local development:

- With `UseFizzyMcapSharpSource=true`, select a `ProjectReference` to this repository's project, using `FizzyMcapSharpRoot` as the root path.
- Otherwise select `PackageReference`. Never select both for the same project.
- These properties are consumer integration conventions; this library does not switch consumer references itself.

For source mode, run this repository's `./scripts/Build.ps1` before restoring and building consumers. The managed project propagates the existing native DLL to source consumer outputs; `dotnet build` alone does not compile Rust. Use the current checkout without fetching or switching branches. Restore after switching Source/Package mode, and serialize builds sharing outputs. Do not commit local paths or mode configuration.

In the combined workspace, API or behavior changes also require RobotController and Parallax Source builds and relevant tests; use the parent workspace README for entry points. Standalone use does not require those consumers. Distinguish source, local package, and published package validation in reports.

# Building, testing, and consumer integration

English | [简体中文](development.zh-CN.md)

## Environment and layout

Run commands from the repository root. Requirements: Python 3.12, .NET 8 SDK, Rust 1.98.1, and a native compiler. Windows x64 uses Visual C++ MSVC build tools with the Windows SDK. Linux x64/ARM64 uses Ubuntu 22.04, GCC/build-essential and binutils; install the Rust target for the host listed below. No local VM, WSL, Docker or cross compiler is required for Windows development. See [rust-toolchain.toml](../rust-toolchain.toml) for the toolchain; `native/Cargo.lock` pins the full dependency graph.

| Path | Responsibility |
| --- | --- |
| `Fizzy.McapSharp.csproj` | Library project and package metadata; compiles only `src/` |
| `src/` | Public .NET API, P/Invoke, and SafeHandle |
| `native/src/lib.rs` | Rust MCAP wrapper, private C ABI, file mapping, and validation |
| `tests/Fizzy.McapSharp.Tests/` | Managed functionality and resource lifetime tests |
| `tests/Interop/`, `tests/interop.py` | Bidirectional interoperability with official Python MCAP |
| `scripts/` | Build, packaging, and isolated package tests |
| `.github/workflows/` | CI build and manual publishing |

`build.py` (called by `Build.ps1`) prefers `.tools/cargo/bin/cargo.exe` and sets the corresponding CARGO_HOME/RUSTUP_HOME when present; otherwise it uses Cargo from PATH. It does not install tools. `.tools/` is ignored; do not commit local tool installations.

## Build and test

```powershell
./scripts/Build.ps1 -Test
# After collecting all three native assets from the same source:
./scripts/Build.ps1 -Test -Pack
./scripts/Test-Package.ps1
```

Linux uses `python scripts/build.py build --test`. Use `python scripts/build.py pack` to pack previously collected assets without rebuilding, and `python scripts/test_package.py` to test the package. CI installs Python 3.12; on Linux use an environment where `python` resolves to Python 3.

| RID | Rust target | Native build runner |
| --- | --- | --- |
| `win-x64` | `x86_64-pc-windows-msvc` | `windows-2022` |
| `linux-x64` | `x86_64-unknown-linux-gnu` | `ubuntu-22.04` |
| `linux-arm64` | `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` |

The build script runs `cargo build --release --locked --target <target>` before managed Release builds. Native outputs are isolated in `native/target/<target>/release/` and staged in `artifacts/native/<rid>/` with a manifest recording the commit, source fingerprint, target and binary hash. `-Test` runs build/packaging regression checks and xUnit; the default builds only the host platform. No script installs tools.

Linux builds audit ELF architecture, dynamic dependencies and required GLIBC versions (at most 2.35), and reject external compression libraries and embedded search paths. CPU-specific Rust flags are rejected. Linux ARM64 builds and tests run on native ARM64 runners. macOS, musl, Ubuntu 20.04, Windows ARM64, 32-bit, NativeAOT and single-file publishing are outside the validation matrix.

`Test-Package.ps1` calls `test_package.py`, which creates an independent application and package cache under `artifacts/smoke-<GUID>/`. It obtains the version from project metadata, verifies all packaged native architectures, restores only from the specified package directory, and tests every compression mode both with ordinary execution and RID-specific framework-dependent publishing. Override the package location with `-PackageDirectory` or `--package-directory`. This tests a local package, not NuGet.org availability.

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

The package ID is `Fizzy.McapSharp`, targeting net8.0. One package includes the managed assembly, README, third-party notices, and these assets:

```text
runtimes/win-x64/native/fizzy_mcap_native.dll
runtimes/linux-x64/native/libfizzy_mcap_native.so
runtimes/linux-arm64/native/libfizzy_mcap_native.so
```

`RequireNativeForPackage` rejects missing assets or manifests, mismatched architecture, binary hashes, commits or source fingerprints. Local `-Pack` also requires all three assets. Download each `native-<rid>` artifact from a single matching CI run into `artifacts/native/<rid>/`, retaining its manifest. Do not mix assets across runs or edit the source after collecting them. Source fingerprints normalize text line endings across Windows and Linux. Packages are written to `artifacts/packages/`; scripts do not publish or change versions.

### CI and release validation

The reusable validation workflow builds and runs xUnit plus Python interoperability on all three native runners, then packs once. Five package-test jobs run on Windows 2022 and Ubuntu 22.04/24.04 x64/ARM64. They restore the complete package into isolated caches, exercise ordinary and RID-published execution, and read fixtures produced by every build platform. Missing fixtures fail validation. Ubuntu is the tested distribution; other glibc distributions also need compatible system libraries and .NET 8.

PRs, pushes to `main`, and manual dispatch run this workflow. Superseded builds on the same PR/ref are cancelled. Intermediate artifacts expire after one day; only packages passing every job are retained for seven days. Cargo caches are separated by OS, architecture, toolchain, lockfile and native source. Standard hosted runners are used; no paid larger runners or additional cache quota are configured. Public-repository runner time is free under GitHub's current rules; storage remains subject to account allowances.

The manual publish workflow runs the same validation and passes the identical verified package to its protected publishing job without repacking. Maintainer publishing rules are in [AGENTS.md](../AGENTS.md#publishing). Passing CI is not verification of a published NuGet.org package.

## Consumer integration

Use `PackageReference` by default. Consumer MSBuild conditions select source references for local development:

- With `UseFizzyMcapSharpSource=true`, select a `ProjectReference` to this repository's project, using `FizzyMcapSharpRoot` as the root path.
- Otherwise select `PackageReference`. Never select both for the same project.
- These properties are consumer integration conventions; this library does not switch consumer references itself.

For source mode, run this repository's `./scripts/Build.ps1` before restoring and building consumers. The managed project uses an explicit `RuntimeIdentifier` when supplied, otherwise the SDK host RID, and propagates only that native library to source consumer outputs; `dotnet build` alone does not compile Rust. Use the current checkout without fetching or switching branches. Restore after switching Source/Package mode, and serialize builds sharing outputs. Do not commit local paths or mode configuration.

In the combined workspace, API or behavior changes also require RobotController and Parallax Source builds and relevant tests; use the parent workspace README for entry points. Standalone use does not require those consumers. Distinguish source, local package, and published package validation in reports.

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

### Allocation acceptance

`Build.ps1 -Test` also runs `dotnet run --project tests/Allocations -c Release`. The gate warms the writer/reader before measuring `GC.GetAllocatedBytesForCurrentThread`, excludes setup and reporting, and requires exactly zero managed bytes in the message loop; this is a mandatory hot-path contract for the library. It covers None/Lz4/Zstd, native file I/O, actual FileStream, and seekable/non-seekable span streams, with chunk boundaries, multiple channels, late declarations, empty/large messages, insufficient-buffer retries, repeated EOF and queries. The executable reports throughput and average per-message elapsed time; these are workload measurements, not latency percentiles or universal performance guarantees. Functional tests additionally cover retry correctness and callback failures.

Use Release only. Caller buffer growth, initialization, description/summary snapshots, errors and owned-record convenience APIs are outside this gate. User Stream implementations may allocate; bridge-only tests use a preallocated span-based stream whose array fallback throws. Native allocations require a separate native profiler. Keep baseline measurements in ignored artifacts, not in API documentation.


Classification enumeration acceptance compares 32 and 4096 unrelated fixed-size messages with a fixed set of target records under all three compression modes. It measures the complete enumeration and permits at most 4096 additional managed bytes, rejecting per-record allocation growth without requiring owned results to allocate zero bytes.

Completion tests distinguish ordinary flushing from explicit file synchronization, cover retained handle ownership and terminal synchronization failures, and use test-only native fault injection and FileStream overrides. These tests verify dispatch and failure contracts, not power-loss durability. ABI changes require rebuilding all three native assets from the same source before complete-package validation.

### Python interoperability

Run in an environment with Python available; CI uses Python 3.12:

```powershell
python -m pip install mcap==1.3.1 lz4==4.4.5 zstandard==0.25.0
$interopDirectory = Join-Path 'artifacts' ('interop-' + [Guid]::NewGuid().ToString('N'))
dotnet run --project tests/Interop -c Release -- write $interopDirectory
python tests/interop.py $interopDirectory
dotnet run --project tests/Interop -c Release -- read $interopDirectory
```

Keep this order: .NET writes files, Python validates them and generates files, then .NET validates the Python output. Checks cover None, Lz4, Zstd, queries, metadata, and attachments. Use a fresh directory each time because the writer's path overload only creates new files. `Build.ps1 -Test` does not run interoperability checks.

## Package contents

The package ID is `Fizzy.McapSharp`, targeting net8.0. One package includes the managed assembly, README, third-party notices, and these assets:

```text
runtimes/win-x64/native/fizzy_mcap_native.dll
runtimes/linux-x64/native/libfizzy_mcap_native.so
runtimes/linux-arm64/native/libfizzy_mcap_native.so
```

`RequireNativeForPackage` rejects missing assets or manifests, mismatched architecture, binary hashes, commits or source fingerprints. Local `-Pack` also requires all three assets. Download each `native-<rid>` artifact from a single matching CI run into `artifacts/native/<rid>/`, retaining its manifest. Do not mix assets across runs or edit the source after collecting them. Source fingerprints normalize text line endings across Windows and Linux. Packages are written to `artifacts/packages/`; scripts do not publish or change versions.

### CI and release validation

The `validate / verified` job is the final gate: it requires every enabled job to succeed, including deep checks for a release. Configure branch protection to require that check. A cancelled or unexpectedly skipped job cannot produce a verified package.

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


### Official API and extended allocation gates

`Build.ps1 -Test` also runs locked Release native differential tests and `scripts/check_api_coverage.py`. The reviewed [coverage inventory](coverage.md) is checked against the exact Cargo source, including optional Tokio public declarations. It does not replace behavior tests. The existing locked binrw version is also a direct dependency for encoding owned upstream records; no dependency versions are upgraded.

Extended allocation tests cover prepared complete-message writes (including late declarations), prepared control records, private records, attachments, record views, direct buffer readers and random index/metadata/attachment reads under every compression mode. Asynchronous tests force suspension using a reusable source and a dedicated I/O thread, summing caller and worker allocation counts, and also exercise direct awaiting with inline completion. Initialization, caller growth, owned convenience objects and errors remain excluded. Native snapshot memory is intentionally outside the managed-allocation contract.

Native differential tests compare all six slice modes against the locked upstream implementation under every compression mode. Lazy-reader checks assert no construction-time advancement and at most one pending record; these state/capacity checks exclude the input copy and do not measure total allocator or decompressor memory. Managed tests cover independent Chunk lifetimes, deferred errors, caller indexes and buffered-sort rejection. The Release allocation gate also measures independent lazy Chunk advancement.

### External contract suites

These suites are for maintainers checking interoperability and failure handling. Build the host native Release library first. Use Python dependencies from the interoperability section and Node 24 for conformance. Each invocation requires a fresh output directory; preserve failed runs for diagnosis.

```powershell
python scripts/test_suites.py conformance --output artifacts/check-conformance
python scripts/test_abi.py
python scripts/test_suites.py differential --no-build --seed 1 --samples 32 --output artifacts/check-differential
python scripts/test_suites.py robustness --no-build --seed 1 --samples 32 --output artifacts/check-robustness
python scripts/test_suites.py stress --no-build --budget 10 --output artifacts/check-stress
```

The first command builds the managed contract runner. `--no-build` reuses it; neither option builds Rust. Conformance downloads the commit pinned in `tests/conformance-lock.json`, verifies the archive and each Git LFS object's SHA-256, and preserves the upstream MIT license. Node imports the official expected-result and Rust support rules directly; no upstream multi-language build or npm installation is required. The inventory requires 416 sequential reads, 32 indexed reads and 208 exact-byte writes. The other 384 indexed cases lack prerequisites required by the official Rust runner; 208 padded writer cases cannot be emitted by the upstream writer. Unsupported cases carry explicit reasons. Count drift, missing data and supported-case failures fail the suite.

Differential testing uses a version-independent xorshift32 generator, 32 fixed seeds, three compression settings and both .NET/Python producers. Each file contains at most 256 messages and is limited to 8 MiB. Tests compare payloads, declarations, metadata and attachments, sequential/buffer/async paths, indexed order and random access. Equal-time ordering across chunks is unspecified: sorted results must be monotonic and contain exactly the expected tied messages. Odd seeds disable chunks; even seeds exercise compression and indexes. Managed xUnit tests also cover every truncation of a small recording, extreme fields, length limits, short reads, injected I/O failures and session isolation. Existing async cancellation and ownership checks remain part of the host gate.

The robustness runner mutates valid input and launches each parser probe in a separate process with a 30-second timeout. Expected MCAP errors are accepted; unexpected exceptions, panics, crashes and timeouts fail. Strict parsing uses an 8 MiB record-length limit. `--samples` selects bounded cases; a positive `--budget` instead runs until that many seconds have elapsed. `report.json` records configuration, commit, RID, runtime versions and current input. Replay with the recorded seed and configuration into a new output directory. The report's original command must have its output directory changed before rerunning.

`test_abi.py` checks all managed P/Invoke export names and uses isolated child processes with an injected missing-library resolver and a test-only incompatible Rust library. The stub stays under ignored artifacts and is never packaged.

### Weekly and manual deep checks

PRs and main pushes run the fixed suites on all three native platforms. Every Sunday at 02:00 UTC, the build workflow additionally runs Linux x64 native mutation (20 minutes), Valgrind (10 minutes), lifecycle stress (10 minutes), and an actual file exceeding 4 GiB. Manual dispatch accepts `deep`, `seed` and mutation `budget` (1–1800 seconds). Publishing requires these deep checks against the same commit and publishes the same candidate package. Scheduled runs use the workflow run number as a recorded rotating seed. No daily job is configured. Hosted schedules may start later than their nominal time.

On Linux x64 with the pinned Rust toolchain, built native asset, .NET and Valgrind installed:

```sh
python scripts/test_deep.py --seed 1 --budget 1200 --valgrind-budget 600 --stress-budget 600 --output artifacts/deep-check
```

The standalone Rust driver links the real private C ABI, checks layouts, and exercises record/chunk parsing, pending buffer reads, snapshots and indexed operations with valid handles. Native mutations run in bounded child processes with a 1 GiB address-space limit and 30-second timeout. Crashes preserve the original input and attempt bounded delta reduction; the reduced input is not guaranteed globally minimal. Valgrind rejects illegal accesses and definite/indirect leaks. This is mutation-based testing, not coverage-guided fuzzing or proof of memory safety.

Lifecycle stress samples private memory and handles after GC, discards the first third of samples, and compares middle/tail medians when at least 30 samples exist. Growth above 32 handles or 256 MiB fails for investigation; smaller growth remains diagnostic, and native allocator caches can retain memory. The large-file test writes 4097 MiB of uncompressed payload with bounded buffers, validates the file and queries its tail through indexes, then removes the file. Ensure at least 6 GiB of free disk space. This does not require snapshot APIs to use constant memory.

Reports are retained for seven days and failed inputs for fourteen days. Successful stress files are not uploaded. Allocation remains a strict 0 B gate; throughput has no hosted-runner pass threshold. The deep job has a 90-minute timeout. Inspect the failing suite report and reproduce its input before changing expectations.

Package jobs restore only the candidate nupkg into isolated caches. In addition to ordinary/RID-published smoke tests, they compile the shared public contract runner against the package and read all 18 exchanged platform fixtures with `python scripts/test_package.py --fixtures artifacts/exchanged`. They do not download source native assets or run a source ProjectReference for those checks. Package validation still requires all three matching native assets; host-only builds cannot substitute for it.


### Native memory acceptance

Release native tests include a test-only thread-local Rust allocation counter and a fixed 4096-message, 1 KiB payload workload under None/Lz4/Zstd (`memory_read_baseline`). It counts allocation/reallocation calls and cumulative requested bytes during advancement, excluding setup and input-copy construction; upstream Rust allocations and codec allocations routed through budgeted callbacks are included; direct foreign allocations are not. Thread-local counters exclude worker threads, while process-wide live/peak counters include them. Run the test with `--nocapture` and save output under ignored artifacts for before/after comparison. The gate rejects a return to per-message allocation and independently requires no wrapper delivery-buffer allocation with an adequate destination. Controlled capacity peaks/copy counters complement this measurement; they do not replace OS working-set profiling.

`SharedPendingTests` checks exact delivery-copy increments and unchanged statistics across retries, record/message switching including empty payloads, borrowed/owned/lease transfer, asynchronous record retries, and short indexed Stream reads with overlapping chunks and distinct retained payloads. It also verifies scratch-limit refusal, truncated input failure, and final storage release. Native ownership tests independently check Pending pins and transfer/refusal cleanup.

Memory tests cover capacity boundaries, retry reuse, retained-buffer release, mapped child lifetimes, raw trailing-byte preservation, snapshot position restoration, sorting descriptors/large payloads and async direct delivery. The managed Release gate also measures mapped cursor reads and statistics queries at exactly 0 B. Linux deep/Valgrind checks and all three RID assets remain required for cross-platform release confidence.

Native probes also exercise 8192-message writers with seekable/buffered output, enabled/disabled indexes and finite/unlimited chunks, repeated random seeks, fallback arenas and overlapping indexed chunks. Cached-seek tests require hits not to advance the parser. Managed gates include mapped buffer readers, cached random reads and reusable random-record scratch.

For diagnostic process-memory samples, run `dotnet run --project tests/Allocations -c Release -- memory-profile 32768 > artifacts/memory-profile.jsonl`. The count must be at least 8192. Each writer configuration samples every 8192 messages and after completion/disposal, recording Private Bytes, working set, managed heap size and elapsed time separately. Output goes to a counting sink, excluding recorded-file storage. These are process samples, not exact native live-byte or peak measurements; allocator caches and prior scenarios can affect later samples. The unbounded-chunk scenarios still obey the finite storage-block limit: larger counts can intentionally fail when the buffered chunk exceeds 64 MiB. Keep measurements in ignored artifacts.

Prepared Chunk indexes and synchronous owned delivery are covered by `DeliveryOptimizationTests`. The Release convenience gate checks final payload arrays plus result overhead and requires 0 B managed allocation for prepared large-index calls. `prepared_index_cached_calls_allocate_nothing` separately requires zero Rust allocations during warmed prepared cache hits under every compression mode; descriptor creation and foreign-library allocations are excluded. Retry tests check retained capacities and exact output-copy increments.

## Maintaining the vendored storage patch

native/vendor/mcap contains pinned mcap 0.25.0 source and the official MIT license. Cargo patch selects it; retain Cargo.lock and build with --locked. UPSTREAM.json records original file SHA-256 values and license provenance. PATCHES.json records reviewed local changes/additions. Review changes before updating patch fingerprints; never regenerate upstream hashes to conceal a patch. `python scripts/check_vendor.py` verifies file inventory, fingerprints and licensing before native builds. Cross-platform source fingerprints include vendor files.

Build.ps1 -Test includes the BatchGate borrowed/ReadBatch/WriteBatch zero-managed-allocation gate and a 10,000-batch retained-storage test. Full memory acceptance additionally requires the large-payload/boundary matrix, codec C allocations and process private bytes, and native runner evidence on all three platforms. Domain counters are not a replacement for allocator measurement. Python interoperability, consumer Source checks and identical-source three-RID packaging remain separate checks described above.

Codec maintenance must audit the pinned C custom-allocation paths: Zstd context/workspace allocation, zstdmt buffer/context pools and common/pool.c thread-pool heap allocation; Lz4 frame contexts, temporary input/output buffers and stream state. OS thread stacks and runtime resources are outside these callbacks. The private adapters use the public advanced creation interfaces and never inspect codec layouts. Preserve frame parameters and validate None/Lz4/Zstd interoperability after changes. Vendor unit tests exercise allocator refusal/overflow/rollback and radix-page sorting, failed growth and one million descriptors; managed tests verify codec release and shared-domain eviction. Detailed-statistics queries join the Release zero-allocation gate.

Audit allocation coverage and codec failure handling using the [native allocation inventory](memory-accounting.md).

The vendored MCAP integration tests accept `MCAP_CONFORMANCE_ROOT` pointing to the `tests/conformance` directory in the corpus revision pinned by `tests/conformance-lock.json`. This redirects only fixture paths; it does not skip tests or replace expected records. Run `cargo test --release --locked --manifest-path native/vendor/mcap/Cargo.toml --tests` with the repository toolchain and patched codec source.

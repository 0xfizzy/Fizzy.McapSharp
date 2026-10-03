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

### Single-file writer memory diagnostics

Run `dotnet run --project tests/Allocations -c Release -- memory-profile 65536` and `cargo test --manifest-path native/Cargo.toml --release --locked writer_summary_profile -- --nocapture`, redirecting reports under ignored `artifacts/`. The managed profile uses a counting Stream rather than retaining output, compares None/Lz4/Zstd, seekable/non-seekable output, 1/4/16 MiB chunk targets and frequent Flush, and disables each index family separately. It samples writing, completion, a held GetSummary result, result collection and disposal; it reports private bytes, working set, managed heap, output length, elapsed time and final chunk count. Increase the message count to study longer recordings. Sampling and forced GC affect timing; process samples across configurations are not isolated allocator measurements.

The native fixture compares identical 64-chunk recordings with eager summary JSON and the production on-demand path. It samples tracked live Rust bytes at 16/32/48/64 chunks, asserts lower completion retention and peak, checks response equivalence and reclamation, and reports cumulative allocations. Peak means tracked live requested allocation sizes on the test thread, not process RSS or allocator-internal realloc overlap. Codec allocations outside the Rust allocator and allocator caches are excluded. The fixture enforces storage properties, not fixed byte counts or throughput thresholds. Managed temporary-file tests verify format, indexes and summary cursor lifetime. Upstream chunk-index growth remains; these diagnostics do not establish constant-memory recording.

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

The package ID is `Fizzy.McapSharp`, targeting net8.0. One package includes the managed assembly, README, official MCAP icon with its MIT license, third-party notices, and these assets:

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

Native differential tests compare all six slice modes against the patched implementation under every compression mode. Lazy-reader checks assert no construction-time advancement and at most one pending record; these state/capacity checks exclude the input copy and do not measure total allocator or decompressor memory. Managed tests cover independent Chunk lifetimes, deferred errors, caller indexes and buffered-sort rejection. The Release allocation gate also measures independent lazy Chunk advancement.

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

Malformed-input mutation failures use one attribution policy for every compression mode. On an eligible FFI failure, a separately compiled, unmodified registry `mcap` probe reads the identical input with equivalent parser options, the same 30-second deadline and 1 GiB address-space limit. Both probes report the read mode, delivered-record count and rolling digest of preceding records; valid fixtures must produce matching progress traces before mutation testing begins. Attribution requires matching configuration, last read stage and failure evidence: a timeout without panic/abort diagnostics, an allocation abort with the same requested bytes and exit code, or an ABI-caught panic with the same source filename and complete first diagnostic line as the reference panic. Matching exit codes or signals alone never qualify. A panic escaping the ABI and test-driver assertions remain failures.

The reference currently covers top-level and decompressed record reads (buffer modes 0 and 2). Failures in other modes, snapshots, setup or delivery have no automatic waiver; unsupported operations, missing references, mismatched outcomes and absent evidence fail closed. This is finite diagnostic evidence of upstream behavior, not proof that arbitrary crashes share a root cause. It does not modify runtime behavior or make malformed input safe; use process isolation when a hard deadline is required.

Reports count completed mutations (including expected parse errors), attributed upstream failures, binding failures and unclassified failures separately. Every failing subprocess case preserves the input by SHA-256 and a JSON evidence file containing both process logs, commands, limits, progress and exit outcomes. The release gate accepts only attributed upstream cases in this mutation suite. Valid-input interoperability, ABI/layout assertions, ownership/lifetime and buffer contracts, retries, concurrency, the zero-managed-allocation gate, Valgrind and package integrity remain unconditional gates.

The standalone Rust driver links the real private C ABI, checks layouts, and exercises record/chunk parsing, pending buffer reads, snapshots and indexed operations with valid handles. Native mutations run in bounded child processes with a 1 GiB address-space limit and 30-second timeout. Mutation failures preserve the original input without reducing away attribution evidence. Valgrind rejects illegal accesses and definite/indirect leaks. This is mutation-based testing, not coverage-guided fuzzing or proof of memory safety.

Lifecycle stress samples private memory and handles after GC, discards the first third of samples, and compares middle/tail medians when at least 30 samples exist. Growth above 32 handles or 256 MiB fails for investigation; smaller growth remains diagnostic, and native allocator caches can retain memory. The large-file test writes 4097 MiB of uncompressed payload with bounded buffers, validates the file and queries its tail through indexes, then removes the file. Ensure at least 6 GiB of free disk space. This does not require snapshot APIs to use constant memory.

Reports are retained for seven days and failed inputs for fourteen days. Successful stress files are not uploaded. Allocation remains a strict 0 B gate; throughput has no hosted-runner pass threshold. The deep job has a 90-minute timeout. Inspect the failing suite report and reproduce its input before changing expectations.

Package jobs restore only the candidate nupkg into isolated caches. In addition to ordinary/RID-published smoke tests, they compile the shared public contract runner against the package and read all 18 exchanged platform fixtures with `python scripts/test_package.py --fixtures artifacts/exchanged`. They do not download source native assets or run a source ProjectReference for those checks. Package validation still requires all three matching native assets; host-only builds cannot substitute for it.


### Native storage and patch validation

| Contract under review | Evidence | What it does not establish |
| --- | --- | --- |
| Warmed managed allocation | Release allocation gates; async lease gate compares inline and suspended result-object baselines | Zero native allocation, zero payload copying or an RSS bound |
| Shared storage lifetime and delivery | Pointer identity, retry, disposal and eviction tests | Small retained backing merely because the payload is small |
| Local collection/cache allowances | Limit boundary tests and deduplicated backing diagnostics | A process-wide heap limit; diagnostic thresholds are workload-specific |
| Random-access reuse | Chunk-load/hit counts for batch, cache and cursor paths | No repeated decompression for arbitrary requests across cache misses |

Release native tests check pointer identity, retained lease lifetime and delivery allocations with a test-only thread-local allocator counter. The counter is absent from production builds and is not a total native-memory limit or codec allocation statistic.

Input reservation tests compare complete-request reservation with 64 KiB incremental reservation on 1/8/32 MiB records. Managed tests exercise short reads, retries and strict validation under all compression modes. Lease storage diagnostics advance 512 batches with fixed 1/4/16-batch retention windows, deduplicate backing allocations by address, and compare middle/tail retained capacities. Copied-input capacity and mapped address space are reported separately. These are fixture-specific retention checks, not a process RSS bound; allocator counters cover the measured thread's Rust allocations, excluding codec allocations through other allocators. Pointer tests cover retained messages after reader/snapshot disposal and actual cache eviction. Lease batch writing additionally verifies that uncompressed output receives the original payload addresses and that both header overloads allocate zero managed bytes after warm-up.

The default allocation runner includes the async lease gate. For focused runs:

```powershell
dotnet run --project tests/Allocations -c Release -- lease-gate
dotnet run --project tests/Allocations -c Release -- lease-profile
cargo test --manifest-path native/Cargo.toml --release --locked memory_probe -- --nocapture
```

`lease-gate` compares fixed one-message batch counts with inline completion and forced suspension at 64 KiB, 4 KiB and 512-byte I/O quanta. It sums caller/worker allocations and requires the same allocation total as the inline result-object baseline. A separate direct-await exercise checks continuation reentry on the I/O thread. `lease-profile` reports the same measurements without enforcing allocation equality, including throughput and median/p95 batch latency. Native diagnostics also report allocation counts/bytes and known retained storage. Keep reports under ignored artifacts; diagnostic timing includes instrumentation and has no performance pass threshold.

Focused storage-policy diagnostics run in the native Release test executable:

```powershell
cargo test --manifest-path native/Cargo.toml --release --locked sort_compaction_profile -- --nocapture
cargo test --manifest-path native/Cargo.toml --release --locked random_access_profile -- --nocapture
```

The sort fixture compares identical dense/sparse selections with shared and adaptive storage. It reports deduplicated retained backing capacity, exact copied payload bytes, peak old-plus-new capacity within a compacted group, cumulative test-thread allocations, time until results are ready and throughput. The group overlap is not a process-wide peak; active parser storage and previously completed groups are separate. Source preparation is included in timing/allocation counts; compression and RSS are not measured by this fixture. Threshold tests cover both sides of the four-times ratio and 256 KiB saving rule. Managed fallback tests cover compression, short reads, mapped input, oversized/empty messages, ties, retries and post-disposal leases.

The random-access fixture reads four chunks repeatedly under None/Lz4/Zstd, with cache disabled, a cache smaller than the working set, and a cache that holds the working set. It asserts chunk-load/hit counts and reports cache allowance charge, deduplicated payload backing capacity, input capacity, allocations and median/p95 lookup latency. Input capacity can also appear in retained backing capacity for uncompressed chunks; do not add them twice. Chunk-cursor traversal reads the same messages and reports its own allocations and elapsed time; it includes caller-buffer delivery copies, whereas cache lookup measures shared message delivery. These measurements include test instrumentation, exclude setup indexes/summary and do not measure codec allocations made outside the Rust allocator. Timing has no CI pass threshold; cache defaults remain unchanged.

`python scripts/test_upstream.py` compiles unmodified registry mcap and the local patched version separately, comparing write bytes, sequential/indexed reads, truncations and corrupt input in independent processes. Both use the same pinned dependency versions; the reference project has its own Cargo.lock and does not inherit the patch. Tests using the same patched source establish internal consistency only.

`python scripts/check_vendor.py` verifies the original file inventory and local patch hashes. Keep UPSTREAM.json unchanged; review differences before updating PATCHES.json. See [local patches](patches.md) for allowed scope, alternatives and validation requirements. Do not submit patches upstream.

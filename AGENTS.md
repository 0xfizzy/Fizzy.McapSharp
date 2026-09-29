# Repository guidelines

## Before making changes

- Read [README.md](README.md), applicable parent workspace rules, and the relevant documents in `docs/`. This repository has its own Git history; check the path, branch, status, and diff before working, and preserve unrelated changes.
- Keep changes within the task scope. Consumer repositories, parent workspace scripts, and local tool configuration are outside this repository.
- Commits, pushes, version changes, and publishing require user authorization. Do not fetch or switch checkouts just to enable source integration.

## Implementation boundaries

- Keep this library focused on general MCAP file operations. Do not depend on LibRobot, UI frameworks, devices, or business payload protocols. Consumers define payload encoding and clock semantics.
- Use the official Rust `mcap` crate through the private C ABI and .NET `SafeHandle`. Do not expose Rust layouts, borrowed memory, or native handles through public APIs.
- Keep struct layouts, calling conventions, operation codes, and ownership consistent across the ABI. Update the ABI version and managed check together for incompatible changes. Catch panics in fallible native operations; unwinding must not cross FFI.
- Serialize writer calls and consume input buffers before returning. Preserve file creation semantics: the path overload creates a new file and fails if it already exists; callers choose creation or truncation when opening a Stream. Preserve terminal failure, explicit `Complete`, and disposal without implicit completion.
- The warmed normal message hot paths are a hard zero-allocation contract: `WriteMessage(in McapMessageHeader, ReadOnlySpan<byte>)` and caller-buffer `ReadNext` must produce exactly 0 B of managed allocation, including chunk/compression boundaries, late declarations, retries and EOF. Any change to these paths requires the Release allocation gate before merge.`r`n- Each reader handle belongs to one read session (or one convenience enumerator). Release parser/iterator state before any mapping or file it borrows. Convenience records own managed copies; buffer APIs copy into caller-owned spans and do not return borrowed native memory. A successful indexed query is not full-file validation.
- Support .NET 8 / win-x64, linux-x64 and linux-arm64. Linux uses glibc with an Ubuntu 22.04 build baseline and native ARM64 runners; macOS and musl are out of scope. Pin native dependencies exactly in `native/Cargo.toml`, retain `Cargo.lock`, and build with `--locked`. Use the toolchain specified in `rust-toolchain.toml`.

## Validation and delivery

- Follow [development.md](docs/development.md). Build the native Release library before managed projects, and serialize builds that share output directories.
- For managed or native behavior changes, run `./scripts/Build.ps1 -Test -Pack` after collecting all three native assets from the same source, or the equivalent Python commands in CI. Local host-only validation uses `./scripts/Build.ps1 -Test`; report missing platform assets rather than manufacturing or bypassing them. Also run Python interoperability checks for format, compression, ABI, or read/write changes. Run `./scripts/Test-Package.ps1` for packaging or runtime asset changes.
- For API or behavior changes, follow parent workspace requirements for RobotController and Parallax Source builds and relevant tests. Package mode validates published dependencies; report incompatibilities with current source. For documentation-only changes, check links, commands, and implementation contracts without claiming unperformed tests.
- Default to NuGet. Consumer source integration uses `UseFizzyMcapSharpSource` / `FizzyMcapSharpRoot`; select exactly one reference type per project and restore after switching modes. Do not commit local paths or source configuration.
- Do not commit `.tools/`, `native/target/`, `bin/`, `obj/`, `artifacts/`, or credentials. Publish only through GitHub Actions Trusted Publishing; follow the publishing rules below.
- Tests use temporary files and require no hardware. Do not operate devices without authorization. Report actual checks, unverified areas, and limitations; a successful build does not validate UI or device behavior.

## Publishing

- Version changes, commits, pushes, and publishing require user authorization. Check the working tree and release ref, complete applicable validation, and verify the package version and all three RID native assets before manually triggering `.github/workflows/publish.yml`.
- Before publishing, configure a NuGet Trusted Publisher matching this repository, publishing workflow, and GitHub `nuget` environment. Configure the environment's protection rules and the `NUGET_USER` secret used by `NuGet/login@v1`. The workflow uses `id-token: write` for OIDC and obtains a temporary API key; do not store long-lived publishing credentials.
- `.github/workflows/build.yml` runs on pushes to main, pull requests, or manual dispatch. It calls `validate.yml` to build native and managed code on three platforms, run xUnit and bidirectional Python interoperability, pack once, validate the identical complete package on five OS/architecture combinations, and exchange platform fixtures. Confirm it passed for the release commit.
- The manual publish workflow calls the same reusable validation, then publishes its verified package without rebuilding or repacking in the publish job. Both workflows run the cross-platform `test_package.py` implementation behind `Test-Package.ps1`. Consumer Source builds, hardware/UI checks and published-package validation are not covered by these workflows.
- For an authorized version update, check the managed project, native `Cargo.toml` / `Cargo.lock`, native library identifier and metadata-driven package tests for consistency. Retain the lockfile and exact native dependency versions.
- After publication, confirm the workflow result and NuGet availability, then validate consumers using the published package source. Local nupkg test success is not published-package validation.

## Documentation

- Keep README limited to an introduction, quick start, and links. Maintain public contracts in `docs/api.md`, the ABI in `docs/native.md`, and builds, package contents, and integration in `docs/development.md`. Keep maintainer publishing rules in this file.
- AGENTS.md is English-only. English is the default for README and the guides in docs/; each has a sibling `*.zh-CN.md` translation with a language switch. Update both versions together when behavior, commands, or support changes. Keep links within the selected language where a translation exists.
- Do not hard-code the current package version in documentation or installation examples; refer to project metadata and omit the version option for normal NuGet installation.
- Identify the audience, use case, core question, and expected action before deciding on the main message and structure.
- Organize content in the order readers need to understand or act on it. Explain its purpose, cover necessary concepts, steps, and exceptions, and provide a conclusion or next step.
- Describe the current implementation and present only final conclusions, necessary supporting evidence, and required actions. Do not introduce conversation content as assumptions or body text, or include conversation summaries, migration histories, one-off incident reports, or the process of generating or revising the document.
- Make every section serve the document's overall goal. Consolidate repetition and remove irrelevant details so readers can understand and use the document without knowing how it was produced.
- For complex documents, review structure, organize content, refine language, and perform a final review in that order. Unless explicitly requested, do not make major changes to structure, scope, and tone at the same time.
- When finished, check that the main message can be stated in one sentence, and look for repetition, abrupt transitions, excessive explanation, and process narratives.

## Commit messages

- Use the `type(scope): description` format; scope is optional. Start the type, scope, and English description with lowercase letters, and mark breaking changes with `!`.
- Examples: `refactor(viewer): simplify frame ownership`, `chore: update dependencies`, `refactor(api)!: remove legacy interface`.

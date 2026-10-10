# Building and maintaining documentation

English | [简体中文](zh-CN/documentation.md)

Maintain member contracts in English XML comments, cross-API contracts in [api.md](api.md), and task sequences in [usage.md](usage.md). Review constraints, units, ownership, cancellation, failure and cleanup against implementation. XML coverage is not semantic correctness.

## Local workflow

Use Python 3.12, PowerShell 7, Node.js 22, the .NET 8 SDK and the pinned Rust toolchain with the host native compiler described in [development](development.md). DocFX is pinned in the local tool manifest; `dotnet tool restore` restores that tool only. No global tool installation is performed.

```powershell
./scripts/Test-Documentation.ps1
./scripts/Build-Docs.ps1
./scripts/Test-Samples.ps1
./scripts/Serve-Docs.ps1 -Port 8087
```

Build-Docs builds native Release before managed Release, checks XML, generates API pages and validates the final site, then runs the pinned Chromium smoke gate. Shared build directories must not be used concurrently. Serve-Docs accepts `-SiteDirectory` and serves an existing site under `/Fizzy.McapSharp/dev/` at a loopback HTTP address; Ctrl+C stops it. Do not preview using file URLs. Inspect English and Chinese guides, navigation, search and representative API pages.

For a verified complete package, `Build-Docs.ps1 -Package <absolute-nupkg-path> -ReleaseCommit <sha>` uses its DLL/XML. The package version must match project metadata. This is candidate-package documentation, not proof of published-package availability. To compare a candidate with public NuGet and execute isolated public-source package tests, run `./scripts/Test-Package.ps1 -PackageDirectory artifacts/release -Published`.

## Translation and changes

Keep English guides in `docs/` and same-name Chinese guides in `docs/zh-CN/`, with independent navigation and language links. The root README uses `README.zh-CN.md`. After reviewing the translated text, explicitly accept the English revision:

```powershell
./scripts/Test-Documentation.ps1 -AcceptTranslation docs/usage.md
```

Normalized source hashes record review state, not translation quality. Never accept hashes without review. Both languages share the same sample source and English API reference.

Public/protected API declarations, XML, guides, translations, README, navigation, templates or documentation tooling changes require the full documentation build. Internal-only changes need it only when they affect the generated reference or documented behavior. Compile and run examples when their code or relevant API changes. Run `Test-Release.ps1` when release, archive, update or restoration behavior changes. Documentation tests never replace native, allocation, interoperability or package tests.

## Outputs and diagnostics

Generated API YAML, site, docs.zip, file hashes and API member inventory live under `artifacts/docs/`. Reports live under `artifacts/reports/`; scripts exit nonzero on failure. No generated HTML belongs on the source branch. Source checks detect translation drift and missing files; final HTML checks validate local links, anchors, required search output and project-prefix-safe URLs. Chromium verifies English/Chinese/API loading, language links, one API search result, version switching, missing-page language fallback, page script errors and required resources. It writes a JSON result and saves screenshots only on failure; static checks cover the full link/anchor graph.

The docs workflow validates PRs and updates `/dev/` only from main. Under the deployment lock, the artifact commit must still equal remote main before archiving and immediately before deployment; stale builds fail explicitly. It does not publish NuGet. See the [release SOP](release.md) for package-version documentation, remote setup, updates and recovery.

# Release and recovery SOP

[简体中文](zh-CN/release.md)

## Setup and package publication

Configure NuGet Trusted Publishing for `publish.yml`, the `nuget` environment and `NUGET_USER`. Set Pages to GitHub Actions, and allow main and release tags in `github-pages`. Protect `documentation-update` with required reviewers. Keep `gh-pages` Git history; never force-push it. Actions artifacts are transport, not long-term storage.

Run `./scripts/Test-Release.ps1`, the full documentation build and affected sample gates before changing publishing tools. For an authorized package release, commit the version update, pass the reusable cross-platform validation, create the matching `v<version>` tag and use:

```powershell
./scripts/Release.ps1 Check -Tag v<version>
./scripts/Release.ps1 Start -Tag v<version>
./scripts/Release.ps1 Status -Tag v<version>
./scripts/Release.ps1 Resume -Tag v<version>
```

The publish workflow validates native assets and the identical package across supported platforms, retains an immutable original candidate in Release assets before OIDC NuGet publication, verifies published package content, then deploys its package-bound documentation. Only after successful deployment does it make the Release public. Resume reuses original package assets; it never repacks or overwrites existing assets. A partial asset upload is recovered from the original self-contained bundle. Commands do not authorize version changes, tags or publication by themselves.

## Documentation addresses and storage

`/dev/` follows current main and is explicitly unreleased. `/v<version>/` holds the latest documentation for that package; an update replaces the entire directory, including removing deleted pages. Numbered documentation paths are removed, without redirects. The version selector lists only Development and package versions. `/latest/` selects the highest successfully deployed stable semantic version; updating older documentation does not change it. The root enters latest, or dev before the first stable release.

All producers use one serialized Pages deployment workflow. Dev artifacts must still match remote main before archive assembly and immediately before deployment. Fixed updates carry the archive SHA captured at preparation; a changed archive causes rejection, rather than overwriting a later update. The payload, source SHA, run ID and content hashes are retained under `.deployments/<run-id>/` in `gh-pages`, excluded from the public Pages artifact. The candidate contains the full current site plus the selected replacement. After Pages deployment and online identity and complete file-byte verification, a success record and current tree are committed without rewriting Git history. Failed candidates are never recorded as successful. A failed build or Pages deployment leaves the previously deployed site in place; an online verification failure after a successful Pages deployment requires Resume or Rollback.

## Updating existing package documentation

```powershell
./scripts/Update-Docs.ps1 Prepare -Tag v<version>
# Edit guides, translations, examples or XML comments; review and commit.
./scripts/Update-Docs.ps1 Check -Tag v<version> -SourceRef <docs-sha>
git push -u origin HEAD
./scripts/Update-Docs.ps1 Start -Tag v<version> -SourceRef <docs-sha>
./scripts/Update-Docs.ps1 Status -Tag v<version>
./scripts/Update-Docs.ps1 Status -Tag v<version> -RunId <run-id>
```

Prepare requires a clean checkout and public Release, verifies original package provenance, fetches the successful archive and its current documentation source SHA and creates `docs/v<version>-update` from it. `-SourceRef` selects an explicit source and `-Branch` overrides the branch name. It saves the archive baseline under artifacts; no push occurs. Keep that preparation state until Start. If starting without it, Start captures the current archive SHA. Check and Start require the selected source checked out. Start dispatches trusted main tooling, renderer, browser tests and pinned tool configuration against the exact source SHA. Script, renderer, sample project and tool configuration edits are outside the update whitelist. The cloud gate supplies the trusted main sample project and copies only example C# sources into its isolated package test.

Only guides, translations, README/navigation, documentation example C# sources and XML documentation comments may differ from the original package commit. Executable C# changes, native code, package metadata and unrelated files are rejected. The original nupkg supplies the API DLL; updated XML must preserve its member IDs. Check builds the complete documentation, runs browser validation and runs examples against the original package. No package, tag or original Release asset is changed by a documentation update.

## Resume and rollback

```powershell
./scripts/Update-Docs.ps1 Resume -Tag v<version> -RunId <failed-run-id>
./scripts/Update-Docs.ps1 Rollback -Tag v<version> -ArchiveCommit <successful-archive-sha>
```

For a failed Development deployment use `Resume -Tag dev -RunId <failed-run-id>`; it still rejects an outdated main source and rechecks the recovered archive baseline under the deployment lock.

Resume loads exact durable bytes and verifies hashes; it does not rebuild. It refuses a completed run or a candidate superseded by archive changes. Failure before a durable candidate exists requires rerunning the original build on the same source SHA. Rollback requires a full archive SHA reachable from current history containing a successful deployment of the selected version, restores only that version and makes a new archive commit; other versions remain current. The archive SHA is the successful completion commit, not the candidate commit. Original Release bundles remain a separate immutable package recovery source.

Scripts fail nonzero and write reports under `artifacts/reports/`. Inspect workflow completion and live identity rather than treating dispatch acceptance as success. Offline regression tests cover replacement/deletion, retained versions, semantic latest, stale guards, exact payload recovery, success markers, rollback and original package provenance. No offline test publishes a package.

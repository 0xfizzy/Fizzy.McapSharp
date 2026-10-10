# Release and recovery SOP

English | [简体中文](zh-CN/release.md)

This SOP is for authorized maintainers. Starting with the next release, every package retains its original bilingual documentation and API reference. Existing historical packages are not reconstructed automatically. Version edits, commits, tags, pushes, settings changes and publication require authorization. Local checks do not authorize those actions.

## One-time repository setup

1. Configure NuGet Trusted Publisher for this repository, `publish.yml` and the `nuget` environment. Set `NUGET_USER`; do not store a long-lived NuGet key. Require the intended release approval on that environment.
2. Set Settings → Pages → Build and deployment to GitHub Actions. Configure `github-pages` to allow main, release tags `v*`, and explicitly approved documentation revision branches. Configure `documentation-revision` with required reviewers and permitted revision branches.
3. Protect main with the existing build and documentation checks. Protect release tags against update/deletion. Protect `gh-pages` against deletion and force pushes while allowing the deployment identity to make ordinary commits. Do not require an impossible PR-only update for that bot branch.
4. Allow workflow job permissions: read-only for builds; contents write for durable Release assets/archive commits; Pages write and OIDC only for deployment; OIDC for NuGet login. PR builds never deploy.
5. Confirm GitHub CLI authentication for local orchestration (`gh auth status`), Python 3.12, Node.js 22, .NET 8 and the pinned Rust toolchain. See [documentation maintenance](documentation.md).
6. Run the local release regression gate, preview the documentation and verify the first Pages deployment. Validation-job evidence is preserved with the candidate, so expired Actions history does not prevent long-term archive recovery. A missing archive branch is initialized only after a successful remote ref listing; authentication/network failures are never interpreted as an empty archive.

## Prepare and publish

Follow this sequence for every version:

1. Update authorized version metadata consistently in the managed project and native Cargo files, retaining exact dependency pins and the lockfile. Check native identifier handling if it changes; this library currently uses the upstream writer library identifier.
2. Update English, Chinese, XML contracts and affected examples together. Run documentation, sample and release gates. Run the existing current-source/package validation applicable to the change. Full package validation requires all three same-source native assets.
3. Commit and push the reviewed release commit to main with authorization. Wait for its normal build and documentation checks. Create and push the authorized `v<version>` tag on that commit; never move a published tag.
4. From the clean checkout at that tag, run:

```powershell
./scripts/Release.ps1 Check -Tag v<version>
./scripts/Release.ps1 Start -Tag v<version>
./scripts/Release.ps1 Status -Tag v<version>
```

Replace placeholders with the metadata version. Check verifies clean source, local/remote tag identity, version consistency, main ancestry and a successful normal build for that commit. Start repeats the check and dispatches `publish.yml` at the tag; it does not push a package locally. Dispatch acceptance is not publication success: follow the Actions run and final Status result.

The workflow runs deep validation, collects the three native assets, packs once and tests the identical package on five environments. It then generates documentation from that package's DLL/XML, tests examples and saves the candidate to a draft GitHub Release **before** NuGet push. OIDC publication uses that same nupkg. It verifies remote package content, runs isolated public-NuGet tests on three supported RIDs, stages/deploys the immutable revision and verifies its live resources, then marks it completed, activates version/latest entrypoints, redeploys and verifies those entrypoints before making the Release public. A failed fixed-URL verification never advances active pointers.

The self-contained `release-bundle.zip` contains the nupkg, docs.zip, validation.zip, validation-jobs.json and provenance.json. It is uploaded before the compatibility loose assets and can restore them without Actions artifacts. Every restored file is checked against provenance. Provenance records the tag, source commit/fingerprint, native hashes, package/archive hashes, complete site file hashes, tool versions and originating workflow. Signed NuGet packages are compared by identity and ZIP entry content except the repository signature, not by whole-package hash alone.

## Version storage and acceptance

`/dev/` follows main and is explicitly unreleased. `/v<version>/r0/` is the frozen original site; later `rN` directories are immutable revisions. `/v<version>/` selects the active revision. `/latest/` selects the highest completed stable semantic version; prereleases and older retries cannot lower it. The root enters latest, or dev before the first stable release. Version numbers in URLs come from package metadata.

The `gh-pages` branch stores the full site history. Release assets are a separate durable recovery source. Actions artifacts expire and are not the archive. All Pages producers share a serialized deployment workflow that rereads the latest archive tree, checks historical file hashes and uses non-force pushes with bounded conflict retries. Completed markers live outside immutable revision trees. Dev deployment rejects stale main artifacts before archiving and again before deploying. An older complete tree must never be copied over the current archive.

Accept a release only after Actions reports all enabled gates successful, NuGet restores the matching package from its public source, both languages and representative API pages load, search/assets work under the project prefix, the version/source banner matches and the original archive remains downloadable. Local package tests do not establish published-package validity. GitHub workflows do not validate consumer hardware/UI.

## Failure and recovery

Scripts return nonzero on failure and write JSON under `artifacts/reports/`. Preserve the workflow/run identity when diagnosing failures; do not expose credentials in logs. No recovery flag bypasses validation.

| Failure | Required action |
| --- | --- |
| Validation failed before upload | Fix the cause. Do not bypass gates. If source changes, create a new reviewed release commit/tag before publication. |
| Candidate assets partially uploaded | Resume first restores the self-contained release-bundle.zip. If it was not saved, candidate.json identifies the original release-bundle Actions artifact to finish missing uploads. Existing assets must match exactly. If that artifact has expired before durable upload completed, stop for maintainer review; never regenerate or overwrite conflicting assets. |
| Upload response failed or timed out | Query NuGet first. Matching content means continue; absent content permits another upload; conflicting content blocks release. |
| NuGet visibility deadline expired | Resume later at the same tag. Each attempt has bounded polling. |
| NuGet succeeded, package checks/Pages failed | Correct infrastructure or permissions, then Resume. The saved candidate is reused without repacking or blind duplicate push. |
| Actions artifacts expired | Resume restores the draft/public Release assets and checks provenance plus the durable original validation-job evidence. |
| Pages failure after archive commit | Resume redeploys the latest complete archive, retaining versions added since the original attempt. |
| Incorrect published package | Publish a new corrected version through the full process. Never replace package bytes under the old version. |

```powershell
./scripts/Release.ps1 Status -Tag v<version>
./scripts/Release.ps1 Resume -Tag v<version>
```

Resume verifies the saved bundle and remote tag before dispatch. The candidate.json journal binds partial uploads to the original run. Resume completes uploads from that run's bundle and verifies provenance before publication. Missing journal/input bytes or unverifiable original validation evidence blocks publication rather than manufacturing evidence. For partial uploads that cannot be resumed from their original inputs, preserve the evidence and resolve the candidate manually under maintainer review; do not claim a new build is the original artifact.

## Documentation revisions and rollback

Use the scripted facade; it never bumps versions, creates tags or publishes a package:

```powershell
./scripts/Revision-Docs.ps1 Prepare -Tag v<version>
# Edit, review and commit on the returned revision branch.
./scripts/Revision-Docs.ps1 Check -Tag v<version> -Revision <N>
# Push the reviewed branch with authorization.
./scripts/Revision-Docs.ps1 Start -Tag v<version> -Revision <N>
./scripts/Revision-Docs.ps1 Status -Tag v<version> -Revision <N>
./scripts/Revision-Docs.ps1 Resume -Tag v<version> -Revision <N>
./scripts/Revision-Docs.ps1 Rollback -Tag v<version> -Revision <previous-N>
```

Prepare requires a clean checkout, a public release and matching local/remote tag provenance, selects the next unused revision and verifies the active revision against its durable provenance and live identity, then creates a branch from that revision's documentation commit to retain earlier corrections. If that verified commit is missing locally, Prepare fetches that exact SHA from origin; executable-change checks still compare with the original package commit. `-Branch` overrides its name. No push occurs. Changes are limited to guides, README/navigation, sample code, site resources and XML documentation lines; executable C# changes (including string literals), package metadata and other files are rejected. The original package supplies the API assembly; revised XML member IDs must match the package. Check performs the full documentation/browser and original-package sample gates without uploading revision assets.

Start checks the clean committed branch matches its remote, then dispatches `docs-revision`. Approval on the documentation-revision environment remains applicable. The workflow first saves a self-contained `revision-rN.zip` containing `docs-rN.zip` and `provenance-rN.json`, followed by the loose compatibility assets, stages/deploys the immutable site, verifies the fixed URL, then activates and verifies version/latest entrypoints. Existing revision content must match exactly; old-release retries never lower an active pointer. Status distinguishes complete, partial and absent uploads and reports live state and recent workflow runs; dispatch acceptance is not successful deployment.

Resume restores durable revision bytes through trusted main and completes activation without rebuilding or lowering a newer pointer. The self-contained revision bundle also recovers a partial loose-asset upload. If failure occurred before the revision became durable, rerun the original revision workflow on the same commit; conflicting bytes remain blocked. Rollback restores no new content and points only to an existing verified revision. No revision is removed. A partial revision upload with no recoverable original artifact requires maintainer review; do not create a replacement under the same identity.

## Regression and restoration drills

Run `./scripts/Test-Release.ps1` before changes to release/archive logic. The tests use temporary directories under artifacts, fake packages and mocked services; they do not publish to NuGet. They cover multiple versions, immutable revisions, dev updates, prerelease/latest rules, conflict rejection, unsafe ZIPs, repository-signature comparison and bounded availability retries. Run the full documentation and affected sample gates after tool/content changes.

For a site-loss incident, preserve any surviving branch and Release assets first. Run Actions → docs-restore from the trusted main workflow, selecting an existing tag and the revision to activate if no pointer survives. It restores every original/revision asset for that tag, verifies file manifests, and merges into the latest archive without changing an existing active pointer. Repeat for each missing version. For a local read-only drill, run `python scripts/release.py recover-site --tag v<version> --revision 0`; the reconstructed tree is under `artifacts/recovered-archive`. Do not restore one old complete site over a newer live tree. Repository administrators remain responsible for access controls, retained Releases and remote protection settings.

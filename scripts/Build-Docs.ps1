param([string]$Package, [Nullable[int]]$Revision, [string]$ReleaseCommit)
$ErrorActionPreference = 'Stop'
$arguments = @('build')
if ($Package) { $arguments += @('--package', $Package) }
if ($null -ne $Revision) { $arguments += @('--revision', $Revision) }
if ($ReleaseCommit) { $arguments += @('--release-commit', $ReleaseCommit) }
& python (Join-Path $PSScriptRoot 'documentation.py') @arguments
if ($LASTEXITCODE) { throw 'Build-Docs.ps1 failed; see artifacts/reports' }

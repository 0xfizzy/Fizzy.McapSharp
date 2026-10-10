param([string]$Package, [string]$ReleaseCommit, [switch]$SkipBrowser)
$ErrorActionPreference = 'Stop'
$arguments = @('build')
if ($SkipBrowser) { $arguments += '--skip-browser' }
if ($Package) { $arguments += @('--package', $Package) }
if ($ReleaseCommit) { $arguments += @('--release-commit', $ReleaseCommit) }
& python (Join-Path $PSScriptRoot 'documentation.py') @arguments
if ($LASTEXITCODE) { throw 'Build-Docs.ps1 failed; see artifacts/reports' }

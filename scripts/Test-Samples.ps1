param([string]$PackageDirectory, [switch]$Published)
$ErrorActionPreference = 'Stop'
$arguments = @('samples')
if ($PackageDirectory) { $arguments += @('--package-directory', $PackageDirectory) }
if ($Published) { $arguments += '--published' }
& python (Join-Path $PSScriptRoot 'documentation.py') @arguments
if ($LASTEXITCODE) { throw 'Test-Samples.ps1 failed; see artifacts/reports' }

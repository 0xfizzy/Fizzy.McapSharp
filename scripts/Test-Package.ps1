param([string]$PackageDirectory = (Join-Path $PSScriptRoot '../artifacts/packages'), [switch]$Published, [switch]$SavedBundle)
$ErrorActionPreference = 'Stop'
$arguments = @('--package-directory', $PackageDirectory)
if ($Published) { $arguments += '--published' }
if ($SavedBundle) { $arguments += '--saved-bundle' }
& python (Join-Path $PSScriptRoot 'test_package.py') @arguments
if ($LASTEXITCODE) { throw 'Isolated package validation failed' }

param([string]$PackageDirectory = (Join-Path $PSScriptRoot '../artifacts/packages'), [switch]$Published)
$ErrorActionPreference = 'Stop'
$arguments = @('--package-directory', $PackageDirectory)
if ($Published) { $arguments += '--published' }
& python (Join-Path $PSScriptRoot 'test_package.py') @arguments
if ($LASTEXITCODE) { throw 'Isolated package validation failed' }

param([string]$PackageDirectory = (Join-Path $PSScriptRoot '../artifacts/packages'))
$ErrorActionPreference = 'Stop'
& python (Join-Path $PSScriptRoot 'test_package.py') --package-directory $PackageDirectory
if ($LASTEXITCODE) { throw 'Isolated package validation failed' }

$ErrorActionPreference = 'Stop'
Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    & python (Join-Path $PSScriptRoot 'test_release.py')
    if ($LASTEXITCODE) { throw 'Release regression tests failed' }
} finally { Pop-Location }

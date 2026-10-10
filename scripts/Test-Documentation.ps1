param([string[]]$AcceptTranslation)
$ErrorActionPreference = 'Stop'
$arguments = @('check')
foreach ($source in $AcceptTranslation) { $arguments += @('--accept-translation', $source) }
& python (Join-Path $PSScriptRoot 'documentation.py') @arguments
if ($LASTEXITCODE) { throw 'Test-Documentation.ps1 failed; see artifacts/reports' }

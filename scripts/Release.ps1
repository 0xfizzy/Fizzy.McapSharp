param([Parameter(Mandatory)][ValidateSet('Check','Start','Status','Resume')][string]$Action, [Parameter(Mandatory)][string]$Tag)
$ErrorActionPreference = 'Stop'
$arguments = @($Action.ToLowerInvariant(), '--tag', $Tag)
& python (Join-Path $PSScriptRoot 'release.py') @arguments
if ($LASTEXITCODE) { throw 'Release.ps1 failed; see artifacts/reports' }

param(
    [Parameter(Mandatory)][ValidateSet('Prepare','Check','Start','Status','Resume','Rollback')][string]$Action,
    [Parameter(Mandatory)][string]$Tag,
    [Nullable[int]]$Revision,
    [string]$Branch
)
$ErrorActionPreference = 'Stop'
$arguments = @($Action.ToLowerInvariant(), '--tag', $Tag)
if ($null -ne $Revision) { $arguments += @('--revision', $Revision) }
if ($Branch) { $arguments += @('--branch', $Branch) }
& python (Join-Path $PSScriptRoot 'revision.py') @arguments
if ($LASTEXITCODE) { throw 'Revision-Docs.ps1 failed; see artifacts/reports/revision.json' }

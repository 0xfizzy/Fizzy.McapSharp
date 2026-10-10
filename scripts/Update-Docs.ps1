param(
    [Parameter(Mandatory)][ValidateSet('Prepare','Check','Start','Status','Resume','Rollback')][string]$Action,
    [string]$Tag,
    [string]$SourceRef,
    [string]$RunId,
    [string]$ArchiveCommit,
    [string]$Branch
)
$ErrorActionPreference = 'Stop'
$arguments = @($Action.ToLowerInvariant())
if ($Tag) { $arguments += @('--tag', $Tag) }
if ($SourceRef) { $arguments += @('--source-ref', $SourceRef) }
if ($RunId) { $arguments += @('--run-id', $RunId) }
if ($ArchiveCommit) { $arguments += @('--archive-commit', $ArchiveCommit) }
if ($Branch) { $arguments += @('--branch', $Branch) }
& python (Join-Path $PSScriptRoot 'update_docs.py') @arguments
if ($LASTEXITCODE) { throw 'Update-Docs.ps1 failed; see artifacts/reports/docs-update.json' }

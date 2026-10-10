param([int]$Port = 8087, [string]$SiteDirectory = (Join-Path $PSScriptRoot '../artifacts/docs/site'))
$ErrorActionPreference = 'Stop'
$arguments = @('serve', '--port', $Port, '--site-directory', $SiteDirectory)
& python (Join-Path $PSScriptRoot 'documentation.py') @arguments
if ($LASTEXITCODE) { throw 'Serve-Docs.ps1 failed; see artifacts/reports' }

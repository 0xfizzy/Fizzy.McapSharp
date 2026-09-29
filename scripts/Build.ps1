param([switch]$Pack, [switch]$Test)
$ErrorActionPreference = 'Stop'
$arguments = @((Join-Path $PSScriptRoot 'build.py'), 'build')
if ($Test) { $arguments += '--test' }
if ($Pack) { $arguments += '--pack' }
& python @arguments
if ($LASTEXITCODE) { throw 'Build, test or complete-package validation failed' }

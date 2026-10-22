$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$ready = Join-Path $project '.local\benchmark\server.json'
if (-not (Test-Path -LiteralPath $ready)) { return }
$server = Get-Content -LiteralPath $ready -Raw | ConvertFrom-Json
$process = Get-Process -Id $server.pid -ErrorAction SilentlyContinue
$expected = Join-Path $project 'target\release\examples\echo_server.exe'
if ($process) {
    if ($process.Path -ne $expected) { throw 'PID was reused by another process; refusing to stop it' }
    Stop-Process -Id $process.Id
    Write-Host "Stopped echo server PID $($process.Id)"
}

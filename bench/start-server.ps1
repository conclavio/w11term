[CmdletBinding()]
param([ValidateRange(1024,65535)][int]$Port = 22222)
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$state = Join-Path $project '.local\benchmark'
$exe = Join-Path $project 'target\release\examples\echo_server.exe'
if (-not (Test-Path -LiteralPath $exe)) { throw 'Run .\bench\setup.ps1 first' }
New-Item -ItemType Directory -Path $state -Force | Out-Null
$ready = Join-Path $state 'server.json'
if (Test-Path -LiteralPath $ready) {
    $previous = Get-Content -LiteralPath $ready -Raw | ConvertFrom-Json
    $running = Get-Process -Id $previous.pid -ErrorAction SilentlyContinue
    if ($running -and $running.Path -eq $exe) {
        if ($previous.port -ne $Port) { throw "Echo server already runs on port $($previous.port)" }
        Write-Host "Echo server already running on 127.0.0.1:$Port (PID $($running.Id))"
        return
    }
}
$server = Start-Process -FilePath $exe -ArgumentList @('--port', $Port, '--state-dir', ('"' + $state + '"')) `
    -WorkingDirectory $project -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput (Join-Path $state 'server.stdout.log') `
    -RedirectStandardError (Join-Path $state 'server.stderr.log')
$deadline = [DateTime]::UtcNow.AddSeconds(10)
do {
    $server.Refresh()
    if ($server.HasExited) { throw "Echo server exited. Check $state\server.stderr.log" }
    if (Test-Path -LiteralPath $ready) {
        try {
            $current = Get-Content -LiteralPath $ready -Raw | ConvertFrom-Json
            if ($current.pid -eq $server.Id) {
                Write-Host "Echo server ready on 127.0.0.1:$Port (PID $($server.Id))"
                return
            }
        } catch { }
    }
    Start-Sleep -Milliseconds 100
} while ([DateTime]::UtcNow -lt $deadline)
throw 'Echo server did not become ready in 10 seconds'

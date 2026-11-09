[CmdletBinding()]
param([string]$Configuration = 'debug')
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $project "target\$Configuration\w11term.exe"
$echo = Join-Path $project "target\$Configuration\examples\echo_server.exe"
$state = Join-Path $project ('.local\profile-ui-smoke\' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $state -Force | Out-Null
$variables = @('W11TERM_UI_SMOKE','W11TERM_PROFILE_SMOKE','W11TERM_PROFILE_SMOKE_PORT','W11TERM_PROFILE_SMOKE_CONFIG','W11TERM_DATA_DIR','W11TERM_SETTINGS_PATH')
$previous = @{}
foreach ($name in $variables) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
$peer = $null
$terminal = $null
try {
    $probe = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $probe.Start()
    $port = $probe.LocalEndpoint.Port
    $probe.Stop()
    $peer = Start-Process -FilePath $echo -ArgumentList @('--port', $port, '--state-dir', ('"' + $state + '"')) -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $state 'peer.log') -RedirectStandardError (Join-Path $state 'peer.err')
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (-not (Test-Path -LiteralPath (Join-Path $state 'server.json'))) {
        if ($peer.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw 'Echo peer did not start' }
        Start-Sleep -Milliseconds 25
    }
    $server = Get-Content -LiteralPath (Join-Path $state 'server.json') -Raw | ConvertFrom-Json
    $profiles = Join-Path $state 'profiles.json'
    @{
        custom = 42
        profiles = @{
            typo = @{ host='127.0.0.1'; user='bench'; port=1; auth='identity'; identity='keys/test'; host_key=$server.host_key; player=@{rustmush=$true;remote_sounds=$false} }
            other = @{ host='127.0.0.1'; user='bench'; port=1; auth='none'; custom=99 }
        }
    } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $profiles -Encoding utf8
    $env:W11TERM_UI_SMOKE = '1'
    $env:W11TERM_PROFILE_SMOKE = '1'
    $env:W11TERM_PROFILE_SMOKE_PORT = "$port"
    $env:W11TERM_PROFILE_SMOKE_CONFIG = $profiles
    $env:W11TERM_DATA_DIR = Join-Path $state 'data'
    $env:W11TERM_SETTINGS_PATH = Join-Path $state 'appearance.json'
    $stderr = Join-Path $state 'stderr.log'
    $terminal = Start-Process -FilePath $gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError $stderr -RedirectStandardOutput (Join-Path $state 'stdout.log')
    if (-not $terminal.WaitForExit(30000)) { throw "Profile smoke timed out: $state" }
    $log = Get-Content -LiteralPath $stderr -Raw
    if ($terminal.ExitCode -ne 0 -or $log -notmatch 'Profile UI smoke passed') { throw "Profile smoke failed: $log" }
    Write-Output "Profile editor native smoke passed: $state"
} finally {
    foreach ($process in @($terminal, $peer)) { if ($null -ne $process -and -not $process.HasExited) { Stop-Process -Id $process.Id } }
    foreach ($name in $variables) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
}

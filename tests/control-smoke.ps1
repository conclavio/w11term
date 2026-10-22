[CmdletBinding()]
param([string]$Configuration = 'release')
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $project "target\$Configuration\w11term.exe"
$ctl = Join-Path $project "target\$Configuration\w11term-cli.exe"
$echo = Join-Path $project "target\$Configuration\examples\echo_server.exe"
foreach ($exe in @($gui, $ctl, $echo)) {
    if (-not (Test-Path -LiteralPath $exe)) { throw "Missing $exe. Build with cargo build --release --bins --example echo_server" }
}
$state = Join-Path $project ('.local\control-smoke\' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $state -Force | Out-Null
$probe = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
$probe.Start()
$port = $probe.LocalEndpoint.Port
$probe.Stop()
$peer = $null
$terminal = $null
function Invoke-Control([string[]]$Arguments) {
    $raw = & $ctl --pid $terminal.Id @Arguments
    $result = $raw | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or -not $result.ok) { throw "Control failed: $raw" }
    return $result.result
}
try {
    $peer = Start-Process -FilePath $echo -ArgumentList @('--port', $port, '--state-dir', ('"' + $state + '"')) -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $state 'peer.log') -RedirectStandardError (Join-Path $state 'peer.err')
    $ready = Join-Path $state 'server.json'
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (-not (Test-Path -LiteralPath $ready)) {
        if ($peer.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw 'Echo peer did not start' }
        Start-Sleep -Milliseconds 50
    }
    $info = Get-Content -LiteralPath $ready -Raw | ConvertFrom-Json
    $sshProfile = @{host='127.0.0.1'; user='bench'; port=$port; auth='none'; host_key=$info.host_key}
    $profiles = Join-Path $state 'profiles.json'
    @{profiles=@{first=$sshProfile; second=$sshProfile}} | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $profiles
    $terminal = Start-Process -FilePath $gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'first', '--tab', 'second', '--cols', '170', '--rows', '48') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $state 'gui.log') -RedirectStandardError (Join-Path $state 'gui.err')
    $manifest = Join-Path $env:LOCALAPPDATA "w11term\instances\$($terminal.Id).json"
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while (-not (Test-Path -LiteralPath $manifest)) {
        if ($terminal.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw "GUI did not start; see $state\gui.err" }
        Start-Sleep -Milliseconds 50
    }
    do {
        $listing = Invoke-Control @('list')
        $text = Invoke-Control @('get-text', '--pane', '1')
        if ($text.text.Contains('Loopback SSH echo')) { break }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $text.text.Contains('Loopback SSH echo')) { throw 'SSH pane never became ready' }
    if ($listing.sessions.Count -ne 2 -or $listing.sessions[0].active -or -not $listing.sessions[1].active) { throw 'Unexpected initial session selection' }
    foreach ($session in $listing.sessions) {
        if ($session.cols -ne 170 -or $session.rows -ne 48) { throw "Incorrect launch geometry: $($session | ConvertTo-Json -Compress)" }
    }
    $sent = Invoke-Control @('send-text', '--pane', '1', '--no-paste', '--base64', 'Qk9BUkQN')
    if ($sent.bytes_written -ne 6) { throw 'Wrong raw write byte count' }
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    do {
        $text = Invoke-Control @('get-text', '--pane', '1')
        if ($text.text.Contains('BOARD')) { break }
        Start-Sleep -Milliseconds 20
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $text.text.Contains('BOARD')) { throw 'Inactive pane did not receive input' }
    $image = Invoke-Control @('screenshot', '--pane', '1', (Join-Path $state 'inactive.png'))
    $png = [System.IO.File]::ReadAllBytes($image.path)
    if ([Convert]::ToHexString($png[0..7]) -ne '89504E470D0A1A0A' -or $image.width -le 0 -or $image.height -le 0) { throw 'Invalid PNG capture' }
    $listing = Invoke-Control @('list')
    if ($listing.sessions[0].active -or -not $listing.sessions[1].active) { throw 'Control changed tab selection' }
    $errorReply = & $ctl --pid $terminal.Id get-text --pane 999 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $errorReply.ok -or $errorReply.error -notmatch 'unknown pane') { throw 'Missing pane should fail with JSON' }
    $errorReply = & $ctl --pid 4294967295 list | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $errorReply.ok -or $errorReply.error -notmatch 'nothing was started') { throw 'Missing instance should fail without starting anything' }
    Write-Output "Control smoke passed: two SSH panes at 170x48, raw input and PNG capture on an inactive pane, selection preserved, JSON failures. Artifacts: $state"
} finally {
    if ($terminal -and -not $terminal.HasExited) {
        $null = $terminal.CloseMainWindow()
        if (-not $terminal.WaitForExit(5000)) { Stop-Process -Id $terminal.Id }
    }
    if ($peer -and -not $peer.HasExited) { Stop-Process -Id $peer.Id }
}

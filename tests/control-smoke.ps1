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
$oldDataDir = $env:W11TERM_DATA_DIR
$env:W11TERM_DATA_DIR = Join-Path $state 'data'
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
    $waited = Invoke-Control @('wait-for-text', '--pane', '1', '--regex', '--timeout', '5', 'BOARD')
    if (-not $waited.matched) { throw 'wait-for-text failed' }
    $null = Invoke-Control @('resize', '--pane', '1', '--cols', '160', '--rows', '40')
    $resized = Invoke-Control @('get-text', '--pane', '1')
    if ($resized.cols -ne 160 -or $resized.rows -ne 40) { throw 'Background resize failed' }
    $opened = Invoke-Control @('open', '--profile', 'first', '--cols', '130', '--rows', '35')
    if ($opened.active) { throw 'open selected a background tab' }
    $null = Invoke-Control @('wait-for-text', '--pane', [string]$opened.pane, '--timeout', '5', 'Loopback SSH echo')
    $listing = Invoke-Control @('list')
    if ($listing.sessions.Count -ne 3 -or -not $listing.sessions[1].active) { throw 'open changed active tab' }
    $null = Invoke-Control @('resize', '--pane', '2', '--cols', '150', '--rows', '42')
    $capture = Join-Path $state 'recording.vt'
    $null = Invoke-Control @('record', '--pane', '1', $capture)
    $null = Invoke-Control @('send-text', '--pane', '1', '--no-paste', "TAPE`r")
    $null = Invoke-Control @('wait-for-text', '--pane', '1', '--timeout', '5', 'TAPE')
    $recorded = Invoke-Control @('record', '--pane', '1', '--stop')
    if ($recorded.bytes -le 0 -or -not (Test-Path -LiteralPath $capture) -or -not (Test-Path -LiteralPath (Join-Path $state 'recording.vt.json'))) { throw 'VT recording failed' }
    $events = Invoke-Control @('events', '--pane', '1', '--since', '0')
    if ($events.journal.events.kind -notcontains 'recording-stopped') { throw 'Recording event missing' }
    $errorReply = & $ctl --pid $terminal.Id wait-for-text --pane 1 --timeout 1 'NO SUCH TEXT EXISTS' | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $errorReply.ok -or $errorReply.error -notmatch 'timed out') { throw 'wait timeout must fail with JSON' }
    $errorReply = & $ctl --pid $terminal.Id get-text --pane 999 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $errorReply.ok -or $errorReply.error -notmatch 'unknown pane') { throw 'Missing pane should fail with JSON' }
    $errorReply = & $ctl --pid 4294967295 list | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $errorReply.ok -or $errorReply.error -notmatch 'nothing was started') { throw 'Missing instance should fail without starting anything' }
    $null = $terminal.CloseMainWindow()
    if (-not $terminal.WaitForExit(5000)) { throw 'GUI failed to close' }
    $terminal = Start-Process -FilePath $gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'second') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $state 'restored.log') -RedirectStandardError (Join-Path $state 'restored.err')
    $manifest = Join-Path $env:LOCALAPPDATA "w11term\instances\$($terminal.Id).json"
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (-not (Test-Path -LiteralPath $manifest)) {
        if ($terminal.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw 'Restored GUI did not start' }
        Start-Sleep -Milliseconds 50
    }
    do {
        $listing = Invoke-Control @('list')
        if ($listing.sessions[0].cols -eq 150 -and $listing.sessions[0].rows -eq 42) { break }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($listing.sessions[0].cols -ne 150 -or $listing.sessions[0].rows -ne 42) { throw "Saved profile window was not restored: $($listing | ConvertTo-Json -Compress)" }
    Write-Output "Control smoke passed: SSH launch geometry, inactive raw input/PNG, event-driven waits, background open, active/background resize, recording/events, saved window restore, selection preserved, JSON failures. Artifacts: $state"
} finally {
    if ($terminal -and -not $terminal.HasExited) {
        $null = $terminal.CloseMainWindow()
        if (-not $terminal.WaitForExit(5000)) { Stop-Process -Id $terminal.Id }
    }
    if ($peer -and -not $peer.HasExited) { Stop-Process -Id $peer.Id }
    $env:W11TERM_DATA_DIR = $oldDataDir
}
if (-not (Test-Path -LiteralPath (Join-Path $state 'data/windows.json'))) { throw 'Window placement was not saved on native close' }

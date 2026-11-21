[CmdletBinding()]
param([string]$Configuration = 'release', [string]$GuiPath, [string]$TargetDir = $env:CARGO_TARGET_DIR)
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
if (-not $TargetDir) { $TargetDir = Join-Path $project 'target' }
$bin = Join-Path (Resolve-Path -LiteralPath $TargetDir).Path $Configuration
$gui = if ($GuiPath) { [System.IO.Path]::GetFullPath($GuiPath) } else { Join-Path $bin 'w11term.exe' }
$ctl = Join-Path $bin 'w11term-cli.exe'
$echo = Join-Path $bin 'examples\echo_server.exe'
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
$oldSettingsPath = $env:W11TERM_SETTINGS_PATH
$env:W11TERM_DATA_DIR = Join-Path $state 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $state 'settings.json'
function Invoke-Control([string[]]$Arguments) {
    $raw = & $ctl --pid $terminal.Id @Arguments
    $result = $raw | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or -not $result.ok) { throw "Control failed: $raw" }
    return $result.result
}
function Wait-ControlManifest {
    $manifest = Join-Path $env:LOCALAPPDATA "w11term\instances\$($terminal.Id).json"
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while (-not (Test-Path -LiteralPath $manifest)) {
        if ($terminal.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw "GUI did not publish its control manifest. See $state" }
        Start-Sleep -Milliseconds 50
    }
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
    $localProfile = @{backend='local'; executable='powershell.exe'; args=@('-NoProfile','-NoLogo')}
    @{profiles=@{first=$sshProfile; second=$sshProfile; 'restart-local'=$localProfile}} | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $profiles
    # Keep every native window within a small CI desktop, including the restored grid.
    $terminal = Start-Process -FilePath $gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'first', '--tab', 'second', '--cols', '80', '--rows', '24') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $state 'gui.log') -RedirectStandardError (Join-Path $state 'gui.err')
    Wait-ControlManifest
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        $listing = Invoke-Control @('list')
        $text = Invoke-Control @('get-text', '--pane', '1')
        if ($text.text.Contains('Loopback SSH echo')) { break }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $text.text.Contains('Loopback SSH echo')) { throw 'SSH pane never became ready' }
    if ($listing.sessions.Count -ne 2 -or $listing.sessions[0].active -or -not $listing.sessions[1].active) { throw 'Unexpected initial session selection' }
    if ($listing.sessions[0].visible -ne $false -or $listing.sessions[1].visible -ne $true) { throw 'Unexpected initial pane visibility' }
    if ($null -eq $listing.sessions[0].tab_id -or $null -eq $listing.sessions[1].tab_id -or $listing.sessions[0].tab_id -eq $listing.sessions[1].tab_id) { throw 'Expected two distinct containing tabs' }
    foreach ($session in $listing.sessions) {
        if ($session.cols -ne 80 -or $session.rows -ne 24) { throw "Incorrect launch geometry: $($session | ConvertTo-Json -Compress)" }
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
    $transcript = Join-Path $state 'shell transcript.txt'
    [IO.File]::WriteAllText($transcript, 'keep this file')
    $collision = & $ctl --pid $terminal.Id save-text --pane 1 $transcript | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $collision.ok -or [IO.File]::ReadAllText($transcript) -ne 'keep this file') {
        throw 'save-text replaced an existing file without --overwrite'
    }
    $saved = Invoke-Control @('save-text', '--pane', '1', $transcript, '--overwrite')
    $savedText = [IO.File]::ReadAllText($transcript, [Text.UTF8Encoding]::new($false, $true))
    if ($saved.pane -ne 1 -or $saved.path -ne $transcript -or $saved.bytes -ne (Get-Item -LiteralPath $transcript).Length -or !$savedText.Contains('BOARD')) {
        throw 'save-text did not export the addressed pane'
    }
    if ((Invoke-Control @('list')).sessions[0].active) { throw 'Saving scrollback changed the focused pane' }
    $image = Invoke-Control @('screenshot', '--pane', '1', (Join-Path $state 'inactive.png'))
    $png = [System.IO.File]::ReadAllBytes($image.path)
    if ([Convert]::ToHexString($png[0..7]) -ne '89504E470D0A1A0A' -or $image.width -le 0 -or $image.height -le 0) { throw 'Invalid PNG capture' }
    $listing = Invoke-Control @('list')
    if ($listing.sessions[0].active -or -not $listing.sessions[1].active) { throw 'Control changed tab selection' }
    $waited = Invoke-Control @('wait-for-text', '--pane', '1', '--regex', '--timeout', '5', 'BOARD')
    if (-not $waited.matched) { throw 'wait-for-text failed' }
    $null = Invoke-Control @('resize', '--pane', '1', '--cols', '76', '--rows', '22')
    $resized = Invoke-Control @('get-text', '--pane', '1')
    if ($resized.cols -ne 76 -or $resized.rows -ne 22) { throw 'Background resize failed' }
    $opened = Invoke-Control @('open', '--profile', 'first', '--cols', '78', '--rows', '23')
    if ($opened.active) { throw 'open selected a background tab' }
    $null = Invoke-Control @('wait-for-text', '--pane', [string]$opened.pane, '--timeout', '5', 'Loopback SSH echo')
    $listing = Invoke-Control @('list')
    if ($listing.sessions.Count -ne 3 -or -not $listing.sessions[1].active) { throw 'open changed active tab' }
    $null = Invoke-Control @('resize', '--pane', '2', '--cols', '72', '--rows', '21')
    $capture = Join-Path $state 'recording.vt'
    $null = Invoke-Control @('record', '--pane', '1', $capture)
    # The peer clears its screen and turns backspace into a three-byte erase.
    # A capture of input rather than output cannot reproduce this screen.
    $inputBytes = [Text.Encoding]::ASCII.GetBytes("`fTAPX`bE")
    $null = Invoke-Control @('send-text', '--pane', '1', '--no-paste', '--base64', [Convert]::ToBase64String($inputBytes))
    $null = Invoke-Control @('wait-for-text', '--pane', '1', '--timeout', '5', 'TAPE')
    $live = Invoke-Control @('get-text', '--pane', '1')
    $recorded = Invoke-Control @('record', '--pane', '1', '--stop')
    $output = [IO.File]::ReadAllBytes($capture)
    if ($recorded.bytes -ne $output.Length -or -not [Text.Encoding]::ASCII.GetString($output).Contains("TAPX`b `bE")) { throw 'Capture did not contain the echoed VT bytes' }
    $events = Invoke-Control @('events', '--pane', '1', '--since', '0')
    if ($events.journal.events.kind -notcontains 'recording-stopped') { throw 'Recording event missing' }
    $errorReply = & $ctl --pid $terminal.Id wait-for-text --pane 1 --timeout 1 'NO SUCH TEXT EXISTS' | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $errorReply.ok -or $errorReply.error -notmatch 'timed out') { throw 'wait timeout must fail with JSON' }
    $errorReply = & $ctl --pid $terminal.Id get-text --pane 999 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $errorReply.ok -or $errorReply.error -notmatch 'unknown pane') { throw 'Missing pane should fail with JSON' }
    $errorReply = & $ctl --pid 4294967295 list | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $errorReply.ok -or $errorReply.error -notmatch 'nothing was started') { throw 'Missing instance should fail without starting anything' }
    $localPane = [string](Invoke-Control @('open', '--profile', 'restart-local')).pane
    $null = Invoke-Control @('wait-for-text', '--pane', $localPane, '--timeout', '15', 'PS ')
    $null = Invoke-Control @('send-text', '--pane', $localPane, '--no-paste', '--base64', 'ZXhpdCAxMg0=')
    $cursor = 0
    $exitEvent = $null
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        $batch = Invoke-Control @('events', '--pane', $localPane, '--since', [string]$cursor, '--timeout', '2')
        foreach ($event in $batch.journal.events) {
            if ($event.kind -eq 'pane-exited') { $exitEvent = $event.data.exit_token }
        }
        $cursor = $batch.journal.next
    } while (($null -eq $exitEvent -or $null -eq $batch.restart_token) -and [DateTime]::UtcNow -lt $deadline)
    $exitToken = [string]$batch.restart_token
    if (!$exitToken -or $exitToken -ne [string]$exitEvent) { throw 'Exited local pane did not report its restart token' }
    $result = Invoke-Control @('restart', '--pane', $localPane, '--exit-token', $exitToken)
    if (!$result.accepted -or $result.pane -ne $localPane) { throw 'Restart did not accept the exited pane' }
    $again = & $ctl --pid $terminal.Id restart --pane $localPane --exit-token $exitToken | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $again.ok) { throw 'A single exit token started another local process twice' }
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        $restarted = (Invoke-Control @('list')).sessions | Where-Object id -eq $localPane
        if ($restarted.connection_generation -ge 2) { break }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($restarted.active -or $restarted.connection_generation -lt 2) { throw 'Restart changed focus or lost the new connection generation' }
    $null = $terminal.CloseMainWindow()
    if (-not $terminal.WaitForExit(5000)) { throw 'GUI failed to close' }
    $terminal = Start-Process -FilePath $gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'second') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $state 'restored.log') -RedirectStandardError (Join-Path $state 'restored.err')
    Wait-ControlManifest
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        $listing = Invoke-Control @('list')
        if ($listing.sessions[0].cols -eq 72 -and $listing.sessions[0].rows -eq 21) { break }
        Start-Sleep -Milliseconds 50
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($listing.sessions[0].cols -ne 72 -or $listing.sessions[0].rows -ne 21) { throw "Saved profile window was not restored: $($listing | ConvertTo-Json -Compress)" }
    $null = $terminal.CloseMainWindow()
    if (-not $terminal.WaitForExit(5000)) { throw 'Restored GUI failed to close' }
    $terminal = Start-Process -FilePath $gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--replay', ('"' + $capture + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $state 'replay.err')
    Wait-ControlManifest
    $null = Invoke-Control @('wait-for-text', '--pane', '1', '--timeout', '5', 'TAPE')
    $replayed = Invoke-Control @('get-text', '--pane', '1')
    if ($replayed.cols -ne $live.cols -or $replayed.rows -ne $live.rows -or $replayed.text -cne $live.text) { throw 'Replay did not reproduce the recorded grid and screen contents' }
    Write-Output "Control smoke passed. Verified loopback SSH, background PowerShell restart, window restore, and VT record/replay. Artifacts: $state"
} finally {
    if ($terminal -and -not $terminal.HasExited) {
        $null = $terminal.CloseMainWindow()
        if (-not $terminal.WaitForExit(5000)) { Stop-Process -Id $terminal.Id }
    }
    if ($peer -and -not $peer.HasExited) { Stop-Process -Id $peer.Id }
    $env:W11TERM_DATA_DIR = $oldDataDir
    $env:W11TERM_SETTINGS_PATH = $oldSettingsPath
}
if (-not (Test-Path -LiteralPath (Join-Path $state 'data/windows.json'))) { throw 'Window placement was not saved on native close' }

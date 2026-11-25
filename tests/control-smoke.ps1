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
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    $manifest = if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        Join-Path $env:ProgramData "w11term\elevated\$($identity.User.Value)\instances\$($terminal.Id).json"
    } else {
        Join-Path $env:LOCALAPPDATA "w11term\instances\$($terminal.Id).json"
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while (-not (Test-Path -LiteralPath $manifest)) {
        if ($terminal.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw "GUI did not publish its control manifest. See $state" }
        Start-Sleep -Milliseconds 50
    }
}
function Stop-TestWindow([Diagnostics.Process]$Process) {
    if ($Process -and -not $Process.HasExited) {
        $Process.Refresh()
        $null = $Process.CloseMainWindow()
        if (-not $Process.WaitForExit(5000)) {
            $Process.Kill()
            $Process.WaitForExit()
        }
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
    $history = "search-archive this SSH line`r" + ("filler`r" * 45) + 'search-fixture-finished'
    $null = Invoke-Control @('send-text', '--pane', '1', '--no-paste', '--base64', [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes($history)))
    $liveSearch = Invoke-Control @('wait-for-text', '--pane', '1', '--timeout', '5', 'search-fixture-finished')
    if ($liveSearch.text.Contains('search-archive')) { throw 'Scrollback search fixture still fits on screen' }
    $all = Invoke-Control @('find-text', '--all-panes', 'SEARCH-ARCHIVE')
    $found = @($all.panes | Where-Object { $_.count -gt 0 })
    if ($all.panes.Count -ne 3 -or $found.Count -ne 1 -or $found[0].pane -ne 1 -or $found[0].count -ne 1 -or
        $found[0].first.excerpt -cnotmatch 'search-archive this SSH line' -or $found[0].truncated) {
        throw "find-text did not locate the offscreen SSH line in its original pane: $($all | ConvertTo-Json -Depth 5)"
    }
    $one = Invoke-Control @('find-text', '--pane', '1', 'search-archive')
    $regex = Invoke-Control @('find-text', '--all-panes', '--regex', 'SEARCH-ARCHIVE\s+this\s+SSH\s+line')
    $regexHits = @($regex.panes | Where-Object count -gt 0)
    if ($regex.panes.Count -ne 3 -or $regexHits.Count -ne 1 -or $regexHits[0].pane -ne 1 -or
        $regexHits[0].count -ne 1 -or $regexHits[0].incomplete -ne $false -or
        $regexHits[0].first.row -ne $found[0].first.row -or $regexHits[0].first.column -ne $found[0].first.column) {
        throw 'find-text regex did not report the same offscreen SSH match'
    }
    $sensitive = Invoke-Control @('find-text', '--pane', '1', '--match-case', 'SEARCH-ARCHIVE')
    $both = Invoke-Control @('find-text', '--pane', '1', '--regex', '--match-case', 'search-archive\s+this SSH line')
    if ($sensitive.panes[0].count -ne 0 -or $both.panes[0].count -ne 1) { throw 'find-text case mode failed' }
    $badRegex = & $ctl --pid $terminal.Id find-text --all-panes --regex '[' | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $badRegex.ok -or $badRegex.error -notmatch 'invalid regular expression') {
        throw 'find-text invalid regex should fail with JSON'
    }
    if ($one.panes.Count -ne 1 -or $one.panes[0].first.row -ne $found[0].first.row -or
        (Invoke-Control @('get-text', '--pane', '1')).text -cne $liveSearch.text -or
        -not (Invoke-Control @('list')).sessions[1].active) { throw 'find-text changed the live screen or active pane' }
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
    Stop-TestWindow $terminal

    # A raw VT capture has no sidecar with a suggested starting grid.
    $rawCapture = Join-Path $state 'raw-image.vt'
    $esc = [char]27
    $red = '/wAA' * 64
    $wire = "${esc}[?25l${esc}[2;4H${esc}_Ga=T,i=49,f=24,s=8,v=8,c=1,r=1,C=1,q=2;$red${esc}\${esc}[3;1HRAW-IMAGE-READY"
    [IO.File]::WriteAllText($rawCapture, $wire, [Text.Encoding]::ASCII)
    $terminal = Start-Process -FilePath $gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--replay', ('"' + $rawCapture + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $state 'raw-replay.err')
    Wait-ControlManifest
    $matched = Invoke-Control @('wait-for-text', '--pane', '1', '--timeout', '8', 'RAW-IMAGE-READY')
    if (-not $matched.matched) { throw 'Raw image replay did not reach its marker' }
    $grid = Invoke-Control @('get-text', '--pane', '1')
    $png = Join-Path $state 'raw-replay.png'
    $null = Invoke-Control @('screenshot', '--pane', '1', $png)
    Add-Type -AssemblyName System.Drawing
    $bitmap = [Drawing.Bitmap]::FromFile($png)
    try {
        $x = [int][Math]::Floor(3.5 * $bitmap.Width / $grid.cols)
        $y = [int][Math]::Floor(1.5 * $bitmap.Height / $grid.rows)
        $color = $bitmap.GetPixel($x, $y)
        if ($color.R -lt 224 -or $color.G -gt 31 -or $color.B -gt 31) {
            throw "Raw replay dropped its image at cell (3,1): $color"
        }
    } finally { $bitmap.Dispose() }
    Write-Output "Control smoke passed. Verified loopback SSH, background PowerShell restart, window restore, VT record/replay, and raw replay image pixels. Artifacts: $state"
} finally {
    Stop-TestWindow $terminal
    if ($peer -and -not $peer.HasExited) { Stop-Process -Id $peer.Id }
    $env:W11TERM_DATA_DIR = $oldDataDir
    $env:W11TERM_SETTINGS_PATH = $oldSettingsPath
}
if (-not (Test-Path -LiteralPath (Join-Path $state 'data/windows.json'))) { throw 'Window placement was not saved on native close' }

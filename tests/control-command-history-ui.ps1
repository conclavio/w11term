[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$peer = [IO.File]::ReadAllText((Join-Path $State 'peer-root'))
$command = 'show ' + ('cmd' * 60) + ' ' + [char]0x754c
$outputText = 'r' + [char]0xe9 + 'sultat-' + ('out' * 60) + [Environment]::NewLine + [Environment]::NewLine + 'end'

function Type-Line([string]$Text) {
    foreach ($ch in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    Press-Key 13
}
function Expect-Failure([string[]]$Arguments, [string]$Reason) {
    $reply = & $Cli --pid $app.Id @Arguments | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $reply.ok -or $reply.error -notmatch $Reason) {
        throw "Expected failure '$Reason' for $Arguments, got exit $LASTEXITCODE and $($reply | ConvertTo-Json -Compress)"
    }
}
function Get-Entry([int]$Id, [long]$Generation) {
    $entry = Control @('get-command','--pane',"$remote",'--id',"$Id",'--connection-generation',"$Generation")
    foreach ($key in 'pane','connection_generation','id','command','exit_code','output','output_error') {
        if ($entry.PSObject.Properties.Name -cnotcontains $key) { throw "Missing get-command field '$key'" }
    }
    if ($entry.pane -ne $remote -or $entry.connection_generation -ne $Generation -or $entry.id -ne $Id) {
        throw "Wrong identity in command detail: $($entry | ConvertTo-Json -Compress)"
    }
    return $entry
}
function Start-CommandWait([uint64]$After, [uint64]$Generation, [string]$Name, [string]$OutputFile, [string]$Command) {
    $path = Join-Path $State "$Name.json"
    $arguments = @('--pid',"$($app.Id)",'wait-command','--pane',"$remote",'--after-id',"$After",'--connection-generation',"$Generation",'--timeout','15')
    if ($OutputFile) { $arguments += @('--output-file', ('"' + $OutputFile + '"')) }
    if ($Command) { $arguments += @('--command', ('"' + $Command + '"'), '--require-success') }
    $process = Start-Process -FilePath $Cli -ArgumentList $arguments -WindowStyle Hidden -PassThru -RedirectStandardOutput $path
    try {
        $null = $process.Handle
        if ($process.WaitForExit(250)) { throw "Command wait returned before the peer acted: $([IO.File]::ReadAllText($path))" }
        return @{ Process = $process; Path = $path }
    } catch {
        $process.Dispose()
        throw
    }
}
function Check-SavedOutput($Entry, [string]$ExpectedPath) {
    $expected = [Text.Encoding]::UTF8.GetBytes($outputText)
    if ($Entry.output_file.path -ne $ExpectedPath -or $Entry.output_file.bytes -ne $expected.Length -or
        $null -ne $Entry.output -or $null -ne $Entry.output_error -or
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($ExpectedPath)) -cne [Convert]::ToBase64String($expected)) {
        throw "Command output export changed the full bytes or the receipt: $($Entry | ConvertTo-Json -Compress)"
    }
}
function Complete-CommandWait($Wait, [int]$ExitCode) {
    $process = $Wait.Process
    try {
        if (!$process.WaitForExit(18000)) { throw "Command wait did not finish: $($Wait.Path)" }
        $reply = [IO.File]::ReadAllText($Wait.Path) | ConvertFrom-Json
        if ($process.ExitCode -ne $ExitCode -or $reply.ok -ne ($ExitCode -ne 1)) {
            throw "Command wait returned exit $($process.ExitCode): $($reply | ConvertTo-Json -Compress)"
        }
        return $reply
    } finally {
        if (!$process.HasExited) { $process.Kill(); $process.WaitForExit() }
        $process.Dispose()
    }
}
function Check-FocusAndLock {
    $panes = @((Control @('list')).sessions)
    $source = $panes | Where-Object id -eq $remote
    $active = @($panes | Where-Object active)
    if ($panes.Count -ne 2 -or $active.Count -ne 1 -or $active[0].id -ne $localPane -or !$source.read_only) {
        throw "Reading command history must leave the SSH pane locked and PowerShell focused"
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','68','--rows','16') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $screen = Control @('wait-for-text','--pane',"$remote",'--timeout','15','history-one-ready')
    if ($screen.cols -ge $command.Length) { throw 'Fixture needs a command spanning multiple physical rows' }
    Run-Palette 'Enable read-only pane input'
    $null = Wait-Until 'source read-only' { ((Control @('list')).sessions | Where-Object id -eq $remote).read_only }
    Run-Palette 'Split right with Palette PowerShell'
    $localPane = Wait-Until 'local shell split' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','PS ')
    Type-Line '$global:historyProof = $PID; "history-seed-" + "ok"'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','history-seed-ok')
    Check-FocusAndLock

    $listing = Control @('list-commands','--pane',"$remote")
    $v1 = $listing.connection_generation
    $rows = @($listing.commands)
    if ($listing.pane -ne $remote -or $v1 -lt 1 -or $rows.Count -ne 3 -or $listing.latest_id -ne $rows[0].id) { throw "Wrong listing: $($listing | ConvertTo-Json -Depth 5)" }
    foreach ($row in $rows) {
        foreach ($key in 'id','command','exit_code','output_available') {
            if ($row.PSObject.Properties.Name -cnotcontains $key) { throw "Missing list-commands field '$key'" }
        }
        if ($row.PSObject.Properties.Name -ccontains 'output' -or $row.PSObject.Properties.Name -ccontains 'output_error') { throw 'Command list included output details' }
    }
    if ($rows[0].id -le $rows[1].id -or $rows[1].id -le $rows[2].id -or
        $rows[0].command -cne 'archived' -or $rows[0].exit_code -ne 3 -or $rows[0].output_available -ne $false -or
        $null -ne $rows[1].command -or $null -ne $rows[1].exit_code -or $rows[1].output_available -ne $true -or
        $rows[2].command -cne $command -or $rows[2].exit_code -ne 17 -or $rows[2].output_available -ne $true) {
        throw "Command summaries, order or nullable fields changed: $($rows | ConvertTo-Json -Compress)"
    }
    $firstId = $rows[2].id
    $first = Get-Entry $firstId $v1
    if ($first.command -cne $command -or $first.exit_code -ne 17 -or $first.output -cne $outputText -or $null -ne $first.output_error) {
        throw "Wrapped command or output changed: $($first | ConvertTo-Json -Compress)"
    }
    $firstFile = Join-Path $State 'original capture.txt'
    $firstSaved = Control @('get-command','--pane',"$remote",'--id',"$firstId",'--connection-generation',"$v1",'--output-file',$firstFile)
    Check-SavedOutput $firstSaved $firstFile
    $empty = Get-Entry $rows[1].id $v1
    if ($null -ne $empty.command -or $null -ne $empty.exit_code -or $null -eq $empty.output -or $empty.output -cne '' -or $null -ne $empty.output_error) {
        throw "Empty output or unknown shell metadata changed: $($empty | ConvertTo-Json -Compress)"
    }
    $lost = Get-Entry $rows[0].id $v1
    if ($null -ne $lost.output -or $lost.output_error -notmatch 'left scrollback') { throw 'Missing scrollback incorrectly returned as output' }
    Expect-Failure @('list-commands','--pane','999999') 'unknown pane'
    Expect-Failure @('get-command','--pane',"$remote",'--connection-generation',"$v1") '--id is required'
    Expect-Failure @('get-command','--pane',"$remote",'--id',"$firstId") '--connection-generation is required'
    Expect-Failure @('get-command','--pane',"$remote",'--id','999999','--connection-generation',"$v1") 'not retained'
    Check-FocusAndLock

    $after = $listing.latest_id
    Expect-Failure @('wait-command','--pane',"$remote",'--after-id',"$after",'--connection-generation',"$v1",'--timeout','1') 'timed out'
    $waitFile = Join-Path $State 'new capture.txt'
    $pending = Start-CommandWait $after $v1 'pending-command' $waitFile $command
    [IO.File]::Delete((Join-Path $peer 'hold-shell-output'))
    $repeated = (Complete-CommandWait $pending 2).result
    if ($repeated.pane -ne $remote -or $repeated.connection_generation -ne $v1 -or
        $repeated.id -ne ($after + 2) -or $repeated.command -cne $command -or
        $repeated.exit_code -ne 6) {
        throw "Waiting for the repeated command returned the wrong record: $($repeated | ConvertTo-Json -Compress)"
    }
    Check-SavedOutput $repeated $waitFile
    $unfiltered = Control @('wait-command','--pane',"$remote",'--after-id',"$after",'--connection-generation',"$v1")
    if ($unfiltered.id -ne ($after + 1) -or $unfiltered.command -cne 'background check') {
        throw "The unfiltered wait should return the first new capture: $($unfiltered | ConvertTo-Json -Compress)"
    }
    Expect-Failure @('wait-command','--pane',"$remote",'--after-id',"$after",'--connection-generation',"$v1",'--command','unreported','--timeout','1') 'timed out without a matching completed command'
    Check-FocusAndLock

    # Have the peer drop the original transport while this pane is still read-only.
    $staleFile = Join-Path $State 'must stay absent.txt'
    $oldWait = Start-CommandWait $repeated.id $v1 'stale-command' $staleFile
    [IO.File]::Copy((Join-Path $State 'replacement.vt'), (Join-Path $peer 'copy-history.vt'), $true)
    [IO.File]::Delete((Join-Path $peer 'keep-first-connection'))
    $stale = Complete-CommandWait $oldWait 1
    if ($stale.error -notmatch 'connection changed|connection generation changed' -or [IO.File]::Exists($staleFile)) {
        throw "A wait on the old transport returned an unexpected error: $($stale.error)"
    }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','25','history-two-ready')
    $next = Control @('list-commands','--pane',"$remote")
    if ($next.pane -ne $remote -or $next.connection_generation -le $v1 -or @($next.commands).Count -ne 1 -or
        $next.commands[0].command -cne 'replacement') { throw 'Reconnected pane retained the wrong history' }
    $freshId = $next.commands[0].id
    if ($freshId -le $repeated.id) { throw "Expected monotonically assigned command IDs, got $freshId after $($repeated.id)" }
    Expect-Failure @('get-command','--pane',"$remote",'--id',"$firstId",'--connection-generation',"$v1") 'connection generation changed'
    Expect-Failure @('get-command','--pane',"$remote",'--id',"$freshId",'--connection-generation',"$v1") 'connection generation changed'
    Expect-Failure @('get-command','--pane',"$remote",'--id',"$firstId",'--connection-generation',"$($next.connection_generation)") 'not retained'
    $fresh = Get-Entry $freshId $next.connection_generation
    if ($fresh.command -cne 'replacement' -or $fresh.exit_code -ne 0 -or $fresh.output -cne 'second transport' -or $null -ne $fresh.output_error) {
        throw 'Fresh generation returned the wrong captured output'
    }
    Check-FocusAndLock
    Type-Line 'if ($historyProof -eq $PID) { "history-local-" + "alive" }'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','history-local-alive')
    [IO.File]::WriteAllText((Join-Path $State 'observed-command-ids'), "$firstId,$freshId")
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

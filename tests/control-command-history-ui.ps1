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
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
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
    if ($listing.pane -ne $remote -or $v1 -lt 1 -or $rows.Count -ne 3) { throw "Wrong listing: $($listing | ConvertTo-Json -Depth 5)" }
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

    # Have the peer drop the original transport while this pane is still read-only.
    [IO.File]::Copy((Join-Path $State 'replacement.vt'), (Join-Path $peer 'copy-history.vt'), $true)
    [IO.File]::Delete((Join-Path $peer 'keep-first-connection'))
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','25','history-two-ready')
    $next = Control @('list-commands','--pane',"$remote")
    if ($next.pane -ne $remote -or $next.connection_generation -le $v1 -or @($next.commands).Count -ne 1 -or
        $next.commands[0].command -cne 'replacement') { throw 'Reconnected pane retained the wrong history' }
    $freshId = $next.commands[0].id
    if ($freshId -le $rows[0].id) { throw "Expected monotonically assigned command IDs, got $freshId after $($rows[0].id)" }
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

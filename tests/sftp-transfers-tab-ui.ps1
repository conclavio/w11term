[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_SFTP_PICKER_SMOKE_DIR = Join-Path $State 'picker'
$profiles = Join-Path $State 'profiles.json'
$alphaRoot = [IO.File]::ReadAllText((Join-Path $State 'alpha-root'))
$betaRoot = [IO.File]::ReadAllText((Join-Path $State 'beta-root'))
$alphaGate = Join-Path $alphaRoot 'hold-transfer-read'
$betaGate = Join-Path $betaRoot 'hold-transfer-read'
$button = [System.Windows.Automation.ControlType]::Button
$text = [System.Windows.Automation.ControlType]::Text
$ellipsis = [char]0x2026
$dot = ' ' + [char]0xb7 + ' '
$empty = 'No active SFTP transfers in this window'

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Choose-Folder([string]$Name) {
    $dialog = Wait-Until 'owned download destination folder picker' {
        Find-OwnedDialog 'Choose a download destination folder'
    }
    Select-DialogItem $dialog $Name
    $null = Wait-Until 'accept selected folder' { [FixtureDialog]::ClickButton($dialog, 1) }
}
function Wait-BlockedRead([IO.BinaryReader]$Trace) {
    Wait-Until 'second SFTP read held on the wire' {
        $wire = [Text.Encoding]::UTF8.GetString((Read-PeerTrace $Trace))
        $entry = [regex]::Match($wire, '(?m)^READ ([1-9][0-9]*)\r?$')
        if ($entry.Success) { [int]$entry.Groups[1].Value }
    }
}
function Pane-ByLabel([string]$Label) {
    ((Control @('list')).sessions | Where-Object label -eq $Label).id
}
function Transfer-Result([uint64]$Pane, [string]$Id) {
    $events = (Control @('events','--pane',"$Pane",'--since','0','--timeout','0')).journal.events
    $events | Where-Object { $_.kind -ceq 'sftp-transfer' -and $_.data.transfer_id -ceq $Id }
}
function Reject-TransferId([string]$Id) {
    $reply = & $Cli --pid $app.Id sftp cancel-transfer $Id | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $reply.ok -or $reply.error -notmatch 'no longer active') {
        throw "Expected stale transfer ID to fail, got $($reply | ConvertTo-Json -Compress)"
    }
}

$alphaTrace = $null
$betaTrace = $null
$waiting = $null
$waitPath = Join-Path $State 'wait-alpha.json'
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','first-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $alphaTrace = Open-PeerTraceReader (Join-Path $alphaRoot 'transfer-trace')
    $betaTrace = Open-PeerTraceReader (Join-Path $betaRoot 'transfer-trace')
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $alpha = Wait-Until 'first SSH pane' { Pane-ByLabel 'first-ssh' }
    $null = Control @('wait-for-text','--pane',"$alpha",'--timeout','15','src/trace.rs')

    Run-Palette 'SFTP transfers'
    $null = Wait-Until 'empty transfer tab' { Find-Element $empty $text }
    Press-Key 0x77
    Wait-Focus $alpha
    Run-Palette 'Open files here'
    $null = Wait-Until 'first SSH directory' { Find-Element 'bulk-alpha' $button }
    Click "Select downloads$ellipsis"
    foreach ($name in 'bulk-alpha','queued-alpha') {
        $mark = "Mark $name for download"
        (Wait-CheckBox $mark ([System.Windows.Automation.ToggleState]::Off)).Toggle()
        $null = Wait-CheckBox $mark ([System.Windows.Automation.ToggleState]::On)
    }
    Click 'Download marked items'
    Choose-Folder 'alpha'
    $alphaBytes = Wait-BlockedRead $alphaTrace
    Run-Palette 'SFTP files'

    $profile = (([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles).'second-ssh'
    Run-Palette "New tab with second-ssh    $($profile.user)@$($profile.host):$($profile.port)"
    $beta = Wait-Until 'second SSH pane' { Pane-ByLabel 'second-ssh' }
    Wait-Focus $beta
    $null = Control @('wait-for-text','--pane',"$beta",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Click 'beta folder/'
    Click "Download folder$ellipsis"
    Choose-Folder 'beta'
    $betaBytes = Wait-BlockedRead $betaTrace

    Run-Palette 'SFTP transfers'
    $alphaCancel = "Cancel transfer for Pane $alpha"
    $betaCancel = "Cancel transfer for Pane $beta"
    $alphaSize = (Get-Item -LiteralPath (Join-Path $alphaRoot 'shell directory/bulk-alpha')).Length
    $null = Wait-Until 'both transfers and their byte progress' {
        (Find-Element $alphaCancel $button) -and (Find-Element $betaCancel $button) -and
            (Find-Element "Downloading /shell directory/bulk-alpha$($dot)Item 1 of 2" $text) -and
            (Find-Element 'Downloading folder /shell directory/beta folder' $text) -and
            (Find-Element "$alphaBytes / $alphaSize bytes" $text) -and
            (Find-Element "$betaBytes bytes" $text)
    }
    $rows = @((Control @('sftp','list-transfers')).transfers)
    $first = @($rows | Where-Object pane -eq $alpha)
    $second = @($rows | Where-Object pane -eq $beta)
    $filtered = @((Control @('sftp','list-transfers','--pane',"$beta")).transfers)
    if ($rows.Count -ne 2 -or $first.Count -ne 1 -or $second.Count -ne 1 -or
        $filtered.Count -ne 1 -or $filtered[0].transfer_id -cne $second[0].transfer_id -or
        $first[0].transfer_id -ceq $second[0].transfer_id -or
        $first[0].operation -cne 'download-file' -or $first[0].state -cne 'running' -or
        $first[0].batch.item -ne 1 -or $first[0].batch.total -ne 2 -or
        $first[0].progress.bytes -ne $alphaBytes -or $first[0].progress.total -ne $alphaSize -or
        $second[0].operation -cne 'download-folder' -or $second[0].progress.bytes -ne $betaBytes) {
        throw "Unexpected transfer snapshots: $($rows | ConvertTo-Json -Depth 5 -Compress)"
    }
    $firstId = $first[0].transfer_id
    $secondId = $second[0].transfer_id
    $timedOut = & $Cli --pid $app.Id sftp wait-transfer $firstId --timeout 1 | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $timedOut.ok -or $timedOut.error -notmatch 'still active') {
        throw "Expected the blocked transfer to outlast the wait: $($timedOut | ConvertTo-Json -Compress)"
    }
    $waiting = Start-Process -FilePath $Cli -ArgumentList @('--pid',"$($app.Id)",'sftp','wait-transfer',$firstId,'--timeout','30') -WindowStyle Hidden -PassThru -RedirectStandardOutput $waitPath
    $null = $waiting.Handle
    $parts = $firstId.Split(':')
    Reject-TransferId ('{0}:{1}:{2}' -f $parts[0], ([uint64]$parts[1] + 1), $parts[2])
    $cancel = Control @('sftp','cancel-transfer',$firstId)
    if ($cancel.transfer_id -cne $firstId -or $cancel.state -cne 'cancelling') {
        throw 'Expected cancellation of the exact first transfer'
    }
    $repeated = Control @('sftp','cancel-transfer',$firstId)
    if ($repeated.state -cne 'already-cancelling') { throw 'Repeated cancellation was not acknowledged' }
    $null = Wait-Until 'cancellation remains on the transfers tab' {
        $alphaRow = Find-Element $alphaCancel $button
        $betaRow = Find-Element $betaCancel $button
        $alphaRow -and !$alphaRow.Current.IsEnabled -and $betaRow -and $betaRow.Current.IsEnabled
    }
    [IO.File]::Delete($alphaGate)
    $null = Wait-Until 'only the unrelated transfer remains' {
        !(Find-Element $alphaCancel $button) -and (Find-Element $betaCancel $button)
    }
    $cancelled = @(Transfer-Result $alpha $firstId)
    if ($cancelled.Count -ne 1 -or !$cancelled[0].data.background -or
        $cancelled[0].data.outcome -cne 'cancelled' -or
        $cancelled[0].data.message -notmatch 'Cancelled download.*0 of 2 items completed') {
        throw 'The pane journal did not report the cancelled batch by ID'
    }
    $null = Wait-Until 'cancelled result shown below the active transfer' {
        (Find-Element 'Recent transfers' $text) -and
            (Find-Element "Cancelled$dot$($cancelled[0].data.message)" $text) -and
            (Find-Element $betaCancel $button)
    }
    if (Find-Element "Open download folder for transfer $firstId" $button) {
        throw 'A cancelled batch without saved items offered an Explorer action in history'
    }
    if (!$waiting.WaitForExit(15000)) { throw 'The named-pipe waiter did not return' }
    $waitReply = [IO.File]::ReadAllText($waitPath) | ConvertFrom-Json
    if ($waiting.ExitCode -ne 0 -or !$waitReply.ok -or
        $waitReply.result.transfer.transfer_id -cne $firstId -or
        $waitReply.result.transfer.outcome -cne 'cancelled' -or
        $waitReply.result.sequence -ne $cancelled[0].sequence) {
        throw "Wrong cancellation result from wait-transfer (exit $($waiting.ExitCode), expected ID $firstId and sequence $($cancelled[0].sequence)): $($waitReply | ConvertTo-Json -Depth 5 -Compress)"
    }
    Reject-TransferId $firstId

    Click "Open SFTP drawer for Pane $beta"
    Wait-Focus $beta
    $null = Wait-Until 'second transfer is visible in the originating drawer' { Find-Element 'Cancel transfer' $button }
    [IO.File]::Delete($betaGate)
    $finished = Wait-Until 'foreground completion in the transfer journal' { Transfer-Result $beta $secondId }
    if ($finished.data.background -or $finished.data.outcome -cne 'completed' -or
        $finished.data.message -notmatch 'Downloaded /shell directory/beta folder') {
        throw 'The pane journal did not report the foreground folder download'
    }
    $retained = Control @('sftp','wait-transfer',$secondId,'--timeout','1')
    if ($retained.sequence -ne $finished.sequence -or $retained.transfer.outcome -cne 'completed') {
        throw 'A late waiter did not receive the retained foreground result'
    }
    Run-Palette 'SFTP transfers'
    $null = Wait-Until 'no active transfers and both results remain' {
        (Find-Element $empty $text) -and
            (Find-Element "Completed$dot$($finished.data.message)" $text) -and
            (Find-Element "Cancelled$dot$($cancelled[0].data.message)" $text) -and
            (Find-Element "Show download in File Explorer for transfer $secondId" $button)
    }
    if (@((Control @('sftp','list-transfers')).transfers).Count -ne 0) {
        throw 'The control pipe still lists an ended transfer'
    }
} catch {
    $alphaWire = if ($alphaTrace) { [Text.Encoding]::UTF8.GetString((Read-PeerTrace $alphaTrace)) }
    $betaWire = if ($betaTrace) { [Text.Encoding]::UTF8.GetString((Read-PeerTrace $betaTrace)) }
    throw "$_ at $($_.ScriptStackTrace). First peer: $alphaWire Second peer: $betaWire"
} finally {
    if ($waiting) {
        if (!$waiting.HasExited) { $waiting.Kill(); $waiting.WaitForExit() }
        $waiting.Dispose()
    }
    [IO.File]::Delete($alphaGate)
    [IO.File]::Delete($betaGate)
    if ($alphaTrace) { $alphaTrace.Dispose() }
    if ($betaTrace) { $betaTrace.Dispose() }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_SFTP_PICKER_SMOKE_DIR = Join-Path $State 'picker'
$env:W11TERM_TEST_EXPLORER_TRACE = Join-Path $State 'explorer-download.json'
$profiles = Join-Path $State 'profiles.json'
$peer = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$readGate = Join-Path $peer 'hold-transfer-read'
$button = [System.Windows.Automation.ControlType]::Button
$download = 'Download folder' + [char]0x2026

function Click([string]$Label) {
    Invoke-Button (Wait-Until "enabled '$Label'" {
        $item = Find-Element $Label $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Choose-DownloadFolder([string]$LocalParent) {
    $dialog = Wait-Until 'owned download destination folder picker' {
        Find-OwnedDialog 'Choose a download destination folder'
    }
    Select-DialogItem $dialog $LocalParent
    $null = Wait-Until 'accept selected parent folder' { [FixtureDialog]::ClickButton($dialog, 1) }
}
function Download-To([string]$RemoteName, [string]$LocalParent) {
    Click "$RemoteName/"
    Click $download
    Choose-DownloadFolder $LocalParent
}
function Wait-Status([string]$Pattern) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $text = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    $null = Wait-Until "drawer message matching '$Pattern'" {
        foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text)) {
            if ($item.Current.Name -match $Pattern) { return $true }
        }
    }
}

function Assert-Explorer([string]$ButtonName, [string]$Action, [string]$Expected) {
    [IO.File]::Delete($env:W11TERM_TEST_EXPLORER_TRACE)
    Click $ButtonName
    $trace = Wait-Until "native Explorer call for '$ButtonName'" {
        try {
            $sharing = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
            $stream = [IO.File]::Open($env:W11TERM_TEST_EXPLORER_TRACE, [IO.FileMode]::Open,
                [IO.FileAccess]::Read, $sharing)
        } catch [IO.FileNotFoundException] {
            return
        } catch [IO.IOException] {
            if (($_.Exception.HResult -band 0xffff) -in 32,33) { return }
            throw
        }
        $reader = [IO.StreamReader]::new($stream)
        try { $reader.ReadToEnd() | ConvertFrom-Json } finally { $reader.Dispose() }
    }
    $samePath = if ([IO.Directory]::Exists($Expected)) {
        [IO.Directory]::Exists($trace.path) -and
            [IO.Path]::GetFullPath($Expected) -ieq [IO.Path]::GetFullPath($trace.path)
    } else {
        [FixtureDialog]::SameFile($Expected, $trace.path)
    }
    if ($trace.action -cne $Action -or !$samePath) {
        throw "Explorer received the wrong action or item: $($trace | ConvertTo-Json -Compress)"
    }
}

function Download-Verified([string]$Digest, [string]$Destination, [switch]$ResumeDownload) {
    Click 'verified.bin'
    Click ('Download and verify' + [char]0x2026)
    $field = Wait-Until 'expected SHA-256 field' { Find-Element 'Expected SHA-256' }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Digest)
    if ($ResumeDownload) {
        (Wait-CheckBox 'Resume interrupted download' ([System.Windows.Automation.ToggleState]::Off)).Toggle()
        $null = Wait-CheckBox 'Resume interrupted download' ([System.Windows.Automation.ToggleState]::On)
    }
    Click 'Choose destination'
    $dialog = Wait-Until 'owned verified download save picker' { Find-OwnedDialog 'Save As' }
    $null = Wait-Until 'enter verified file destination' {
        [FixtureDialog]::SetFilename($dialog, $Destination, 'verified.bin')
    }
    $null = Wait-Until 'accept verified file destination' { [FixtureDialog]::ClickButton($dialog, 1) }
}

function Mark-Download([string]$Label) {
    (Wait-CheckBox "Mark $Label for download" ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $null = Wait-CheckBox "Mark $Label for download" ([System.Windows.Automation.ToggleState]::On)
}

function Download-Marked([string]$Parent) {
    Click 'Download marked items'
    Choose-DownloadFolder $Parent
}

function Start-BlockedBatch([string]$Parent) {
    [IO.File]::WriteAllBytes($readGate, [byte[]]@())
    $start = (Read-PeerTrace $reader).Length
    Click ('Select downloads' + [char]0x2026)
    Mark-Download 'interrupted/'
    Mark-Download 'verified.bin'
    Download-Marked $Parent
    $null = Wait-Until 'blocked SFTP read in the first marked item' {
        $trace = Read-PeerTrace $reader
        if ($trace.Length -gt $start) {
            $wire = [Text.Encoding]::UTF8.GetString($trace, $start, $trace.Length - $start)
            $wire.Contains('OPEN /shell directory/interrupted/nested/payload.bin') -and $wire -match '(?m)^READ [1-9][0-9]*\r?$'
        }
    }
}

function Read-TransferEvents([uint64]$Since) {
    $journal = (Control @('events','--pane',"$pane",'--since',"$Since",'--timeout','0')).journal
    $journal.events | Where-Object { $_.kind -ceq 'sftp-transfer' -and $_.data.background }
}

function Visit-TransferNotice([string]$Summary) {
    Click 'Attention inbox, 1 unread'
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $button)
    $row = Wait-Until "unread SFTP result matching '$Summary'" {
        foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) {
            if ($item.Current.Name -match "^Unread.*SFTP transfer.*$Summary") { return $item }
        }
    }
    Invoke-Button $row
    Wait-Focus $pane
    Wait-Status $Summary
    $null = Wait-Until 'SFTP notice acknowledged' { Find-Element 'Attention inbox, 0 unread' $button }
}

$reader = $null
try {
    $explorer = New-Object -ComObject Shell.Application
    $priorExplorerWindows = @($explorer.Windows() | ForEach-Object { $_.HWND })
} catch { $explorer = $null }
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $reader = Open-PeerTraceReader (Join-Path $peer 'transfer-trace')
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','src/trace.rs')
    $since = (Control @('events','--pane',"$pane",'--since','0','--timeout','0')).journal.next
    Run-Palette 'Open files here'
    $null = Wait-Until 'reported SSH directory' { (Find-Element '/shell directory') -and (Find-Element 'snapshot/' $button) }

    Download-To 'interrupted' 'receive here'
    $null = Wait-Until 'blocked SFTP read inside the selected remote folder' {
        $wire = [Text.Encoding]::UTF8.GetString((Read-PeerTrace $reader))
        $wire.Contains('OPEN /shell directory/interrupted/nested/payload.bin') -and $wire -match '(?m)^READ [1-9][0-9]*\r?$'
    }
    Click 'Cancel transfer'
    $null = Wait-Until 'cancel acknowledged by the drawer' {
        $cancel = Find-Element 'Cancel transfer' $button
        $cancel -and !$cancel.Current.IsEnabled
    }
    [IO.File]::Delete($readGate)
    Wait-Status '^Cancelled download of /shell directory/interrupted\b'

    Download-To 'snapshot' 'receive here'
    Wait-Status '^Downloaded /shell directory/snapshot\b.*2 files, 23 bytes'
    Assert-Explorer 'Show download in File Explorer' 'select' (Join-Path $State 'picker/receive here/snapshot')
    # Even when single-file replacement is enabled, an existing directory cannot be merged or replaced.
    (Wait-CheckBox 'Replace existing files' ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $null = Wait-CheckBox 'Replace existing files' ([System.Windows.Automation.ToggleState]::On)
    Download-To 'snapshot' 'already exists'
    Wait-Status 'destination.*exists'
    if (Find-Element 'Show download in File Explorer' $button) { throw 'An unsuccessful download offered an Explorer action' }

    $digest = [IO.File]::ReadAllText((Join-Path $State 'verified.sha256'))
    Download-Verified ('0' * 64) (Join-Path $State 'rejected.bin')
    Wait-Status 'SHA-256 mismatch'
    Download-Verified $digest (Join-Path $State 'accepted.bin') -ResumeDownload
    Wait-Status 'SHA-256 verified'
    Assert-Explorer 'Show download in File Explorer' 'select' (Join-Path $State 'accepted.bin')

    Click ('Select downloads' + [char]0x2026)
    Mark-Download 'snapshot/'
    Click ('Filter filenames' + [char]0x2026)
    $edit = [System.Windows.Automation.ControlType]::Edit
    $filter = Wait-Until 'filename filter for marked downloads' { Find-Element 'Filter filenames' $edit }
    $filter.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('verified.bin')
    $null = Wait-Until 'filtered single file' { Find-Element '1 match' }
    Click 'Done'
    Mark-Download 'verified.bin'
    Download-Marked 'batch received'
    Wait-Status '^Downloaded /shell directory/verified\.bin.*2 of 2 items completed'

    Click 'Clear filter'
    Click ('Select downloads' + [char]0x2026)
    Mark-Download 'snapshot/'
    Mark-Download 'verified.bin'
    Mark-Download 'z-later.bin'
    Download-Marked 'batch conflict'
    Wait-Status '^Could not download /shell directory/verified\.bin.*destination.*exists.*1 of 3 items completed'

    Start-BlockedBatch 'batch cancelled'
    Click 'Cancel transfer'
    $null = Wait-Until 'batch cancel acknowledged' {
        $cancel = Find-Element 'Cancel transfer' $button
        $cancel -and !$cancel.Current.IsEnabled
    }
    [IO.File]::Delete($readGate)
    Wait-Status '^Cancelled download of /shell directory/interrupted.*0 of 2 items completed'
    if (Find-Element 'Open download folder' $button) { throw 'A cancelled batch without saved items offered an Explorer action' }
    if (@(Read-TransferEvents $since).Count -ne 0) { throw 'Visible transfers produced a background notice' }

    Start-BlockedBatch 'batch background'
    $dot = ' ' + [char]0xb7 + ' '
    Run-Palette "New tab with Palette PowerShell${dot}Local"
    $local = Wait-Until 'PowerShell beside the background transfer' {
        ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id
    }
    Wait-Focus $local
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    $foreground = [FixtureMessages]::GetForegroundWindow()
    [IO.File]::Delete($readGate)
    $null = Wait-Until 'one unread SFTP result while PowerShell is active' { Find-Element 'Attention inbox, 1 unread' $button }
    Wait-Focus $local
    if ([FixtureMessages]::GetForegroundWindow() -ne $foreground) { throw 'Transfer stole window focus' }
    Visit-TransferNotice 'Downloaded /shell directory/verified\.bin.*2 of 2 items completed'
    $events = @(Read-TransferEvents $since)
    if ($events.Count -ne 1 -or $events[0].data.message -notmatch '2 of 2 items completed') {
        throw 'Expected one final result for the entire batch'
    }

    Start-BlockedBatch 'batch background conflict'
    Run-Palette 'SFTP files'
    $null = Wait-Until 'drawer closed during the transfer' { !(Find-Element 'verified.bin' $button) }
    [IO.File]::Delete($readGate)
    Visit-TransferNotice 'Could not download /shell directory/verified\.bin.*destination.*exists.*1 of 2 items completed'
    $results = @(Read-TransferEvents $since)
    if ($results.Count -ne 2 -or $results[1].data.outcome -cne 'failed') {
        throw 'Expected one additional notice for the failed batch'
    }
    $failed = Control @('sftp','wait-transfer',$results[1].data.transfer_id)
    if ($failed.sequence -ne $results[1].sequence -or $failed.transfer.outcome -cne 'failed') {
        throw 'Expected wait-transfer to return the retained failure'
    }
    Assert-Explorer 'Open download folder' 'explore' (Join-Path $State 'picker/batch background conflict')

    Run-Palette 'SFTP files'
    foreach ($c in 'folder-download-live'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','folder-download-live')
    Click 'Attention inbox, 0 unread'
    Assert-Explorer "Open download folder for palette-ssh, notice $($events[0].sequence)" 'explore' (Join-Path $State 'picker/batch background')
    Run-Palette 'SFTP transfers'
    Assert-Explorer "Open download folder for transfer $($results[1].data.transfer_id)" 'explore' (Join-Path $State 'picker/batch background conflict')
} catch {
    $wire = if ($reader) { [Text.Encoding]::UTF8.GetString((Read-PeerTrace $reader)) }
    throw "$_ at $($_.ScriptStackTrace). Server packets: $wire"
} finally {
    [IO.File]::Delete($readGate)
    if ($reader) { $reader.Dispose() }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
    if ($explorer) {
        $ownedRoot = [IO.Path]::GetFullPath($State).TrimEnd('\') + '\'
        try { $views = @($explorer.Windows()) } catch { $views = @() }
        foreach ($view in $views) {
            try {
                $folder = $view.Document.Folder.Self.Path
                if ($view.HWND -notin $priorExplorerWindows -and $folder -and
                    $folder.StartsWith($ownedRoot, [StringComparison]::OrdinalIgnoreCase)) { $view.Quit() }
            } catch { }
        }
    }
}

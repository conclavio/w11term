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

$alphaTrace = $null
$betaTrace = $null
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
    Click $alphaCancel
    $null = Wait-Until 'cancellation remains on the transfers tab' {
        $alphaRow = Find-Element $alphaCancel $button
        $betaRow = Find-Element $betaCancel $button
        $alphaRow -and !$alphaRow.Current.IsEnabled -and $betaRow -and $betaRow.Current.IsEnabled
    }
    [IO.File]::Delete($alphaGate)
    $null = Wait-Until 'only the unrelated transfer remains' {
        !(Find-Element $alphaCancel $button) -and (Find-Element $betaCancel $button)
    }

    Click "Open SFTP drawer for Pane $beta"
    Wait-Focus $beta
    $null = Wait-Until 'second transfer is visible in the originating drawer' { Find-Element 'Cancel transfer' $button }
    Run-Palette 'SFTP transfers'
    [IO.File]::Delete($betaGate)
    $null = Wait-Until 'completed transfers leave the tab' { Find-Element $empty $text }
} catch {
    $alphaWire = if ($alphaTrace) { [Text.Encoding]::UTF8.GetString((Read-PeerTrace $alphaTrace)) }
    $betaWire = if ($betaTrace) { [Text.Encoding]::UTF8.GetString((Read-PeerTrace $betaTrace)) }
    throw "$_ at $($_.ScriptStackTrace). First peer: $alphaWire Second peer: $betaWire"
} finally {
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

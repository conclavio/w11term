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

$reader = $null
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $reader = Open-PeerTraceReader (Join-Path $peer 'transfer-trace')
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','src/trace.rs')
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
    # Even when single-file replacement is enabled, an existing directory cannot be merged or replaced.
    (Wait-CheckBox 'Replace existing files' ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $null = Wait-CheckBox 'Replace existing files' ([System.Windows.Automation.ToggleState]::On)
    Download-To 'snapshot' 'already exists'
    Wait-Status 'destination.*exists'

    $digest = [IO.File]::ReadAllText((Join-Path $State 'verified.sha256'))
    Download-Verified ('0' * 64) (Join-Path $State 'rejected.bin')
    Wait-Status 'SHA-256 mismatch'
    Download-Verified $digest (Join-Path $State 'accepted.bin') -ResumeDownload
    Wait-Status 'SHA-256 verified'

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

    [IO.File]::WriteAllBytes($readGate, [byte[]]@())
    $start = (Read-PeerTrace $reader).Length
    Click ('Select downloads' + [char]0x2026)
    Mark-Download 'interrupted/'
    Mark-Download 'verified.bin'
    Download-Marked 'batch cancelled'
    $null = Wait-Until 'blocked SFTP read in the first marked item' {
        $trace = Read-PeerTrace $reader
        if ($trace.Length -gt $start) {
            $wire = [Text.Encoding]::UTF8.GetString($trace, $start, $trace.Length - $start)
            $wire.Contains('OPEN /shell directory/interrupted/nested/payload.bin') -and $wire -match '(?m)^READ [1-9][0-9]*\r?$'
        }
    }
    Click 'Cancel transfer'
    $null = Wait-Until 'batch cancel acknowledged' {
        $cancel = Find-Element 'Cancel transfer' $button
        $cancel -and !$cancel.Current.IsEnabled
    }
    [IO.File]::Delete($readGate)
    Wait-Status '^Cancelled download of /shell directory/interrupted.*0 of 2 items completed'

    Run-Palette 'SFTP files'
    foreach ($c in 'folder-download-live'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','folder-download-live')
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
}

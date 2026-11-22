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
$writeGate = Join-Path $peer 'hold-transfer-write'
$button = [System.Windows.Automation.ControlType]::Button
$upload = 'Upload folder' + [char]0x2026
$release = 'release ' + [char]0x96ea

function Click([string]$Label) {
    Invoke-Button (Wait-Until "enabled '$Label'" {
        $item = Find-Element $Label $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Pick-Folder([string]$Folder) {
    Click $upload
    $dialog = Wait-Until 'owned upload folder picker' { Find-OwnedDialog 'Choose a folder to upload' }
    Select-DialogItem $dialog $Folder
    $null = Wait-Until 'accept exact selected source folder' { [FixtureDialog]::ClickButton($dialog, 1) }
}
function Wait-Status([string]$Pattern) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $text = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    try {
        $null = Wait-Until "drawer message matching '$Pattern'" {
            foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text)) {
                if ($item.Current.Name -match $Pattern) { return $true }
            }
        }
    } catch {
        $labels = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text) | ForEach-Object { $_.Current.Name }
        throw "$_; visible text: $($labels -join ' | ')"
    }
}
function Type-Line([string]$Text) {
    foreach ($c in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
}

$reader = $null
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $reader = Open-PeerTraceReader (Join-Path $peer 'transfer-trace')
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    $ssh = (Control @('list')).sessions | Where-Object id -eq $remote
    Run-Palette 'Split right with Palette PowerShell'
    $localPane = Wait-Until 'local neighboring pane' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','PS ')
    Type-Line '$global:folderTransferPid = $PID; "seeded-" + "local"'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','seeded-local')
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    Run-Palette 'Open files here'
    $null = Wait-Until 'original SSH shell directory' { (Find-Element '/shell directory') -and (Find-Element 'readme.txt' $button) }

    Pick-Folder 'interrupted'
    $offset = Wait-Until 'in-flight second write to the nested file' {
        $wire = [Text.Encoding]::UTF8.GetString((Read-PeerTrace $reader))
        $entry = [regex]::Match($wire, '(?m)^WRITE ([1-9][0-9]*)\r?$')
        if ($wire.Contains('OPEN /shell directory/interrupted/nested/payload.bin') -and $entry.Success) {
            [int]$entry.Groups[1].Value
        }
    }
    Click 'Cancel transfer'
    $null = Wait-Until 'cancel accepted before the write reply' {
        $cancel = Find-Element 'Cancel transfer' $button
        $cancel -and !$cancel.Current.IsEnabled
    }
    [IO.File]::Delete($writeGate)
    Wait-Status "^Cancelled upload of /shell directory/interrupted \(0 files completed, $($offset * 2) bytes acknowledged\)\. The remote folder may be incomplete\.$"

    Pick-Folder $release
    Wait-Status "^Uploaded folder /shell directory/$release \(2 files, 3 folders, 6 bytes, 1 skipped\)\.$"
    $null = Wait-Until 'refreshed directory lists uploaded root' { Find-Element "$release/" $button }
    (Wait-CheckBox 'Replace existing files' ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $null = Wait-CheckBox 'Replace existing files' ([System.Windows.Automation.ToggleState]::On)
    Pick-Folder $release
    Wait-Status "^Could not upload folder /shell directory/$release.*already exists"

    Run-Palette 'SFTP files'
    foreach ($c in 'folder-upload-live'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','folder-upload-live')
    $panes = (Control @('list')).sessions
    $active = $panes | Where-Object id -eq $remote
    if ($panes.Count -ne 2 -or !$active.active -or $active.connection_generation -ne $ssh.connection_generation -or $active.tab_id -ne $ssh.tab_id) {
        throw 'The upload changed the original SSH pane, connection or tab'
    }
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $localPane
    Type-Line 'if ($folderTransferPid -eq $PID) { "local-still-" + "alive" }'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','local-still-alive')
} catch {
    $wire = if ($reader) { [Text.Encoding]::UTF8.GetString((Read-PeerTrace $reader)) }
    throw "$_ at $($_.ScriptStackTrace). Server packets: $wire"
} finally {
    [IO.File]::Delete($writeGate)
    if ($reader) { $reader.Dispose() }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

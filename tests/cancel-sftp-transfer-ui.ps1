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
$writeGate = Join-Path $peer 'hold-transfer-write'
$download = 'Download' + [char]0x2026
$button = [System.Windows.Automation.ControlType]::Button

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Type-Line([string]$Text) {
    foreach ($c in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
}
function Owned-CommonDialog([IntPtr]$Owner) {
    foreach ($hwnd in [FixtureDialog]::Windows($app.Id)) {
        if ([FixtureDialog]::GetWindow($hwnd, 4) -ne $Owner) { continue }
        $element = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        if ($element.Current.ClassName -eq '#32770') { return $hwnd }
    }
}
function Click-Dialog([IntPtr]$Handle, [int]$Id) {
    $null = Wait-Until "owned dialog button $Id" {
        if ([FixtureDialog]::ClickButton($Handle, $Id)) { return $true }
        if ($Id -eq 6) { return [FixtureDialog]::ConfirmReplace($Handle) }
    }
}
function Start-Download([string]$Name, [switch]$Replace) {
    Click $Name
    Click $download
    $save = Wait-Until 'owned Save As picker' { Owned-CommonDialog $window }
    # The native picker opens inside the fixture and suggests the selected basename.
    Click-Dialog $save 1
    if ($Replace) {
        $confirm = Wait-Until 'overwrite confirmation owned by Save As' { Owned-CommonDialog $save }
        Click-Dialog $confirm 6
    }
}
function Select-UploadPair([IntPtr]$Picker) {
    Select-DialogItem $Picker '01-first-upload'
    $dialog = [System.Windows.Automation.AutomationElement]::FromHandle($Picker)
    $byName = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, '02-queued-upload')
    $selectable = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::IsSelectionItemPatternAvailableProperty, $true)
    $match = [System.Windows.Automation.AndCondition]::new($byName, $selectable)
    $null = Wait-Until 'second upload added to native multi-selection' {
        $second = $dialog.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $match)
        if (!$second) { return }
        $selection = $second.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern)
        if (!$selection.Current.IsSelected) { $selection.AddToSelection() }
        $list = $selection.Current.SelectionContainer.GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern)
        $names = @($list.Current.GetSelection() | ForEach-Object { $_.Current.Name })
        return $names.Count -eq 2 -and $names -ccontains '01-first-upload' -and $names -ccontains '02-queued-upload'
    }
}
function Cancel-AtSecondPacket([string]$Opcode, [string]$Gate) {
    $offset = Wait-Until "second $Opcode at the server" {
        $wire = [Text.Encoding]::ASCII.GetString((Read-PeerTrace $reader))
        $entry = [regex]::Match($wire, "(?m)^$Opcode ([1-9][0-9]*)\r?$")
        if ($entry.Success) { [int]$entry.Groups[1].Value }
    }
    if ($Opcode -eq 'WRITE') {
        $null = Control @('send-text','--pane',"$remote",'--no-paste','while-upload/')
        $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','while-upload/')
        Run-Palette 'SFTP transfers'
        $dot = ' ' + [char]0xb7 + ' '
        $null = Wait-Until 'active upload and batch length in the transfers tab' {
            Find-Element "Uploading /shell directory/01-first-upload$($dot)Item 1 of 2"
        }
        Click "Open SFTP drawer for Pane $remote"
        Wait-Focus $remote
    }
    Click 'Cancel transfer'
    $null = Wait-Until 'cancel disabled before server acknowledges the in-flight packet' {
        $item = Find-Element 'Cancel transfer' $button
        $item -and !$item.Current.IsEnabled
    }
    [IO.File]::Delete($Gate)
    return $offset
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
$reader = $null
try {
    $reader = Open-PeerTraceReader (Join-Path $peer 'transfer-trace')
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $ssh = (Control @('list')).sessions | Where-Object label -eq 'palette-ssh'
    $remote = $ssh.id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    $ssh = (Control @('list')).sessions | Where-Object id -eq $remote
    Run-Palette 'Split right with Palette PowerShell'
    $localPane = Wait-Until 'local neighboring pane' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','PS ')
    Type-Line '$global:transferProof = $PID; "seeded-" + "local"'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','seeded-local')
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    Run-Palette 'Open files here'
    $null = Wait-Until 'SSH shell directory' { (Find-Element '/shell directory') -and (Find-Element 'retained-download' $button) }

    Click ('Upload' + [char]0x2026)
    $open = Wait-Until 'owned multiple file upload picker' { Find-OwnedDialog 'Choose files to upload' }
    Select-UploadPair $open
    Click-Dialog $open 1
    $offset = Cancel-AtSecondPacket 'WRITE' $writeGate
    $null = Wait-Until 'partial upload status with acknowledged byte count' {
        Find-Element "Cancelled upload of /shell directory/01-first-upload ($($offset * 2) bytes acknowledged). The remote file may be incomplete. 0 of 2 items completed."
    }

    Enable-SftpFileOption 'Replace existing files'
    Start-Download 'retained-download' -Replace
    $offset = Cancel-AtSecondPacket 'READ' $readGate
    $null = Wait-Until 'cancelled download reports its temporary byte count' {
        Find-Element "Cancelled download of /shell directory/retained-download ($offset bytes written to the temporary file)."
    }
    Start-Download 'verified-download'
    $null = Wait-Until 'next native download completes on that pane' {
        Find-Element 'Downloaded /shell directory/verified-download (17 bytes)'
    }
    Enable-SftpFileOption 'Upload files atomically'
    Click ('Upload' + [char]0x2026)
    $open = Wait-Until 'owned atomic upload picker' { Find-OwnedDialog 'Choose files to upload' }
    Select-DialogItem $open 'atomic-note'
    Click-Dialog $open 1
    $null = Wait-Until 'atomic file upload completes in the drawer' {
        Find-Element 'Uploaded atomic-note (14 bytes)'
    }
    Run-Palette 'SFTP files'
    foreach ($c in 'after-cancel'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','after-cancel')
    $active = (Control @('list')).sessions | Where-Object id -eq $remote
    if (!$active.active -or $active.connection_generation -ne $ssh.connection_generation -or $active.tab_id -ne $ssh.tab_id) { throw 'SSH pane or connection changed during cancellation' }
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $localPane
    Type-Line 'if ($transferProof -eq $PID) { "local-still-" + "alive" }'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','local-still-alive')
} catch {
    $wire = if ($reader) { [Text.Encoding]::ASCII.GetString((Read-PeerTrace $reader)) }
    throw "$_ at $($_.ScriptStackTrace). Server packets: $wire"
} finally {
    [IO.File]::Delete($writeGate)
    [IO.File]::Delete($readGate)
    if ($reader) { $reader.Dispose() }
    Close-FixtureApp $app
}

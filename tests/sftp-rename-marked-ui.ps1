[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$peer = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$folder = Join-Path $peer 'shell directory'
$gate = Join-Path $peer 'hold-rename-reply'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$rename = 'Rename marked entries' + [char]0x2026
$on = [System.Windows.Automation.ToggleState]::On
$off = [System.Windows.Automation.ToggleState]::Off

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Drawer-Text([string]$Id) {
    $byId = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $item = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $byId)
    if ($item) { $item.Current.Name -replace '\s+', ' ' }
}
function Review-Rename {
    Click $rename
    foreach ($pair in @(@('Find text in names','old-'),@('Replace with','new-'))) {
        $field = Wait-Until "edit field '$($pair[0])'" { Find-Element $pair[0] $edit }
        $field.GetCurrentPattern($value).SetValue($pair[1])
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Click ('Select entries' + [char]0x2026)
    foreach ($name in 'simple-old-a.txt','simple-old-b.txt') {
        (Wait-CheckBox "Mark $name" $off).Toggle()
        $null = Wait-CheckBox "Mark $name" $on
    }
    Review-Rename
    $arrow = [char]0x2192
    $null = Wait-Until 'both renamed files are in the preview' {
        $preview = Drawer-Text 'sftp-rename-preview'
        $preview -like "*simple-old-a.txt $arrow simple-new-a.txt*" -and
            $preview -like "*simple-old-b.txt $arrow simple-new-b.txt*"
    }
    Click 'Rename'
    $null = Wait-Until 'the occupied destination blocks the entire batch' {
        (Drawer-Text 'sftp-listing-status') -like 'No entries were renamed.*simple-new-b.txt*'
    }
    if ([IO.File]::Exists((Join-Path $folder 'simple-new-a.txt'))) { throw 'The batch renamed a file before preflight passed' }
    if ([IO.File]::ReadAllText((Join-Path $folder 'simple-new-b.txt')) -cne 'occupied') { throw 'The collision was changed' }
    [IO.File]::Delete((Join-Path $folder 'simple-new-b.txt'))
    Review-Rename
    Click 'Rename'
    $null = Wait-Until 'the confirmed names are listed' {
        (Drawer-Text 'sftp-listing-status') -ceq 'Renamed 2 of 2 marked entries' -and
            (Wait-CheckBox 'Mark simple-new-a.txt' $off) -and (Wait-CheckBox 'Mark simple-new-b.txt' $off)
    }
    Click 'Exit selection'

    Click ('Find files' + [char]0x2026)
    $query = Wait-Until 'search query field' { Find-Element 'Filename contains' $edit }
    $query.GetCurrentPattern($value).SetValue('scan-old-')
    Click 'Search'
    Click ('Select results' + [char]0x2026)
    Click 'Mark all results'
    $null = Wait-CheckBox 'Mark /shell directory/nested/scan-old-02.txt' $on
    Review-Rename
    [IO.File]::WriteAllBytes($gate, [byte[]]@())
    Click 'Rename'
    $null = Wait-Until 'first search rename awaits acknowledgement' {
        [IO.File]::Exists((Join-Path $folder 'scan-new-01.txt'))
    }
    Click 'Stop after current rename'
    $null = Wait-Until 'the remaining rename has been stopped' {
        $item = Find-Element ('Stopping after current rename' + [char]0x2026) $button
        $item -and !$item.Current.IsEnabled
    }
    [IO.File]::Delete($gate)
    $null = Wait-Until 'stopped batch reports one rename and refreshes the search' {
        (Drawer-Text 'sftp-search-status') -like 'Renamed 1 of 2 marked entries. Stopped after the current rename.*'
    }
    $null = Wait-CheckBox 'Mark /shell directory/nested/scan-old-02.txt' $on
    if ([IO.File]::Exists((Join-Path $folder 'nested/scan-new-02.txt'))) { throw 'A queued rename ran after Stop' }
} catch {
    $status = if ($window -and !$app.HasExited) {
        Drawer-Text 'sftp-listing-status'
        Drawer-Text 'sftp-search-status'
    }
    throw "$($_.Exception.Message)`nDrawer status: $status`n$($_.ScriptStackTrace)"
} finally {
    [IO.File]::Delete($gate)
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

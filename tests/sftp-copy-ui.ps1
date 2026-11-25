[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$parent = '/shell directory'
$copy = 'Copy to folder' + [char]0x2026
$batchCopy = 'Copy marked files' + [char]0x2026
$on = [System.Windows.Automation.ToggleState]::On
$off = [System.Windows.Automation.ToggleState]::Off
$peerRoot = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$renameGate = Join-Path $peerRoot 'hold-rename-reply'
$searchDestination = Join-Path $peerRoot 'shell directory/scan-copy-01.bin'
$queuedDestination = Join-Path $peerRoot 'shell directory/scan-copy-02.bin'

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Drawer-Status([string]$Id) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $status = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if ($status) { $status.Current.Name -replace '\s+', ' ' }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Click 'archive/'
    $null = Wait-Until 'folders do not offer the regular-file copy action' {
        $action = Find-Element $copy $button
        $action -and !$action.Current.IsEnabled
    }
    Click 'copy-first.bin'
    Click $copy
    $null = Wait-Until 'the copy dialog shows the original file and destination editor' {
        (Find-Element 'Remote copy source') -and (Find-Element 'Destination folder' $edit)
    }
    Click ('Browse folders' + [char]0x2026)
    Click 'Browse folder archive'
    Click 'Use this folder'
    $null = Wait-Until 'the chosen folder is ready to review' {
        $destination = Find-Element 'Destination folder' $edit
        $destination -and $destination.GetCurrentPattern($value).Current.Value -ceq "$parent/archive"
    }
    Click 'Copy'
    $null = Wait-Until 'copy completes while keeping the original listed' {
        (Find-Element "Copied copy-first.bin to $parent/archive") -and (Find-Element 'copy-first.bin' $button)
    }

    Click ('Find files' + [char]0x2026)
    $query = Wait-Until 'recursive filename query' {
        $field = Find-Element 'Filename contains' $edit
        if ($field -and $field.Current.HasKeyboardFocus) { $field }
    }
    $query.GetCurrentPattern($value).SetValue('copy-nested.bin')
    Click 'Search'
    Click "Copy to folder $parent/sub/copy-nested.bin"
    $destination = Wait-Until 'copy from search shows the destination editor' {
        $field = Find-Element 'Destination folder' $edit
        if ($field -and (Find-Element 'Remote copy source')) { $field }
    }
    $destination.GetCurrentPattern($value).SetValue($parent)
    Click 'Copy'
    $null = Wait-Until 'the search refresh finds the original and its copy' {
        (Find-Element "Copied copy-nested.bin to $parent. 2 matches in 3 folders") -and
            (Find-Element "Reveal $parent/sub/copy-nested.bin" $button) -and
            (Find-Element "Reveal $parent/copy-nested.bin" $button)
    }
    Click 'Back to files'
    $null = Wait-Until 'the cached listing contains the copy' { Find-Element 'copy-nested.bin' $button }

    Click ('Select entries' + [char]0x2026)
    (Wait-CheckBox 'Mark archive/' $off).Toggle()
    $null = Wait-CheckBox 'Mark archive/' $on
    $null = Wait-Until 'a folder mark disables batch copy' {
        $item = Find-Element $batchCopy $button
        $item -and !$item.Current.IsEnabled
    }
    Click 'Clear marks'
    foreach ($name in 'batch-01.bin','batch-02.bin','batch-03.bin') {
        (Wait-CheckBox "Mark $name" $off).Toggle()
        $null = Wait-CheckBox "Mark $name" $on
    }
    Click $batchCopy
    $destination = Wait-Until 'the listing review contains all three file names' {
        $field = Find-Element 'Destination folder' $edit
        $names = @('batch-01.bin','batch-02.bin','batch-03.bin') -join [char]10
        if ($field -and (Find-Element $names)) { $field }
    }
    $destination.GetCurrentPattern($value).SetValue("$parent/archive")
    Click 'Copy'
    $null = Wait-Until 'a collision stops the listing batch after one confirmed copy' {
        $status = Drawer-Status 'sftp-listing-status'
        $status -and $status.StartsWith("Copied 1 of 3 marked files to $parent/archive. Stopped at $parent/batch-02.bin.") -and
            $status.Contains("An entry already exists at $parent/archive/batch-02.bin")
    }
    foreach ($name in 'batch-01.bin','batch-02.bin','batch-03.bin') {
        $null = Wait-CheckBox "Mark $name" $on
    }
    Click 'Exit selection'

    Click ('Find files' + [char]0x2026)
    $query = Wait-Until 'batch filename query' {
        $field = Find-Element 'Filename contains' $edit
        if ($field -and $field.Current.HasKeyboardFocus) { $field }
    }
    $query.GetCurrentPattern($value).SetValue('scan-copy-')
    Click 'Search'
    $null = Wait-Until 'both search rows are ready' {
        (Find-Element "Reveal $parent/archive/scan-copy-01.bin" $button) -and
            (Find-Element "Reveal $parent/sub/scan-copy-02.bin" $button)
    }
    Click ('Select results' + [char]0x2026)
    Click 'Mark all results'
    $null = Wait-CheckBox "Mark $parent/archive/scan-copy-01.bin" $on
    $null = Wait-CheckBox "Mark $parent/sub/scan-copy-02.bin" $on
    Click $batchCopy
    $destination = Wait-Until 'the search batch offers the destination editor' {
        $field = Find-Element 'Destination folder' $edit
        if ($field -and (Find-Element "Copy marked files")) { $field }
    }
    $destination.GetCurrentPattern($value).SetValue($parent)
    [IO.File]::WriteAllBytes($renameGate, [byte[]]@())
    Click 'Copy'
    $null = Wait-Until 'the first copy is published and its acknowledgement is held' {
        [IO.File]::Exists($searchDestination)
    }
    Click 'Stop after current copy'
    $null = Wait-Until 'the copy batch is stopping' {
        $stopping = Find-Element ('Stopping after current copy' + [char]0x2026) $button
        $stopping -and !$stopping.Current.IsEnabled
    }
    [IO.File]::Delete($renameGate)
    $null = Wait-Until 'the stopped batch reports one completed copy and refreshes the search' {
        (Drawer-Status 'sftp-search-status') -ceq "Copied 1 of 2 marked files to $parent. Stopped after the current copy. 3 matches in 3 folders"
    }
    if ([IO.File]::Exists($queuedDestination)) { throw 'The queued copy ran after Stop' }
    $null = Wait-CheckBox "Mark $parent/archive/scan-copy-01.bin" $on
    $null = Wait-CheckBox "Mark $parent/sub/scan-copy-02.bin" $on
    $null = Wait-CheckBox "Mark $parent/scan-copy-01.bin" $off
    (Wait-CheckBox "Mark $parent/archive/scan-copy-01.bin" $on).Toggle()
    $null = Wait-CheckBox "Mark $parent/archive/scan-copy-01.bin" $off
    Click $batchCopy
    $destination = Wait-Until 'the remaining search file can be copied in a new batch' {
        Find-Element 'Destination folder' $edit
    }
    $destination.GetCurrentPattern($value).SetValue($parent)
    Click 'Copy'
    $null = Wait-Until 'the new search batch completes' {
        (Drawer-Status 'sftp-search-status') -ceq "Copied 1 of 1 marked file to $parent. 4 matches in 3 folders"
    }
    Click 'Back to files'
    $null = Wait-Until 'the cached listing includes both search copies' {
        (Find-Element 'scan-copy-01.bin' $button) -and (Find-Element 'scan-copy-02.bin' $button)
    }
} catch {
    $status = if ($window -and !$app.HasExited) {
        Drawer-Status 'sftp-listing-status'
        Drawer-Status 'sftp-search-status'
    }
    throw "$($_.Exception.Message)`nDrawer status: $status`n$($_.ScriptStackTrace)"
} finally {
    [IO.File]::Delete($renameGate)
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

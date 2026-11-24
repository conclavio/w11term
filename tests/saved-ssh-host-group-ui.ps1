[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit

function Catalog {
    # ProfileStore atomically replaces the path while this fixture polls it.
    $share = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
    $stream = [IO.File]::Open($profiles, [IO.FileMode]::Open, [IO.FileAccess]::Read, $share)
    $reader = [IO.StreamReader]::new($stream)
    try { $reader.ReadToEnd() | ConvertFrom-Json } finally { $reader.Dispose() }
}
function Click([string]$Name) {
    $action = Wait-Until "enabled '$Name'" {
        $entry = Find-Element $Name $button
        $provider = $null
        if ($entry -and $entry.Current.IsEnabled -and $entry.TryGetCurrentPattern(
                [System.Windows.Automation.InvokePattern]::Pattern, [ref]$provider)) { $provider }
    }
    $action.Invoke()
}
function Name-Group([string]$Name) {
    $value = Wait-Until 'group name editor' {
        $field = Find-Element 'SSH group name' $edit
        $provider = $null
        if ($field -and $field.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider)) { $provider }
    }
    $value.SetValue($Name)
}
function Filter-Roster([string]$Query) {
    $value = Wait-Until 'host filter editor' {
        $field = Find-Element 'Host filter' $edit
        $provider = $null
        if ($field -and $field.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider)) { $provider }
    }
    $value.SetValue($Query)
}
function Filter-GroupProfiles([string]$Query) {
    $field = Wait-Until 'saved group profile search' { Find-Element 'Find group profiles' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Query)
}
function Launch-Window {
    $launchArgs = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','95','--rows','32')
    $script:app = Start-Process -FilePath $Gui -ArgumentList $launchArgs -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
    $script:window = Wait-InputWindow
    $menu = Wait-Until 'tab menu button' { Find-Element 'Tab menu' $button }
    $menu.SetFocus()
}
function Open-GroupPage {
    Click 'Tab menu'
    Invoke-Button (Wait-Until 'SSH host group menu item' {
        Find-Element 'SSH host group' ([System.Windows.Automation.ControlType]::MenuItem)
    })
}
function Open-FleetPalette {
    $command = "Open SSH group${dot}Fleet"
    $field = Open-Palette $command
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('FLEET')
    $null = Wait-Until 'saved group found in the palette' { Find-Element $command $button }
}
function Dismiss-PaletteError([string]$Message, [int]$Count = 1) {
    Click ($Message + '  ' + [char]0xb7 + '  Dismiss')
    if (@((Control @('list')).sessions).Count -ne $Count -or (Find-Element 'Group command' $edit)) {
        throw 'A failed palette launch changed the active surface or opened SSH sessions'
    }
}
function Close-GroupWindow {
    if (!$app) { return }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
    $script:app = $null
}

try {
    Launch-Window
    Open-GroupPage
    Click 'New group'
    Name-Group 'Fleet'
    $null = Wait-Until 'the first catalog page is bounded' { Find-Element "0 selected${dot}32 matches${dot}Page 1 of 2" }
    if (Find-Element 'Group member West') { throw 'The full catalog was mounted on the first page' }
    Click 'Check matches'
    $null = Wait-Until 'checking matches reaches the second page' { Find-Element "32 selected${dot}32 matches${dot}Page 1 of 2" }
    Click 'Uncheck matches'
    $null = Wait-Until 'clearing matches reaches the second page' { Find-Element "0 selected${dot}32 matches${dot}Page 1 of 2" }
    Click 'Next profiles'
    if (Find-Element 'Group member Local witness') { throw 'Local shell was offered as a group member' }
    $null = Wait-CheckBox 'Group member West' Off
    Filter-GroupProfiles 'missing fixture host'
    $null = Wait-Until 'an empty search shows its own status' { Find-Element 'No SSH profiles match this search.' }
    Filter-GroupProfiles 'EAST'
    $null = Wait-Until 'search returns East from the earlier page' {
        (Find-Element 'Group member East') -and !(Find-Element 'Group member West')
    }
    Click 'Check matches'
    $null = Wait-CheckBox 'Group member East' On
    $westernPort = (Catalog).profiles.West.port
    Filter-GroupProfiles "TEST 127.0.0.1:$westernPort"
    $null = Wait-Until 'address search finds West alone' {
        (Find-Element 'Group member West') -and !(Find-Element 'Group member East')
    }
    Click 'Check matches'
    $null = Wait-CheckBox 'Group member West' On
    Click 'Uncheck matches'
    $null = Wait-CheckBox 'Group member West' Off
    $null = Wait-Until 'the hidden member remains checked' { Find-Element "1 selected${dot}1 matches${dot}Page 1 of 1" }
    Click 'Check matches'
    $null = Wait-CheckBox 'Group member West' On
    Click 'Move West up'
    $null = Wait-Until 'West moves to the first position above hidden East' {
        $move = Find-Element 'Move West up' $button
        $move -and !$move.Current.IsEnabled -and (Find-Element 'Position 1')
    }
    Click 'Move West down'
    $null = Wait-Until 'West can move back down' { Find-Element 'Position 2' }
    Click 'Move West up'
    $null = Wait-Until 'West returns to the first position' { Find-Element 'Position 1' }
    Click 'Save group'
    $null = Wait-Until 'both members appear in the saved order' { Find-Element 'West, East' }
    if (((Catalog).ssh_host_groups.Fleet -join ',') -cne 'West,East') { throw 'Both members were not saved in the reviewed order' }
    if (@((Control @('list')).sessions).Count -ne 1) { throw 'Saving opened terminal sessions' }
    $original = [IO.File]::ReadAllBytes($profiles)
    $changed = Catalog
    $changed.profiles.PSObject.Properties.Remove('West')
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Click 'Open group'
    $null = Wait-Until 'the missing profile is reported' {
        Find-Element 'SSH profile West is unavailable for groups'
    }
    if (@((Control @('list')).sessions).Count -ne 1) { throw 'A group with a missing member opened a partial set of sessions' }
    [IO.File]::WriteAllBytes($profiles, $original)
    Close-GroupWindow

    Launch-Window
    Open-FleetPalette
    $changed = Catalog
    $changed.ssh_host_groups.Fleet = @('East')
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Click "Open SSH group${dot}Fleet"
    Dismiss-PaletteError 'The SSH group changed on disk. Select it again.'
    [IO.File]::WriteAllBytes($profiles, $original)
    Open-FleetPalette
    $changed = Catalog
    $changed.profiles.PSObject.Properties.Remove('West')
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Click "Open SSH group${dot}Fleet"
    Dismiss-PaletteError 'SSH profile West is unavailable for groups'
    [IO.File]::WriteAllBytes($profiles, $original)
    Open-FleetPalette
    Click "Open SSH group${dot}Fleet"
    $null = Wait-Until 'the saved group opened in the group tab' { Find-Element 'Opened 2 SSH panes for Fleet' }
    $sessions = Wait-Until 'two group sessions in separate tabs' {
        $current = @((Control @('list')).sessions)
        if ($current.Count -eq 3 -and @($current.tab_id | Sort-Object -Unique).Count -eq 3) { ,$current }
    }
    $tabItem = [System.Windows.Automation.ControlType]::TabItem
    $null = Wait-Until 'reopened sessions follow the saved tab order' {
        $left = Find-Element 'West' $tabItem
        $right = Find-Element 'East' $tabItem
        $left -and $right -and $left.Current.BoundingRectangle.X -lt $right.Current.BoundingRectangle.X
    }
    foreach ($name in 'East','West') {
        $pane = ($sessions | Where-Object label -ceq $name).id
        if (!$pane) { throw "Missing group pane $name" }
        $port = (Catalog).profiles.$name.port
        $null = Wait-CheckBox "$name${dot}test@127.0.0.1:${port}${dot}Pane $pane" On
        $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
    }

    Click 'Edit group'
    $null = Wait-CheckBox 'Group member East' On
    Filter-GroupProfiles 'west'
    (Wait-CheckBox 'Group member West' On).Toggle()
    $null = Wait-CheckBox 'Group member West' Off
    Name-Group 'East only'
    Open-FleetPalette
    Click "Open SSH group${dot}Fleet"
    Dismiss-PaletteError 'Finish editing the SSH group before opening another group.' 3
    $null = Wait-CheckBox 'Group member West' Off
    Click 'Save group'
    $null = Wait-Until 'group editor closes after rename' { Find-Element 'Edit group' $button }
    $groups = (Catalog).ssh_host_groups
    if ($groups.Fleet -or ($groups.'East only' -join ',') -cne 'East') {
        throw 'The renamed group has the wrong saved membership'
    }
    Click 'Edit group'
    Click 'Delete group'
    Click 'Confirm delete group'
    $null = Wait-Until 'group editor closes after delete' { Find-Element 'New group' $button }
    $saved = Catalog
    if ($saved.ssh_host_groups -or !$saved.profiles.East -or !$saved.profiles.West -or $saved.other -ne 17) {
        throw 'The deletion changed unrelated profile data or left the group behind'
    }
    if (@((Control @('list')).sessions).Count -ne 3) { throw 'Editing or deleting the group changed its running sessions' }

    $saveSelection = 'Save selection as group' + [char]0x2026
    $eastPane = ($sessions | Where-Object label -ceq 'East').id
    $westPane = ($sessions | Where-Object label -ceq 'West').id
    $withProfiles = [IO.File]::ReadAllBytes($profiles)
    $changed = Catalog
    $changed.profiles.PSObject.Properties.Remove('East')
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Click $saveSelection
    $null = Wait-CheckBox 'Group member West' On
    $null = Wait-Until 'a selected pane has lost its saved profile' {
        Find-Element '1 selected pane has no available saved SSH profile.'
    }
    Click 'Cancel'
    $null = Wait-Until 'selection draft canceled' { Find-Element $saveSelection $button }
    if ((Catalog).ssh_host_groups) { throw 'Canceling the selection draft saved a group' }
    [IO.File]::WriteAllBytes($profiles, $withProfiles)
    Filter-Roster 'west'
    $null = Wait-Until 'East is hidden from the selectable roster' {
        (Find-Element "Focus pane $westPane" $button) -and !(Find-Element "Focus pane $eastPane" $button)
    }
    Click $saveSelection
    $null = Wait-CheckBox 'Group member West' On
    Filter-GroupProfiles 'east'
    $null = Wait-CheckBox 'Group member East' Off
    Name-Group 'Selected West'
    Click 'Save group'
    $null = Wait-Until 'new group saved from the roster' { Find-Element 'SSH group saved' }
    if (((Catalog).ssh_host_groups.'Selected West' -join ',') -cne 'West' -or
        @((Control @('list')).sessions).Count -ne 3) {
        throw 'Saving the visible selection changed the running panes or saved the wrong membership'
    }
    Filter-Roster ''
    foreach ($name in 'East','West') {
        $pane = ($sessions | Where-Object label -ceq $name).id
        $port = (Catalog).profiles.$name.port
        $null = Wait-CheckBox "$name${dot}test@127.0.0.1:${port}${dot}Pane $pane" On
    }
} catch {
    $failure = $_
    $groups = (Catalog).ssh_host_groups | ConvertTo-Json -Depth 4 -Compress
    $messages = if ($app -and !$app.HasExited) {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $filter = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Text)
        $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $filter) |
            ForEach-Object { $_.Current.Name }
    }
    throw "$($failure.Exception.Message)`nGroups: $groups`nWindow text: $($messages -join ', ')`n$($failure.ScriptStackTrace)"
} finally {
    Close-GroupWindow
}

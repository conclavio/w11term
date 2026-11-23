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
function Launch-GroupWindow {
    $launchArgs = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','95','--rows','32')
    $script:app = Start-Process -FilePath $Gui -ArgumentList $launchArgs -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
    $script:window = Wait-InputWindow
    Click 'Tab menu'
    Invoke-Button (Wait-Until 'SSH host group menu item' {
        Find-Element 'SSH host group' ([System.Windows.Automation.ControlType]::MenuItem)
    })
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
    Launch-GroupWindow
    Click 'New group'
    Name-Group 'Fleet'
    if (Find-Element 'Group member Local witness') { throw 'Local shell was offered as a group member' }
    (Wait-CheckBox 'Group member East' Off).Toggle()
    $null = Wait-CheckBox 'Group member East' On
    (Wait-CheckBox 'Group member West' Off).Toggle()
    $null = Wait-CheckBox 'Group member West' On
    Click 'Save group'
    $null = Wait-Until 'both saved members on the group page' { Find-Element 'East, West' }
    if (((Catalog).ssh_host_groups.Fleet -join ',') -cne 'East,West') { throw 'Both members were not saved' }
    if (@((Control @('list')).sessions).Count -ne 1) { throw 'Saving opened terminal sessions' }
    Close-GroupWindow

    Launch-GroupWindow
    $null = Wait-Until 'group loaded from disk' { Find-Element 'East, West' }
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
    Click 'Open group'
    $sessions = Wait-Until 'two group sessions in separate tabs' {
        $current = @((Control @('list')).sessions)
        if ($current.Count -eq 3 -and @($current.tab_id | Sort-Object -Unique).Count -eq 3) { ,$current }
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
    (Wait-CheckBox 'Group member West' On).Toggle()
    $null = Wait-CheckBox 'Group member West' Off
    Name-Group 'East only'
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

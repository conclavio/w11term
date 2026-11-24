[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:USERPROFILE = Join-Path $State 'ssh-home'
$null = [IO.Directory]::CreateDirectory((Join-Path $env:USERPROFILE '.ssh'))
$profiles = Join-Path $State 'profiles.json'
$before = [IO.File]::ReadAllText($profiles)
$catalog = $before | ConvertFrom-Json
$east = "test@127.0.0.1:$($catalog.profiles.East.port)"
$west = "test@127.0.0.1:$($catalog.profiles.West.port)"
$port = [IO.File]::ReadAllText((Join-Path $State 'one-time-port'))
$direct = "test@127.0.0.1:$port"
$other = "test@127.0.0.2:$port"
$env:W11TERM_HOST_LIST_SMOKE_DIR = Join-Path $State 'host-lists'
$null = [IO.Directory]::CreateDirectory($env:W11TERM_HOST_LIST_SMOKE_DIR)
$fleetText = @('# fleet', 'West # saved profile', "ssh://$west", $east, 'East', "127.<0>.<0>.<2-1,2>:$port # inventory") -join "`n"
[IO.File]::WriteAllText((Join-Path $env:W11TERM_HOST_LIST_SMOKE_DIR 'fleet-hosts'), $fleetText, [Text.UTF8Encoding]::new($true))
[IO.File]::WriteAllBytes((Join-Path $env:W11TERM_HOST_LIST_SMOKE_DIR 'bad-hosts'), [byte[]]@(0x61, 0, 0x62))
$label = 'Import host list' + [char]0x2026
$openFile = 'Open host list file' + [char]0x2026
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$combo = [System.Windows.Automation.ControlType]::ComboBox
$menu = [System.Windows.Automation.ControlType]::MenuItem

function Click([string]$Name, $Kind = $button) {
    $action = Wait-Until "enabled '$Name'" {
        $entry = Find-Element $Name $Kind
        $provider = $null
        if ($entry -and $entry.Current.IsEnabled -and $entry.TryGetCurrentPattern(
                [System.Windows.Automation.InvokePattern]::Pattern, [ref]$provider)) { $provider }
    }
    $action.Invoke()
}
function Text-Field([string]$Name) {
    Wait-Until "editable '$Name'" {
        $field = Find-Element $Name $edit
        $provider = $null
        if ($field -and $field.Current.IsEnabled -and $field.TryGetCurrentPattern($value, [ref]$provider)) { $provider }
    }
}
function Keyless {
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $selection = [System.Windows.Automation.SelectionPattern]::Pattern
    $fixtureName = 'Authentication for new hosts'
    $entry = Wait-Until 'new host authentication' { Find-Element $fixtureName $combo }
    $entry.GetCurrentPattern($expand).Expand()
    $item = Wait-Until 'visible keyless choice' {
        $choice = Find-Element 'None (keyless server)' ([System.Windows.Automation.ControlType]::ListItem)
        if ($choice -and !$choice.Current.IsOffscreen) { $choice }
    }
    $item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'keyless authentication selected' {
        $current = Find-Element $fixtureName $combo
        if ($current) {
            $selected = $current.GetCurrentPattern($selection).Current.GetSelection()
            $selected.Count -eq 1 -and $selected[0].Current.Name -ceq 'None (keyless server)'
        }
    }
    $popup = (Find-Element $fixtureName $combo).GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}
function Assert-PreviewOnly {
    if (@((Control @('list')).sessions).Count -ne 1) {
        throw 'SSH sessions opened before confirmation'
    }
}
function Open-HostListFile([string]$Name) {
    Click $openFile
    $dialog = Wait-Until 'owned host list file picker' { Find-OwnedDialog 'Open SSH host list' }
    if ($Name) { Select-DialogItem $dialog $Name }
    $buttonId = if ($Name) { 1 } else { 2 }
    $null = Wait-Until 'close the host list file picker' { [FixtureDialog]::ClickButton($dialog, $buttonId) }
    $null = Wait-Until 'host list picker dismissed' { !(Find-OwnedDialog 'Open SSH host list') }
}
function Expect-HostList([string]$Expected) {
    $null = Wait-Until 'host list editor contains the expected text' {
        $text = (Text-Field 'SSH host list').Current.Value
        $text.Replace("`r`n", "`n").Replace("`r", "`n") -ceq $Expected
    }
}
function Expect-Connection([string]$HostLabel, [string]$Choice) {
    $null = Wait-Until "'$HostLabel' chooses '$Choice'" {
        $picker = Find-Element "Connection for $HostLabel" $combo
        if ($picker) {
            $selected = $picker.GetCurrentPattern(
                [System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
            $selected.Count -eq 1 -and $selected[0].Current.Name -ceq $Choice
        }
    }
}

$launch = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    (Wait-Until 'tab menu' { Find-Element 'Tab menu' $button }).SetFocus()
    Click 'Tab menu'
    Click 'SSH host group' $menu
    Click $label
    (Text-Field 'Default SSH user').SetValue('test')
    $typed = 'test@keep.example:22'
    (Text-Field 'SSH host list').SetValue($typed)
    Open-HostListFile
    Expect-HostList $typed
    Open-HostListFile 'bad-hosts'
    $errorText = 'Could not open the host list. Choose a UTF-8 or BOM-marked UTF-16 text file.'
    $null = Wait-Until 'invalid host list is reported' { Find-Element $errorText }
    Expect-HostList $typed
    Open-HostListFile 'fleet-hosts'
    Expect-HostList $fleetText
    $null = Wait-Until 'loading clears the file error' { !(Find-Element $errorText) }
    if ((Text-Field 'Default SSH user').Current.Value -cne 'test') { throw 'Loading changed the default SSH user' }
    Assert-PreviewOnly
    Keyless
    Press-Key 0x77 # The first tab is the original demo session.
    $null = Wait-Until 'host list editor is hidden on the other tab' { !(Find-Element 'SSH host list') }
    Run-Palette 'SSH host group'
    Click 'Preview hosts'
    $null = Wait-Until 'host review and diagnostics' {
        (Find-Element 'Review SSH hosts') -and
            (Find-Element "Line 3${dot}Duplicate $west${dot}first listed on line 2") -and
            (Find-Element "Line 4${dot}Multiple SSH profiles match${dot}use an exact profile name (East, East alternative)") -and
            (Find-Element "Line 6${dot}Duplicate $other${dot}first listed on line 6")
    }
    $eastChoice = "East${dot}$east"
    $westChoice = "West${dot}$west"
    $directChoice = "$direct${dot}One-time connection"
    $otherChoice = "$other${dot}One-time connection"
    foreach ($name in $eastChoice,$westChoice,$directChoice,$otherChoice) {
        $null = Wait-CheckBox $name On
    }
    Assert-PreviewOnly
    $catalog.profiles.West.port = $catalog.profiles.East.port
    [IO.File]::WriteAllText($profiles, ($catalog | ConvertTo-Json -Depth 30))
    Click 'Open 4 checked hosts'
    $null = Wait-Until 'a changed profile invalidates the reviewed list' {
        Find-Element 'SSH profiles changed. Preview the host list again.'
    }
    Assert-PreviewOnly
    [IO.File]::WriteAllText($profiles, $before)
    Click 'Preview hosts'
    (Wait-CheckBox $eastChoice On).Toggle()
    (Wait-CheckBox $otherChoice On).Toggle()
    $null = Wait-CheckBox $eastChoice Off
    $null = Wait-CheckBox $otherChoice Off
    Click 'Open 2 checked hosts'
    $null = Wait-Until 'reviewed set becomes the live selection' { Find-Element 'Opened 2 SSH panes from the host list' }
    $sessions = @((Control @('list')).sessions)
    $savedPane = ($sessions | Where-Object label -ceq 'West').id
    $directPane = ($sessions | Where-Object label -ceq $direct).id
    if ($sessions.Count -ne 3 -or !$savedPane -or !$directPane -or $savedPane -eq $directPane) {
        throw 'The import did not create exactly the two checked sessions'
    }
    $null = Wait-CheckBox "West${dot}$west${dot}Pane $savedPane" On
    $null = Wait-CheckBox "$direct${dot}$direct${dot}Pane $directPane" On
    $null = Control @('wait-for-text','--pane',"$savedPane",'--timeout','15','ready')
    Click "Focus pane $directPane"
    Click 'Accept and save'
    $null = Control @('wait-for-text','--pane',"$directPane",'--timeout','15','ready')
    $originalDirect = (Control @('list')).sessions | Where-Object id -eq $directPane

    Run-Palette 'SSH host group'
    Click $label
    (Text-Field 'SSH host list').SetValue((@('West', $direct, 'East') -join [Environment]::NewLine))
    Keyless
    Click 'Preview hosts'
    $null = Wait-Until 'import leaves matching connections alone by default' {
        Find-Element "0 to reuse${dot}3 to open"
    }
    Expect-Connection $westChoice 'Open a new SSH pane'
    Expect-Connection $directChoice 'Open a new SSH pane'
    Click 'Reuse unique matches'
    Expect-Connection $westChoice "Reuse${dot}West${dot}$west${dot}Pane $savedPane"
    Expect-Connection $directChoice "Reuse${dot}$direct${dot}$direct${dot}Pane $directPane"
    $null = Wait-Until 'mixed import review counts both existing panes' { Find-Element "2 to reuse${dot}1 to open" }
    $tabType = [System.Windows.Automation.ControlType]::TabItem
    $westTab = Wait-Until 'existing West SSH tab' { Find-Element 'West' $tabType }
    $westTab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'existing West SSH session is active' {
        ((Control @('list')).sessions | Where-Object id -eq $savedPane).active
    }
    Run-Palette 'Enable read-only pane input'
    $null = Wait-Until 'the reviewed West pane becomes read-only' {
        ((Control @('list')).sessions | Where-Object id -eq $savedPane).read_only
    }
    Run-Palette 'SSH host group'
    Click 'Use 3 checked hosts'
    $null = Wait-Until 'a stale reused pane aborts the whole import' {
        Find-Element "The reviewed SSH connection for $westChoice changed. Choose a new pane or preview the host list again."
    }
    if (@((Control @('list')).sessions).Count -ne 3) { throw 'Stale reuse opened a fresh connection' }
    (Wait-CheckBox $westChoice On).Toggle()
    $null = Wait-CheckBox $westChoice Off
    Click 'Use 2 checked hosts'
    $null = Wait-Until 'mixed launch is reported' { Find-Element "Reused 1${dot}Opened 1 from the host list" }
    $sessions = @((Control @('list')).sessions)
    $directAgain = $sessions | Where-Object id -eq $directPane
    $eastPane = ($sessions | Where-Object label -ceq 'East').id
    if ($sessions.Count -ne 4 -or !$eastPane -or !$directAgain -or
            $directAgain.connection_generation -ne $originalDirect.connection_generation) {
        throw 'The mixed import did not preserve the approved session and open exactly one fresh pane'
    }
    $null = Wait-CheckBox "East${dot}$east${dot}Pane $eastPane" On
    $null = Wait-CheckBox "$direct${dot}$direct${dot}Pane $directPane" On
    $null = Wait-CheckBox "West${dot}$west${dot}Pane $savedPane" Off
    $null = Control @('wait-for-text','--pane',"$eastPane",'--timeout','15','ready')
    if ([IO.File]::ReadAllText($profiles) -cne $before) { throw 'Import changed the saved SSH profiles' }

    Click $label
    (Text-Field 'SSH host list').SetValue((@('West', $direct) -join [Environment]::NewLine))
    Keyless
    Click 'Preview hosts'
    $null = Wait-CheckBox $westChoice On
    $null = Wait-CheckBox $directChoice On
    Click ('Save checked hosts as group' + [char]0x2026)
    (Text-Field 'Imported SSH group name').SetValue('Fleet from file')
    $profileField = "Profile name for $direct"
    $null = Wait-Until 'the new profile defaults to its reviewed address' {
        (Text-Field $profileField).Current.Value -ceq $direct
    }
    $null = Wait-Until 'West is reviewed as an existing profile' { Find-Element "Use saved profile West${dot}$west" }
    (Text-Field $profileField).SetValue('West')
    Click 'Save SSH group'
    $null = Wait-Until 'a profile name collision refuses the whole save' { Find-Element 'Profile West already exists' }
    if ([IO.File]::ReadAllText($profiles) -cne $before) { throw 'A name collision partially saved the host group' }

    (Text-Field $profileField).SetValue('Imported relay')
    $appeared = $before | ConvertFrom-Json
    $appeared.profiles | Add-Member -NotePropertyName 'Already saved relay' -NotePropertyValue @{
        host = '127.0.0.1'; user = 'test'; port = [int]$port; auth = 'none'
    }
    $concurrent = $appeared | ConvertTo-Json -Depth 30
    [IO.File]::WriteAllText($profiles, $concurrent)
    Click 'Save SSH group'
    $null = Wait-Until 'a new profile match invalidates the saved review' {
        Find-Element 'SSH profiles changed. Preview the host list again.'
    }
    if ([IO.File]::ReadAllText($profiles) -cne $concurrent) { throw 'A stale import changed the profiles catalog' }
    [IO.File]::WriteAllText($profiles, $before)
    Click 'Save SSH group'
    $null = Wait-Until 'the reviewed host group is saved' { Find-Element 'Saved SSH group Fleet from file' }
    $saved = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
    $relay = $saved.profiles.'Imported relay'
    $originalWest = ($before | ConvertFrom-Json).profiles.West
    if (($saved.ssh_host_groups.'Fleet from file' -join ',') -cne 'West,Imported relay' -or
            $relay.host -cne '127.0.0.1' -or $relay.user -cne 'test' -or $relay.port -ne [int]$port -or
            $relay.auth -cne 'none' -or $saved.profiles.West.port -ne $originalWest.port) {
        throw 'The saved group or reviewed profile differs from the import'
    }
    $null = Wait-Until 'the saved group is available in the chooser' {
        $picker = Find-Element 'Saved SSH groups' $combo
        if ($picker) {
            $selection = $picker.GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
            $selection.Count -eq 1 -and $selection[0].Current.Name -ceq 'Fleet from file'
        }
    }
    $current = (Control @('list')).sessions
    if ((($current.id | Sort-Object) -join ',') -cne (($sessions.id | Sort-Object) -join ',')) {
        throw 'Saving the reviewed group opened or closed SSH panes'
    }
} catch {
    $failure = $_
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $rows = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) |
        ForEach-Object { $_.Current.Name } | Where-Object { $_ } | Select-Object -First 45)
    throw "$($failure.Exception.Message)`n$($failure.ScriptStackTrace)`nVisible controls: $($rows -join ' | ')"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

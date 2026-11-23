[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
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
$label = 'Import host list' + [char]0x2026
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

$launch = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    (Wait-Until 'tab menu' { Find-Element 'Tab menu' $button }).SetFocus()
    Click 'Tab menu'
    Click 'SSH host group' $menu
    Click $label
    (Text-Field 'Default SSH user').SetValue('test')
    $input = @('# fleet', 'West', "ssh://$west", $east, 'East', "127.0.0.<1-2>:$port") -join [Environment]::NewLine
    (Text-Field 'SSH host list').SetValue($input)
    Keyless
    Press-Key 0x77 # The first tab is the original demo session.
    $null = Wait-Until 'host list editor is hidden on the other tab' { !(Find-Element 'SSH host list') }
    Run-Palette 'SSH host group'
    Click 'Preview hosts'
    $null = Wait-Until 'host review and diagnostics' {
        (Find-Element 'Review SSH hosts') -and
            (Find-Element "Line 3${dot}Duplicate $west${dot}first listed on line 2") -and
            (Find-Element "Line 4${dot}Multiple SSH profiles match${dot}use an exact profile name (East, East alternative)")
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
    if ([IO.File]::ReadAllText($profiles) -cne $before) { throw 'Import changed the saved SSH profiles' }
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

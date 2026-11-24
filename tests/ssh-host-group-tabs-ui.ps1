[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$config = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button
$tabItem = [System.Windows.Automation.ControlType]::TabItem
$edit = [System.Windows.Automation.ControlType]::Edit
$comboBox = [System.Windows.Automation.ControlType]::ComboBox
$listItem = [System.Windows.Automation.ControlType]::ListItem
$selectionItem = [System.Windows.Automation.SelectionItemPattern]::Pattern

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $entry = Find-Element $Name $button
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
}
function Field([string]$Name) {
    Wait-Until "editable '$Name'" {
        $entry = Find-Element $Name $edit
        $provider = $null
        if ($entry -and $entry.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider)) { $provider }
    }
}
function Expect-Field([string]$Name, [string]$Value) {
    $null = Wait-Until "$Name is '$Value'" {
        ((Field $Name).Current.Value -ceq $Value)
    }
}
function Wait-Tab([string]$Name) {
    $null = Wait-Until "tab '$Name' is selected" {
        $tab = Find-Element $Name $tabItem
        $tab -and $tab.GetCurrentPattern($selectionItem).Current.IsSelected
    }
}
function Select-Tab([string]$Name) {
    $tab = Wait-Until "tab '$Name'" { Find-Element $Name $tabItem }
    $tab.GetCurrentPattern($selectionItem).Select()
    Wait-Tab $Name
}
function Choose-SavedGroup([string]$Name) {
    $combo = Wait-Until 'saved SSH group chooser' { Find-Element 'Saved SSH groups' $comboBox }
    $popup = $combo.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
    $popup.Expand()
    $item = Wait-Until "'$Name' in the group chooser" {
        $entry = Find-Element $Name $listItem
        if ($entry -and !$entry.Current.IsOffscreen) { $entry }
    }
    $item.GetCurrentPattern($selectionItem).Select()
    $null = Wait-Until "'$Name' is selected in the group chooser" {
        $selection = (Find-Element 'Saved SSH groups' $comboBox).GetCurrentPattern(
            [System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
        $selection.Count -eq 1 -and $selection[0].Current.Name -ceq $Name
    }
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}
function Session([string]$Name) {
    Wait-Until "SSH pane '$Name'" { (Control @('list')).sessions | Where-Object label -ceq $Name }
}
function Target-Label([string]$HostName, [uint64]$Pane) {
    $port = $config.$HostName.port
    "$HostName${dot}test@127.0.0.1:${port}${dot}Pane $Pane"
}
function Submit([string]$Command) {
    (Field 'Group command').SetValue($Command)
    Click 'Send to 1 host'
    $null = Wait-Until "'$Command' was submitted" { Find-Element "Submitted${dot}$Command" }
}

$argsForApp = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','110','--rows','34')
$app = Start-Process -FilePath $Gui -ArgumentList $argsForApp -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $eastTab = "SSH group${dot}Fleet East"
    $westTab = "SSH group${dot}Fleet West"
    Run-Palette "Open SSH group${dot}Fleet East"
    Wait-Tab $eastTab
    $east = (Session 'East').id
    $eastName = Target-Label 'East' $east
    $null = Wait-CheckBox $eastName On
    $null = Wait-Until 'East is ready' { Find-Element "Pane $east${dot}Connected${dot}Ready" }
    Submit 'east-probe'
    $null = Wait-Until 'East command stays running across the tab switch' { Find-Element "$eastName${dot}Running" }
    (Field 'Group command').SetValue('east-only draft')
    (Field 'Host filter').SetValue('east')
    Expect-Field 'Group command' 'east-only draft'
    Expect-Field 'Host filter' 'east'

    Choose-SavedGroup 'Fleet West'
    Click 'Open in new tab'
    Wait-Tab $westTab
    $west = (Session 'West').id
    $westName = Target-Label 'West' $west
    $null = Wait-CheckBox $westName On
    if (Find-Element "Focus pane $east" $button) { throw 'East leaked into the West roster' }
    Expect-Field 'Group command' ''
    Expect-Field 'Host filter' ''
    $null = Wait-Until 'West is ready' { Find-Element "Pane $west${dot}Connected${dot}Ready" }
    Submit 'west-probe'
    $null = Wait-Until 'only the West submission result is shown' {
        (Find-Element "$westName${dot}Exit 0") -and !(Find-Element "Submitted${dot}east-probe")
    }

    Click "Focus result pane $west"
    Wait-Focus $west
    Run-Palette 'SSH host group'
    Wait-Tab $westTab
    (Wait-CheckBox 'Show all window SSH panes' Off).Toggle()
    (Wait-CheckBox $eastName Off).Toggle()
    $null = Wait-CheckBox $eastName On
    (Wait-CheckBox 'Show all window SSH panes' On).Toggle()
    $null = Wait-Until 'the East pane leaves the West scope' { !(Find-Element "Focus pane $east" $button) }
    (Wait-CheckBox 'Show all window SSH panes' Off).Toggle()
    $null = Wait-CheckBox $eastName Off
    $null = Wait-CheckBox $westName On
    (Wait-CheckBox 'Show all window SSH panes' On).Toggle()

    [IO.File]::WriteAllText((Join-Path $State 'release-east'), '')
    $null = Control @('wait-for-text','--pane',"$east",'--timeout','15','East finished east-probe')
    Select-Tab $eastTab
    Expect-Field 'Group command' 'east-only draft'
    Expect-Field 'Host filter' 'east'
    $null = Wait-CheckBox $eastName On
    $null = Wait-Until 'East completed while the tabs remained independent' {
        (Find-Element "Submitted${dot}east-probe") -and (Find-Element "$eastName${dot}Exit 0") -and
            !(Find-Element "Submitted${dot}west-probe")
    }

    Run-Palette 'New SSH group tab'
    Wait-Tab 'SSH host group'
    $null = Wait-CheckBox $eastName Off
    $null = Wait-CheckBox $westName Off
    Expect-Field 'Group command' ''
    if (@((Control @('list')).sessions).Count -ne 3) { throw 'Changing group tabs started or closed an SSH pane' }
    Select-Tab $westTab
    $null = Wait-CheckBox $westName On
    $null = Wait-Until 'West retains its own results and narrow scope' {
        (Find-Element "Submitted${dot}west-probe") -and !(Find-Element "Focus pane $east" $button)
    }
} catch {
    throw ($_.Exception.Message + [Environment]::NewLine + $_.ScriptStackTrace)
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

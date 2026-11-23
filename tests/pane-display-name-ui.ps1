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
$tab = [System.Windows.Automation.ControlType]::TabItem
$value = [System.Windows.Automation.ValuePattern]::Pattern
$selected = [System.Windows.Automation.SelectionItemPattern]::Pattern
$paneRename = 'Rename focused pane' + [char]0x2026
$tabRename = 'Rename tab' + [char]0x2026
$workspaces = 'Workspaces' + [char]0x2026
$alias = 'Deploy ' + [char]0x96ea
$shared = 'shared-application'

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled button '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Menu-Route([string[]]$Path) {
    Click 'Tab menu'
    $menu = [System.Windows.Automation.ControlType]::MenuItem
    foreach ($name in $Path) {
        $item = Wait-Until "menu item '$name'" { Find-Element $name $menu }
        if ($name -ceq $Path[-1]) { Invoke-Button $item }
        else { $item.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand() }
    }
}
function Open-Editor([string]$Action, [string]$Label, [switch]$Palette) {
    if ($Palette) { Run-Palette $Action }
    elseif ($Action -ceq $paneRename) { Menu-Route @('Panes',$Action) }
    else { Menu-Route @($Action) }
    (Wait-Until "$Label edit field" { Find-Element $Label $edit }).GetCurrentPattern($value)
}
function Finish-Editor([string]$Action, [string]$Label, [int]$Pane) {
    Click $Action
    $null = Wait-Until "closed $Label dialog" { !(Find-Element $Label $edit) }
    # The native dialog may disappear before its Closed callback reaches the host.
    Document-Name $Pane
}
function Caption([string]$Name) {
    $expected = 'w11term ' + [char]0xb7 + " $Name"
    $null = Wait-Until "selected tab and window caption '$Name'" {
        $app.Refresh()
        $header = Find-Element $Name $tab
        $app.MainWindowTitle -ceq $expected -and $header -and $header.GetCurrentPattern($selected).Current.IsSelected
    }
}
function Document-Name([int]$Pane, [string]$Name) {
    Wait-Focus $Pane
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $id = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, "w11term-pane-$Pane")
    $expected = 'Terminal ' + [char]0xb7 + " $Name"
    $null = Wait-Until "accessible Document for pane $Pane" {
        $doc = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $id)
        $doc -and $doc.Current.ControlType -eq [System.Windows.Automation.ControlType]::Document -and (!$Name -or $doc.Current.Name -ceq $expected)
    }
}
function Marker([int]$Pane, [int]$Neighbor, [string]$Text) {
    Wait-Focus $Pane
    foreach ($ch in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','10',$Text)
    if ((Control @('get-text','--pane',"$Neighbor")).text.Contains($Text)) { throw "Input also reached pane $Neighbor" }
}
function Wait-Names([int]$Named, [int]$Other, [string]$Custom, [string]$RawTitle) {
    $null = Wait-Until "raw and custom names on panes $Named and $Other" {
        $panes = (Control @('list')).sessions
        $a = $panes | Where-Object id -eq $Named
        $b = $panes | Where-Object id -eq $Other
        if (!$a -or !$b -or $a.title -cne $RawTitle -or $b.title -cne $shared -or
            $a.label -cne 'palette-ssh' -or $b.label -cne 'palette-ssh' -or
            $b.PSObject.Properties.Name -contains 'pane_name') { return $false }
        if ($Custom) { return $a.pane_name -ceq $Custom }
        return $a.PSObject.Properties.Name -notcontains 'pane_name'
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $origin = (Control @('list')).sessions[0]
    $left = $origin.id
    $null = Control @('wait-for-text','--pane',"$left",'--timeout','15','ready')
    Run-Palette 'Split right with palette-ssh'
    $right = Wait-Until 'second SSH split on original tab' {
        $p = (Control @('list')).sessions | Where-Object { $_.active -and $_.id -ne $left -and $_.tab_id -eq $origin.tab_id }
        if ($p) { $p.id }
    }
    Wait-Names $right $left '' $shared
    Caption 'palette-ssh'

    $field = Open-Editor $paneRename 'Pane name' -Palette
    $field.SetValue($alias)
    Finish-Editor 'Save' 'Pane name' $right
    Wait-Names $right $left $alias $shared
    Caption $alias
    Document-Name $right $alias
    Menu-Route @('Panes','Focus left    Ctrl+Alt+Left')
    Wait-Focus $left
    Caption 'palette-ssh'
    Open-PickerQuery $shared
    $null = Wait-Until 'both matching OSC titles, with only the renamed pane led by its alias' {
        $rows = @(Get-PickerRows)
        $namedRow = @($rows | Where-Object { $_.StartsWith($alias) -and $_.Contains($shared) -and $_.Contains('palette-ssh') })
        $rows.Count -eq 2 -and $namedRow.Count -eq 1
    }
    Press-Key 27
    Wait-Focus $left
    Picker-Query $alias
    Press-Key 13
    Marker $right $left 'named-input/'
    Caption $alias

    $osc = [char]27 + ']2;changed-by-shell' + [char]7
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($osc))
    $null = Control @('send-text','--pane',"$right",'--no-paste','--base64',$encoded)
    Wait-Names $right $left $alias 'changed-by-shell'
    Caption $alias
    if ((Control @('get-text','--pane',"$right")).title -cne 'changed-by-shell') { throw 'get-text lost the application title' }

    $field = Open-Editor $tabRename 'Tab name'
    $field.SetValue('Manual tab')
    Finish-Editor 'Save' 'Tab name' $right
    Caption 'Manual tab'
    Wait-Names $right $left $alias 'changed-by-shell'
    $null = Open-Editor $tabRename 'Tab name'
    Finish-Editor 'Use default' 'Tab name' $right
    Caption $alias

    Menu-Route @($workspaces)
    $workspaceName = Wait-Until 'workspace name' { Find-Element 'Workspace name' $edit }
    $workspaceName.GetCurrentPattern($value).SetValue('pane-names')
    Click 'Save workspace'
    $null = Wait-Until 'workspace saved' { Find-Element 'Saved pane-names' }
    $catalog = [IO.File]::ReadAllText((Join-Path $env:W11TERM_DATA_DIR 'workspaces.json')) | ConvertFrom-Json
    $saved = $catalog.workspaces.'pane-names'.tabs[0]
    if ($saved.layout.second.pane_name -cne $alias -or $saved.layout.first.PSObject.Properties.Name -contains 'pane_name' -or
        $saved.title -or $saved.focused -ne $saved.layout.second.slot) { throw 'Pane name or focus serialized on the wrong workspace leaf' }

    Picker-Query $alias
    Press-Key 13
    Wait-Focus $right
    $field = Open-Editor $paneRename 'Pane name'
    if ($field.Current.Value -cne $alias) { throw 'The editor did not load the saved pane name' }
    $field.SetValue('Discarded')
    Finish-Editor 'Cancel' 'Pane name' $right
    Wait-Names $right $left $alias 'changed-by-shell'
    $field = Open-Editor $paneRename 'Pane name'
    if ($field.Current.Value -cne $alias) { throw 'Cancelling persisted the pending name' }
    Finish-Editor 'Use default' 'Pane name' $right
    Wait-Names $right $left '' 'changed-by-shell'
    Caption 'palette-ssh'
    Document-Name $right 'palette-ssh'
    Open-PickerQuery $alias
    $null = Wait-Until 'reset removes the alias from pane search' { (Find-Element 'No matching panes') -and @(Get-PickerRows).Count -eq 0 }
    Press-Key 27
    Wait-Focus $right

    Menu-Route @($workspaces)
    Click 'Open workspace pane-names in new tabs'
    $restored = Wait-Until 'restored two live SSH panes with fresh IDs in a new tab' {
        $new = @((Control @('list')).sessions | Where-Object { $_.id -ne $left -and $_.id -ne $right })
        if ($new.Count -eq 2 -and $new[0].tab_id -eq $new[1].tab_id -and $new[0].tab_id -ne $origin.tab_id) { $new }
    }
    $restoredRight = ($restored | Where-Object pane_name -ceq $alias).id
    $restoredLeft = ($restored | Where-Object id -ne $restoredRight).id
    Wait-Names $restoredRight $restoredLeft $alias $shared
    Caption $alias
    Marker $restoredRight $restoredLeft 'restored-input/'
    Wait-Names $right $left '' 'changed-by-shell'
} catch {
    $panes = try { (Control @('list')).sessions | Select-Object id,label,title,pane_name,tab_id,tab_title,active | ConvertTo-Json -Compress } catch {}
    throw "$_ at $($_.ScriptStackTrace). Panes: $panes"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

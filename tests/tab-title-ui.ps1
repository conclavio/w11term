# Exercise the native dialog in the mixed, named and unnamed workspace fixture.
[CmdletBinding()]
param([Parameter(Mandatory)][int]$ProcessId, [Parameter(Mandatory)][string]$Cli)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')

$app = Get-Process -Id $ProcessId
$window = $app.MainWindowHandle
$tabType = [System.Windows.Automation.ControlType]::TabItem
$buttonType = [System.Windows.Automation.ControlType]::Button
$menuItemType = [System.Windows.Automation.ControlType]::MenuItem
$editType = [System.Windows.Automation.ControlType]::Edit
$valuePattern = [System.Windows.Automation.ValuePattern]::Pattern

function Open-TitleEditor([string]$Stage) {
    $rename = 'Rename tab' + [char]0x2026
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' $buttonType -Visible })
    Invoke-Button (Wait-Until "visible rename menu item ($Stage)" {
        $item = Find-Element $rename $menuItemType -Visible
        if ($item -and $item.Current.IsEnabled) { $item }
    })
    try {
        return (Wait-Until "tab name field ($Stage)" { Find-Element 'Tab name' $editType -Visible }).GetCurrentPattern($valuePattern)
    } catch {
        $focus = [System.Windows.Automation.AutomationElement]::FocusedElement
        $ownedFocus = $focus -and $focus.Current.ProcessId -eq $app.Id
        $menuOpen = $null -ne (Find-Element $rename $menuItemType -Visible)
        throw "$_ Foreground HWND: $([FixtureMessages]::GetForegroundWindow()), fixture HWND: $window. Focus in fixture: $ownedFocus. Rename menu open: $menuOpen."
    }
}

function Wait-Title([string]$Expected) {
    $null = Wait-Until "window and tab named $Expected" {
        $app.Refresh()
        $app.MainWindowTitle.EndsWith(" $Expected") -and (Find-Element $Expected $tabType)
    }
}

function Wait-TitleEditorClosed([uint64]$Pane) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $document = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, "w11term-pane-$Pane")
    # WinUI hides the dialog before Closed reaches the app. Reopen the tab menu
    # after the app has restored its terminal input and accessibility surface.
    $null = Wait-Until "terminal document for pane $Pane after rename closes" {
        $entry = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $document)
        $entry -and $entry.Current.ControlType -eq [System.Windows.Automation.ControlType]::Document -and
            !(Find-Element 'Tab name' $editType)
    }
}

Wait-Title 'Build console'
$before = (Control @('list')).sessions
$activePane = ($before | Where-Object active).id
$field = Open-TitleEditor 'cancel'
if ($field.Current.Value -ne 'Build console') { throw 'The dialog did not load the saved title' }
$field.SetValue('Ignore this edit')
Invoke-Button (Wait-Until 'cancel rename' { Find-Element 'Cancel' })
Wait-TitleEditorClosed $activePane
Wait-Title 'Build console'
$field = Open-TitleEditor 'save'
$field.SetValue(('x' * 129))
$null = Wait-Until 'invalid title disabled Save' {
    $button = Find-Element 'Save'
    $button -and -not $button.Current.IsEnabled
}
$field.SetValue('Deploy console')
Invoke-Button (Wait-Until 'valid title enabled Save' {
    $button = Find-Element 'Save'
    if ($button -and $button.Current.IsEnabled) { $button }
})
Wait-TitleEditorClosed $activePane
Wait-Title 'Deploy console'
$after = (Control @('list')).sessions
foreach ($original in $before) {
    $same = @($after | Where-Object { $_.id -eq $original.id })
    if ($same.Count -ne 1 -or $same[0].label -ne $original.label -or
        $same[0].title -ne $original.title -or $same[0].tab_id -ne $original.tab_id) {
        throw "Rename changed the identity or terminal title of pane $($original.id)"
    }
    if ($original.visible -and $same[0].tab_title -ne 'Deploy console') { throw 'Split tab title missing' }
    if (-not $original.visible -and $same[0].tab_title) { throw 'Unrelated tab was renamed' }
}
$null = Open-TitleEditor 'reset'
Invoke-Button (Wait-Until 'reset tab title' { Find-Element 'Use default' })
Wait-TitleEditorClosed $activePane
Wait-Title (($before | Where-Object active).label)
if (@((Control @('list')).sessions | Where-Object tab_title).Count) { throw 'Custom title survived reset' }

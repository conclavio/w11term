# Exercise the native dialog in the mixed, named and unnamed workspace fixture.
[CmdletBinding()]
param([Parameter(Mandatory)][int]$ProcessId, [Parameter(Mandatory)][string]$Cli)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')

$app = Get-Process -Id $ProcessId
$window = $app.MainWindowHandle
$tabType = [System.Windows.Automation.ControlType]::TabItem
$valuePattern = [System.Windows.Automation.ValuePattern]::Pattern

function Open-TitleEditor {
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    Invoke-Button (Wait-Until 'rename tab' { Find-Element ('Rename tab' + [char]0x2026) })
    return (Wait-Until 'tab name field' { Find-Element 'Tab name' }).GetCurrentPattern($valuePattern)
}

function Wait-Title([string]$Expected) {
    $null = Wait-Until "window and tab named $Expected" {
        $app.Refresh()
        $app.MainWindowTitle.EndsWith(" $Expected") -and (Find-Element $Expected $tabType)
    }
}

Wait-Title 'Build console'
$before = (Control @('list')).sessions
$field = Open-TitleEditor
if ($field.Current.Value -ne 'Build console') { throw 'The dialog did not load the saved title' }
$field.SetValue('Ignore this edit')
Invoke-Button (Wait-Until 'cancel rename' { Find-Element 'Cancel' })
$null = Wait-Until 'rename cancelled' { -not (Find-Element 'Tab name') }
Wait-Title 'Build console'
$field = Open-TitleEditor
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
$null = Open-TitleEditor
Invoke-Button (Wait-Until 'reset tab title' { Find-Element 'Use default' })
Wait-Title (($before | Where-Object active).label)
if (@((Control @('list')).sessions | Where-Object tab_title).Count) { throw 'Custom title survived reset' }

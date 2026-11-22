[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$direction = [IO.File]::ReadAllText((Join-Path $State 'direction'))
$right = 'Split right in current directory'
$below = 'Split below in current directory'
$button = [System.Windows.Automation.ControlType]::Button
$menuItem = [System.Windows.Automation.ControlType]::MenuItem

function Check-Actions([bool]$Enabled) {
    $field = Open-Palette 'current directory'
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('current directory')
    $null = Wait-Until "both directory split actions enabled=$Enabled" {
        $a = Find-Element $right $button
        $b = Find-Element $below $button
        $a -and $b -and $a.Current.IsEnabled -eq $Enabled -and $b.Current.IsEnabled -eq $Enabled
    }
    Press-Key 27
    $null = Wait-Until 'palette dismissed' { !(Find-Element 'Search commands' ([System.Windows.Automation.ControlType]::Edit)) }
}
function Open-PanesMenu {
    Invoke-Button (Wait-Until 'caret menu' { Find-Element 'Tab menu' $button })
    $panes = Wait-Until 'Panes submenu' { Find-Element 'Panes' $menuItem }
    $panes.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
}
function Send-To([int]$Pane, [string]$Text, [string]$Reply) {
    $null = Control @('send-text','--pane',"$Pane",'--no-paste',$Text)
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','15',$Reply)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','128','--rows','32') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $source = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$source",'--timeout','15','ready')
    Check-Actions $false
    Send-To $source 'report-current' 'current-reported'
    Check-Actions $true
    Send-To $source 'report-foreign' 'foreign-reported'
    Check-Actions $false
    if (@((Control @('list')).sessions).Count -ne 1) { throw 'Missing or foreign CWD created a session' }
    Send-To $source 'report-current-again' 'current-confirmed'
    Check-Actions $true
    $original = (Control @('list')).sessions | Where-Object id -eq $source
    if ($direction -eq 'right') {
        Run-Palette $right
    } else {
        Open-PanesMenu
        Invoke-Button (Wait-Until 'enabled Split below in current directory menu command' {
            $item = Find-Element $below $menuItem
            if ($item -and $item.Current.IsEnabled) { $item }
        })
    }
    $child = Wait-Until "$direction split active beside the original shell" {
        $sessions = @((Control @('list')).sessions)
        $a = $sessions | Where-Object id -eq $source
        $b = $sessions | Where-Object id -ne $source
        if ($sessions.Count -ne 2 -or !$a.visible -or !$b.visible -or !$b.active -or
            $b.tab_id -ne $original.tab_id -or $a.connection_generation -ne $original.connection_generation) { return }
        if ($direction -eq 'right') {
            if ($a.cols -lt 0.8 * $original.cols -and $b.cols -lt 0.8 * $original.cols -and [Math]::Abs($a.rows - $b.rows) -le 1) { $b.id }
        } elseif ($a.rows -lt 0.8 * $original.rows -and $b.rows -lt 0.8 * $original.rows -and [Math]::Abs($a.cols - $b.cols) -le 1) { $b.id }
    }
    $null = Control @('wait-for-text','--pane',"$child",'--timeout','15','workspace-command-started')
    Send-To $source 'parent-live' 'parent-live'
    Wait-Focus $child
    foreach ($c in 'child-live'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$child",'--timeout','15','child-live')
    $originalScreen = (Control @('get-text','--pane',"$source")).text
    $childScreen = (Control @('get-text','--pane',"$child")).text
    if ($originalScreen.Contains('child-live') -or $originalScreen.Contains('workspace-command-started') -or
        $childScreen.Contains('parent-live') -or $childScreen.Contains('current-reported')) { throw 'The two SSH channels mixed output or input' }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

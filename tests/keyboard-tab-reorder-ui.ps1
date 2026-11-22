param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$keymap = @{ shortcuts = @{
    command_palette='F7'; focus_left='F4'; move_tab_left='F6'; move_tab_right='F10'
    select_tab_1='F8'; select_tab_2='F9'
} }
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, ($keymap | ConvertTo-Json))
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$menu = [System.Windows.Automation.ControlType]::MenuItem
$tabItem = [System.Windows.Automation.ControlType]::TabItem
$left = 'Move tab left    F6'
$right = 'Move tab right    F10'

function Type-Text([string]$Text) {
    foreach ($ch in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
}
function Wait-Strip([string]$First, [string]$Second, [string]$Selected) {
    $null = Wait-Until "tab strip [$First, $Second] with '$Selected' selected" {
        $a = Find-Element $First $tabItem
        $b = Find-Element $Second $tabItem
        if (!$a -or !$b) { return }
        $aRect = $a.Current.BoundingRectangle
        $bRect = $b.Current.BoundingRectangle
        $selection = [System.Windows.Automation.SelectionItemPattern]::Pattern
        $aRect.Width -gt 0 -and $bRect.Width -gt 0 -and $aRect.Left -lt $bRect.Left -and
            ($a.GetCurrentPattern($selection).Current.IsSelected -eq ($First -ceq $Selected)) -and
            ($b.GetCurrentPattern($selection).Current.IsSelected -eq ($Second -ceq $Selected))
    }
}
function Shell-Witness([string]$Stage, [switch]$Initialize) {
    $setup = if ($Initialize) { '$global:ReorderState="preserved"; ' } else { '' }
    Type-Text ($setup + "'$Stage/' + " + '$PID + "/" + $global:ReorderState')
    Press-Key 13
    $pattern = "$Stage/([0-9]+)/preserved"
    $reply = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','--regex',$pattern)
    return [regex]::Match($reply.text, $pattern).Groups[1].Value
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','120','--rows','28') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $ssh = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','ready')
    $origin = (Control @('list')).sessions | Where-Object id -eq $ssh
    Type-Text 'before;'
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','12','before;')
    Run-Palette 'Split right with Palette PowerShell'
    $localPane = Wait-Until 'focused PowerShell sharing the SSH tab' {
        $p = (Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell'
        if ($p.active -and $p.tab_id -eq $origin.tab_id) { $p.id }
    }
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','PS ')
    $shellPid = Shell-Witness 'initial' -Initialize
    Run-Palette 'Settings'
    Wait-Strip 'Palette PowerShell' 'Settings' 'Settings'

    Press-Key 0x75 # F6 moves Settings left
    Wait-Strip 'Settings' 'Palette PowerShell' 'Settings'
    Press-Key 0x78 # F9 selects the new second tab
    Wait-Focus $localPane
    Press-Key 0x75
    Wait-Strip 'Palette PowerShell' 'Settings' 'Palette PowerShell'
    Wait-Focus $localPane
    if ((Shell-Witness 'moved') -cne $shellPid) { throw 'Reordering replaced the PowerShell process' }
    $field = Open-Palette 'Move tab left'
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('Move tab left')
    $null = Wait-Until 'left edge action disabled in the palette' {
        $row = Find-Element $left $button
        $row -and !$row.Current.IsEnabled
    }
    Press-Key 27
    $null = Wait-Until 'palette closed' { !(Find-Element 'Search commands') }

    Press-Key 0x73 # F4 focuses the SSH half of the same group
    Wait-Focus $ssh
    Press-Key 0x79 # F10 moves the tab right
    Wait-Strip 'Settings' 'palette-ssh' 'palette-ssh'
    Wait-Focus $ssh
    Type-Text 'after-right;'
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','12','after-right;')
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' $button })
    $null = Wait-Until 'right edge action disabled in the native menu' {
        $row = Find-Element $right $menu
        $row -and !$row.Current.IsEnabled
    }
    Invoke-Button (Wait-Until 'enabled Move tab left menu item' {
        $row = Find-Element $left $menu
        if ($row -and $row.Current.IsEnabled) { $row }
    })
    Wait-Strip 'palette-ssh' 'Settings' 'palette-ssh'
    Press-Key 0x78
    Wait-Strip 'palette-ssh' 'Settings' 'Settings'
    Press-Key 0x77 # F8 selects the SSH tab in its final position
    Wait-Focus $ssh
    Type-Text 'after-select'
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','12','after-select')
    $panes = @((Control @('list')).sessions)
    $remote = $panes | Where-Object id -eq $ssh
    $local = $panes | Where-Object id -eq $localPane
    if ($panes.Count -ne 2 -or !$remote.active -or !$local.visible -or
        $remote.tab_id -ne $origin.tab_id -or $local.tab_id -ne $origin.tab_id -or
        $remote.connection_generation -ne $origin.connection_generation) { throw 'The original SSH/PowerShell split changed' }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

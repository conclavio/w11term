param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","focus_previous_pane":"F6","close_pane":"F9"}}')
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$menu = [System.Windows.Automation.ControlType]::MenuItem
$label = 'Focus previous pane    F6'

function Type-Text([string]$Text) {
    foreach ($ch in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
}
function Shell-Witness([int]$Pane, [string]$Role, [string]$Stage, [switch]$Initialize) {
    Wait-Focus $Pane
    # The output marker includes the variable and PID from that live shell.
    $command = if ($Initialize) { '$global:PaneRole="' + $Role + '"; ' } else { '' }
    Type-Text ($command + "'" + $Stage + "-' + " + '$global:PaneRole + "-" + $PID')
    Press-Key 13
    $pattern = "$Stage-$Role-([0-9]+)"
    $frame = Control @('wait-for-text','--pane',"$Pane",'--timeout','15','--regex',$pattern)
    return [regex]::Match($frame.text, $pattern).Groups[1].Value
}
function Previous([int]$Expected) {
    Press-Key 0x75 # F6
    Wait-Focus $Expected
}
function No-Previous {
    $field = Open-Palette 'Focus previous pane'
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('Focus previous pane')
    $null = Wait-Until 'previous pane disabled before visiting another terminal' {
        $row = Find-Element $label $button
        $row -and !$row.Current.IsEnabled
    }
    Press-Key 27
    $null = Wait-Until 'command palette closed' { !(Find-Element 'Search commands') }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','110','--rows','24') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $ssh = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','ready')
    $origin = ((Control @('list')).sessions | Where-Object id -eq $ssh)
    No-Previous

    Run-Palette 'Split right with Palette PowerShell'
    $split = Wait-Until 'PowerShell sharing the SSH tab' {
        $p = (Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell'
        if ($p.active -and $p.tab_id -eq $origin.tab_id) { $p.id }
    }
    $null = Control @('wait-for-text','--pane',"$split",'--timeout','15','PS ')
    Previous $ssh
    Type-Text 'ssh-split;'
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','10','ssh-split;')
    Run-Palette $label
    $splitProcess = Shell-Witness $split 'split' 'initial' -Initialize

    $tab = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    $null = Control @('wait-for-text','--pane',"$tab",'--timeout','15','PS ')
    $tabProcess = Shell-Witness $tab 'tab' 'initial' -Initialize
    $other = ((Control @('list')).sessions | Where-Object id -eq $tab)
    if ($other.tab_id -eq $origin.tab_id -or $tabProcess -eq $splitProcess) { throw 'Expected two distinct PowerShell processes in different tabs' }
    Previous $split
    if ((Shell-Witness $split 'split' 'again') -ne $splitProcess) { throw 'Split PowerShell process changed' }
    Previous $tab
    if ((Shell-Witness $tab 'tab' 'again') -ne $tabProcess) { throw 'Separate PowerShell process changed' }

    Invoke-Button (Wait-Until 'Tab menu' { Find-Element 'Tab menu' $button })
    Invoke-Button (Wait-Until 'Settings menu item' { Find-Element 'Settings' $menu })
    $null = Wait-Until 'Settings outside the terminal tabs' { Find-Element 'Edit profile palette-ssh' $button }
    Previous $tab
    Previous $split

    # Closing the active split promotes its SSH neighbor. Its previous live
    # terminal was the PowerShell in the other tab after removing this split.
    Press-Key 0x78 # F9
    $null = Wait-Until 'closed split removed from the process registry' {
        $panes = @((Control @('list')).sessions)
        $panes.Count -eq 2 -and !($panes | Where-Object id -eq $split)
    }
    Wait-Focus $ssh
    Previous $tab
    if ((Shell-Witness $tab 'tab' 'survived') -ne $tabProcess) { throw 'Remaining PowerShell process changed' }
    Previous $ssh
    Type-Text 'ssh-restored'
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','10','ssh-restored')
    $current = ((Control @('list')).sessions | Where-Object id -eq $ssh)
    if ($current.tab_id -ne $origin.tab_id -or $current.connection_generation -ne $origin.connection_generation) {
        throw 'Returning to SSH replaced the original tab or connection'
    }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

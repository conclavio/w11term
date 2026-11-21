[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'

function Run-Palette([string]$Command) {
    Press-Key 0x76 # F7 in the fixture's keymap
    $field = Wait-Until 'command search focus' {
        $entry = Find-Element 'Search commands'
        if ($entry -and $entry.Current.HasKeyboardFocus) { $entry }
    }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Command)
    Invoke-Button (Wait-Until $Command { Find-Element $Command ([System.Windows.Automation.ControlType]::Button) })
    $null = Wait-Until 'picker to close' { !(Find-Element 'Search commands') }
}

function Assert-Locked([int]$Pane) {
    $null = Wait-Until "read-only flag for pane $Pane" { ((Control @('list')).sessions | Where-Object id -eq $Pane).read_only }
    $result = & $Cli --pid $app.Id send-text --pane $Pane --no-paste 'unexpected' | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $result.ok -or $result.error -notmatch 'read-only') { throw 'Expected a rejected raw write' }
    Post-Key 0x102 ([uint32][char]'X')
    Press-Key 0x27 # ordinary tty cursor key
    Press-Key 13
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-Until 'window handle' { $app.Refresh(); if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle } }
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $remote = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    Run-Palette 'Enable read-only pane input'
    Assert-Locked $remote
    Run-Palette 'Find in scrollback    Ctrl+Shift+F'
    $find = Wait-Until 'find remains available' { Find-Element 'Find in scrollback' ([System.Windows.Automation.ControlType]::Edit) }
    $find.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('ready')
    Press-Key 0x1b
    $null = Wait-Until 'find to close' { !(Find-Element 'Find in scrollback' ([System.Windows.Automation.ControlType]::Edit)) }
    Press-Key 0x78 # keyboard copy mode
    $dot = ' ' + [char]0xb7 + ' '
    $null = Wait-Until 'keyboard copy mode' { Find-Element "Copy mode${dot}Arrows or hjkl move${dot}v select${dot}Shift+V select lines${dot}Esc exit" }
    Press-Key 0x47
    Press-Key 0x24
    Press-Key 0x56
    Press-Key 0x27
    $null = Wait-Until 'selection is exposed through UI Automation' {
        $doc = Find-Element ("Terminal${dot}palette-ssh")
        if ($doc) {
            $ranges = @($doc.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).GetSelection())
            $ranges.Count -eq 1 -and $ranges[0].GetText(-1).Length -eq 2
        }
    }
    Press-Key 0x1b
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'PowerShell neighbor' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $local
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    $panes = (Control @('list')).sessions
    if ($panes.Count -ne 2 -or $panes[0].tab_id -ne $panes[1].tab_id -or ($panes | Where-Object id -eq $local).read_only) {
        throw 'New split inherited the input lock or left the source tab'
    }
    foreach ($c in "'source-locked-' + (20+2)".ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','10','source-locked-22')
    Run-Palette 'Enable read-only pane input'
    Assert-Locked $local
    Run-Palette 'Disable read-only pane input'
    foreach ($c in "'resumed-local-' + (30+3)".ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','10','resumed-local-33')
    if ((Control @('get-text','--pane',"$local")).text -match 'unexpected|CommandNotFoundException') { throw 'Blocked input reached PowerShell' }
    Picker-Query 'palette-ssh'
    $null = Wait-Until 'read-only state in pane picker' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) |
            Where-Object { $_.Current.Name -like '*Read-only*Tab 1 / pane 1*' } | Select-Object -First 1
    }
    Press-Key 13
    Wait-Focus $remote
    Assert-Locked $remote
    Run-Palette 'Disable read-only pane input'
    foreach ($c in 'resumed'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','resumed')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

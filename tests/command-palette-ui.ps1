[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'

function Search-Command($Field, [string]$Query, [string]$Expected) {
    $Field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Query)
    return Wait-Until "command result $Expected" {
        Find-Element $Expected ([System.Windows.Automation.ControlType]::Button)
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    $field = Open-Palette 'Move pane to existing tab'
    $disabled = Search-Command $field 'move existing' ('Move pane to existing tab' + [char]0x2026)
    if ($disabled.Current.IsEnabled) { throw 'Move needs a second terminal tab' }
    Press-Key 13
    if (!(Find-Element 'Search commands')) { throw 'Enter executed a disabled command' }
    # These are addressed to the fixture HWND. They must never reach the SSH channel.
    Post-Key 0x102 ([uint32][char]'z')
    Press-Key 0x28
    Press-Key 0x1b
    $null = Wait-Until 'palette to close' { !(Find-Element 'Search commands') }
    Wait-Focus $remote
    foreach ($c in 'af'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','af')

    # Use the caret entry, then invoke a profile result as a mouse or assistive client would.
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    Invoke-Button (Wait-Until 'palette menu binding' { Find-Element ('Command palette' + [char]0x2026 + '    F7') })
    $field = Wait-Until 'palette from caret' { Find-Element 'Search commands' }
    $button = Search-Command $field 'new palette powershell' ('New tab with Palette PowerShell ' + [char]0xb7 + ' Local')
    Invoke-Button $button
    $local = Wait-Until 'new PowerShell pane' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $local
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    foreach ($c in "'palette-local-' + (40+2)".ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','10','palette-local-42')

    $field = Open-Palette 'Rename tab'
    $null = Search-Command $field 'rename tab' ('Rename tab' + [char]0x2026)
    Press-Key 13
    $null = Wait-Until 'tab name dialog' { Find-Element 'Tab name' }
    Invoke-Button (Wait-Until 'cancel rename' { Find-Element 'Cancel' ([System.Windows.Automation.ControlType]::Button) })
    $null = Wait-Until 'tab name dialog to close' { !(Find-Element 'Tab name') }
    $field = Open-Palette 'Settings'
    $null = Search-Command $field 'settings' 'Settings'
    Press-Key 13
    $null = Wait-Until 'settings utility tab' { Find-Element 'Keyboard' }
    $field = Open-Palette 'Copy selection'
    $disabled = Search-Command $field 'copy selection' 'Copy selection    Ctrl+Shift+C'
    if ($disabled.Current.IsEnabled) { throw 'Settings has no terminal selection' }
    $null = Search-Command $field 'F8' 'Select tab 1    F8'
    Press-Key 13
    Wait-Focus $remote
    foreach ($c in 'ter'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','after')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

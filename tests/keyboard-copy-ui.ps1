# Drive only this fixture's HWND. Real clipboard writes are for the disposable CI desktop.
[CmdletBinding()]
param([string]$Gui, [string]$Cli, [string]$State, [string]$Trace)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$verifyClipboard = $env:GITHUB_ACTIONS -eq 'true'
if ($verifyClipboard) { . (Join-Path $PSScriptRoot 'support/clipboard.ps1') }
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$dot = ' ' + [char]0xb7 + ' '
$normal = "Copy mode${dot}Arrows or hjkl move${dot}v select${dot}Shift+V select lines${dot}Esc exit"
$character = "Copy mode${dot}Character selection${dot}y or Enter copy${dot}Esc cancel"
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'copy-fixture') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-Until 'rendered SSH window' { $app.Refresh(); if ($app.MainWindowTitle -like 'w11term*copy-fixture*') { $app.MainWindowHandle } }
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $pane = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','history-end')
    $null = Wait-Until 'measured canvas' {
        $box = Control @('get-text','--pane',"$pane")
        $box.cols -ne 80 -or $box.rows -ne 24
    }
    Press-Key 0x77 # F8, remapped in this profile's settings
    $null = Wait-Until 'copy mode' { Find-Element $normal }
    Press-Key 0x21 # PageUp
    Press-Key 0x47 # g, oldest retained row
    Press-Key 0x24 # Home
    Press-Key 0x56 # v
    $null = Wait-Until 'character selection in scrollback' { Find-Element $character }
    Press-Key 0x4c # l
    Press-Key 0x27 # Right
    Post-Key 0x102 0x71 # unrelated WM_CHAR stays local
    $null = Wait-Until 'three exact cells selected in the oldest scrollback row' {
        $document = Find-Element ("Terminal${dot}copy-fixture")
        if ($document) {
            $pattern = $document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
            $ranges = @($pattern.GetSelection())
            $ranges.Count -eq 1 -and $ranges[0].GetText(-1) -ceq 'his'
        }
    }
    if ($verifyClipboard) {
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Press-Key 0x59 # y
        $null = Wait-Until 'copy of first three history cells' { [FixtureClipboard]::ContainsNewText($before, 'his') }
        $null = Wait-Until 'return from copy' { -not (Find-Element $character) }
        Press-Key 0x77
        $null = Wait-Until 'second copy mode' { Find-Element $normal }
    }
    Press-Key 0x1b
    $null = Wait-Until 'return from Escape' { -not (Find-Element $normal) -and -not (Find-Element $character) }
    Post-Key 0x102 0x21 # explicit WM_CHAR proves that normal shell input resumed
    $null = Wait-Until 'SSH peer received following input' { (Get-Item -LiteralPath $Trace).Length -gt 0 }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}

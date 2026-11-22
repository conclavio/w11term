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
function Wait-Selection([string]$Expected) {
    $null = Wait-Until "exact keyboard selection '$Expected'" {
        $document = Find-Element ("Terminal${dot}copy-fixture")
        if ($document) {
            $pattern = $document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
            $ranges = @($pattern.GetSelection())
            $ranges.Count -eq 1 -and $ranges[0].GetText(-1) -ceq $Expected
        }
    }
}
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'copy-fixture') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-Until 'rendered SSH window' { $app.Refresh(); if ($app.MainWindowTitle -like 'w11term*copy-fixture*') { $app.MainWindowHandle } }
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $pane = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','history-end')
    $null = Wait-Until 'measured canvas with the path outside the live screen' {
        $box = Control @('get-text','--pane',"$pane")
        ($box.cols -ne 80 -or $box.rows -ne 24) -and !$box.text.Contains('/srv/game/zone')
    }
    Press-Key 0x77 # F8, remapped in this profile's settings
    $null = Wait-Until 'copy mode' { Copy-ModeOpen }
    Press-Key 0x21 # PageUp
    Press-Key 0x47 # g, oldest retained row
    Press-Key 0x24 # Home
    Press-Key 0x56 # v
    Press-Key 0x4c # l
    Press-Key 0x27 # Right
    Post-Key 0x102 0x71 # unrelated WM_CHAR stays local
    Wait-Selection 'his'
    if ($verifyClipboard) {
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Press-Key 0x59 # y
        $null = Wait-Until 'copy of first three history cells' { [FixtureClipboard]::ContainsNewText($before, 'his') }
    } else {
        Press-Key 0x1b
    }
    $null = Wait-Until 'first copy mode closed' { !(Copy-ModeOpen) }

    Press-Key 0x77
    $null = Wait-Until 'copy mode for word navigation' { Copy-ModeOpen }
    # Post a burst: jump to retained output, pass the path, step back and
    # select through its final cell. Only ':' is configured as punctuation.
    foreach ($key in 0x47,0x24,0x57,0x57,0x42,0x56,0x45) { Press-Key $key } # g Home w w b v e
    Wait-Selection '/srv/game/zone'
    if ($verifyClipboard) {
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Press-Key 0x59
        $null = Wait-Until 'custom-delimited path on the clipboard' {
            [FixtureClipboard]::ContainsNewText($before, '/srv/game/zone')
        }
        $null = Wait-Until 'word selection copied and closed' { !(Copy-ModeOpen) }
        Press-Key 0x77
        $null = Wait-Until 'copy mode before Escape' { Copy-ModeOpen }
    }
    Press-Key 0x1b
    $null = Wait-Until 'return from Escape' { !(Copy-ModeOpen) }
    Post-Key 0x102 0x21 # explicit WM_CHAR proves that normal shell input resumed
    $null = Wait-Until 'SSH peer received following input' { (Get-Item -LiteralPath $Trace).Length -gt 0 }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
